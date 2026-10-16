//! The binding's outbound path can drop a request without an HTTP answer.

use contextful_d1::D1;
use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy)]
enum Answer {
    Ok,
    Drop,
}

/// A binding stand-in answering requests in arrival order and logging each one as
/// `(connection, request line)`; a dropped request closes its connection unanswered.
struct Binding {
    endpoint: String,
    log: Arc<Mutex<Vec<(usize, String)>>>,
}

impl Binding {
    fn start(script: Vec<Answer>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let log = Arc::new(Mutex::new(Vec::new()));
        let script = Arc::new(Mutex::new(script.into_iter()));
        let shared = log.clone();
        std::thread::spawn(move || {
            for (connection, stream) in listener.incoming().enumerate() {
                let (log, script) = (shared.clone(), script.clone());
                std::thread::spawn(move || serve(connection, stream.unwrap(), &log, &script));
            }
        });
        Self { endpoint, log }
    }

    fn log(&self) -> Vec<(usize, String)> {
        self.log.lock().unwrap().clone()
    }
}

fn serve(
    connection: usize,
    stream: TcpStream,
    log: &Mutex<Vec<(usize, String)>>,
    script: &Mutex<std::vec::IntoIter<Answer>>,
) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut length = 0;
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            if header == "\r\n" {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap();
                }
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        log.lock().unwrap().push((connection, line.trim().to_string()));
        match script.lock().unwrap().next().unwrap_or(Answer::Ok) {
            Answer::Drop => return,
            Answer::Ok => {
                let body = br#"{"rows":[{"ready":1}],"changes":0}"#;
                write!(writer, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n", body.len()).unwrap();
                writer.write_all(body).unwrap();
            }
        }
    }
}

#[test]
fn a_read_dropped_on_a_reused_connection_resends_on_a_fresh_one() {
    let binding = Binding::start(vec![Answer::Ok, Answer::Drop]);
    let db = D1::connect(&binding.endpoint).unwrap();
    db.query("SELECT 1 AS ready", vec![]).unwrap();
    let read = db.query("SELECT 1 AS ready", vec![]).unwrap();
    assert_eq!(read.rows, vec![json!({"ready": 1})]);
    let log = binding.log();
    assert_eq!(log.len(), 3, "{log:?}");
    assert_eq!(log[1].0, log[0].0, "the second read reused the pooled connection");
    assert_ne!(log[2].0, log[1].0, "the resend opened a fresh connection");
}

#[test]
fn a_read_dropped_on_every_attempt_stops_after_its_bound() {
    let binding = Binding::start(vec![Answer::Drop; 8]);
    let db = D1::connect(&binding.endpoint).unwrap();
    assert_eq!(db.query("SELECT 1 AS ready", vec![]).unwrap_err(), "StorageBindingUnavailable");
    assert_eq!(binding.log().len(), 3);
}

#[test]
fn a_write_never_resends_and_never_rides_a_pooled_connection() {
    let binding = Binding::start(vec![Answer::Ok, Answer::Drop]);
    let db = D1::connect(&binding.endpoint).unwrap();
    db.query("SELECT 1 AS ready", vec![]).unwrap();
    let write = db.query("UPDATE contextful_lease SET holder = NULL WHERE key = ?", vec![json!("cadence")]);
    assert_eq!(write.unwrap_err(), "StorageBindingUnavailable");
    let log = binding.log();
    assert_eq!(log.len(), 2, "a write whose answer is lost is sent once: {log:?}");
    assert_ne!(log[1].0, log[0].0, "the write opened a fresh connection");
}

#[test]
fn a_blob_request_resends_after_a_dropped_answer() {
    let binding = Binding::start(vec![Answer::Drop]);
    let db = D1::connect(&binding.endpoint).unwrap();
    db.health().unwrap();
    assert_eq!(binding.log().len(), 2);
}
