//! A loopback SQS queue speaking the JSON protocol: `SendMessage`, `ReceiveMessage` and
//! `DeleteMessage` over one queue, with visibility timeouts held by the test rather than
//! the clock. It records every request, signed headers included, and checks none.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

/// One request the queue received.
#[derive(Debug, Clone)]
pub struct SqsRequest {
    /// The action of `X-Amz-Target`, `AmazonSQS.` stripped.
    pub action: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

impl SqsRequest {
    /// The first header of `name`, compared case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

#[derive(Default)]
struct Held {
    next: u64,
    /// Each message: its id, body, and the receipt of its current receive while hidden.
    messages: Vec<(u64, String, Option<String>)>,
    requests: Vec<SqsRequest>,
}

/// A running loopback queue, served on a background thread for the test's life.
pub struct SqsServer {
    pub endpoint: String,
    held: Arc<Mutex<Held>>,
}

impl SqsServer {
    pub fn start() -> SqsServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binding the loopback SQS queue");
        let endpoint = format!("http://127.0.0.1:{}", listener.local_addr().expect("the queue's address").port());
        let held: Arc<Mutex<Held>> = Arc::default();
        let serving = held.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let held = serving.clone();
                std::thread::spawn(move || {
                    let _ = serve(stream, &held);
                });
            }
        });
        SqsServer { endpoint, held }
    }

    /// The queue URL a client names in each request.
    pub fn queue_url(&self) -> String {
        format!("{}/000000000000/contextful", self.endpoint)
    }

    /// Queue `body` as a producer outside the engine would, an S3 notification among them.
    pub fn seed(&self, body: &str) {
        let mut held = self.held.lock().unwrap();
        held.next += 1;
        let id = held.next;
        held.messages.push((id, body.to_string(), None));
    }

    /// The bodies no consumer has deleted, hidden or not, oldest first.
    pub fn pending(&self) -> Vec<String> {
        self.held.lock().unwrap().messages.iter().map(|(_, b, _)| b.clone()).collect()
    }

    /// End every message's visibility timeout, as the clock passing it would.
    pub fn expire_visibility(&self) {
        for m in self.held.lock().unwrap().messages.iter_mut() {
            m.2 = None;
        }
    }

    /// Every request received for `action`.
    pub fn requests(&self, action: &str) -> Vec<SqsRequest> {
        self.held.lock().unwrap().requests.iter().filter(|r| r.action == action).cloned().collect()
    }
}

fn answer(held: &Mutex<Held>, request: SqsRequest) -> (u16, Value) {
    let mut held = held.lock().unwrap();
    let body = request.body.clone();
    let action = request.action.clone();
    held.requests.push(request);
    match action.as_str() {
        "SendMessage" => {
            held.next += 1;
            let id = held.next;
            let text = body["MessageBody"].as_str().unwrap_or_default().to_string();
            held.messages.push((id, text, None));
            (200, json!({ "MessageId": format!("m-{id}"), "MD5OfMessageBody": "" }))
        }
        "ReceiveMessage" => {
            let max = body["MaxNumberOfMessages"].as_u64().unwrap_or(1).clamp(1, 10) as usize;
            let mut out = Vec::new();
            let next = held.next;
            for (n, m) in held.messages.iter_mut().filter(|m| m.2.is_none()).take(max).enumerate() {
                let receipt = format!("r-{}-{}-{n}", m.0, next);
                m.2 = Some(receipt.clone());
                out.push(json!({ "MessageId": format!("m-{}", m.0), "ReceiptHandle": receipt, "Body": m.1 }));
            }
            held.next += 1;
            match out.is_empty() {
                true => (200, json!({})),
                false => (200, json!({ "Messages": out })),
            }
        }
        "DeleteMessage" => {
            let receipt = body["ReceiptHandle"].as_str().unwrap_or_default();
            let before = held.messages.len();
            held.messages.retain(|m| m.2.as_deref() != Some(receipt));
            match held.messages.len() < before {
                true => (200, json!({})),
                false => (400, json!({ "__type": "com.amazonaws.sqs#ReceiptHandleIsInvalid", "message": "The receipt handle is not valid." })),
            }
        }
        other => (400, json!({ "__type": "com.amazonaws.sqs#InvalidAction", "message": format!("no action `{other}`") })),
    }
}

fn serve(stream: TcpStream, held: &Mutex<Held>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        reader.read_line(&mut h)?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let len = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    let mut body = vec![0; len];
    reader.read_exact(&mut body)?;
    let action = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("x-amz-target")).map(|(_, v)| v.trim_start_matches("AmazonSQS.").to_string()).unwrap_or_default();
    let request = SqsRequest { action, headers, body: serde_json::from_slice(&body).unwrap_or_default() };
    let (status, reply) = answer(held, request);
    let text = reply.to_string();
    let mut stream = stream;
    write!(stream, "HTTP/1.1 {status} OK\r\nContent-Type: application/x-amz-json-1.0\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len())?;
    stream.flush()
}
