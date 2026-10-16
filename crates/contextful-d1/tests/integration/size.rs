//! A binding answer carries no size cap of its own.

use super::Fixture;
use contextful_d1::D1;
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

const OVER_TEN_MIB: usize = 10_979_107;

#[test]
fn a_blob_over_ten_mebibytes_reads_back_whole() {
    let fixture = Fixture::start();
    let db = D1::connect(&fixture.endpoint).unwrap();
    let bytes: Vec<u8> = (0..OVER_TEN_MIB).map(|i| (i % 251) as u8).collect();
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    db.put_blob(&sha256, &bytes).unwrap();
    assert_eq!(db.get_blob(&sha256).unwrap(), Some(bytes));
}

#[test]
fn a_query_answer_over_ten_mebibytes_reads_back_whole() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let text = "x".repeat(OVER_TEN_MIB);
    let body = format!(r#"{{"rows":[{{"row":"{text}"}}],"changes":0}}"#);
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut length = 0;
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            if header == "\r\n" {
                break;
            }
            if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
        }
        reader.read_exact(&mut vec![0; length]).unwrap();
        let mut writer = stream;
        write!(writer, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n", body.len()).unwrap();
        writer.write_all(body.as_bytes()).unwrap();
    });
    let db = D1::connect(&endpoint).unwrap();
    let answer = db.query("SELECT row FROM contextful_run", vec![]).unwrap();
    assert_eq!(answer.rows[0]["row"].as_str().map(str::len), Some(OVER_TEN_MIB));
}
