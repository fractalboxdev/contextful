//! Gzip decompression ahead of a decoder, under a ceiling that refuses rather than truncates.

use contextful_decode::gunzip;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::io::Write;

fn gz(body: &[u8]) -> Vec<u8> {
    let mut e = GzEncoder::new(Vec::new(), Compression::fast());
    e.write_all(body).unwrap();
    e.finish().unwrap()
}

#[test]
fn a_gzip_body_decompresses_to_its_bytes() {
    let body = b"{\"id\":1}\n{\"id\":2}\n";
    assert_eq!(gunzip(&gz(body), 1 << 20, "s3://b/k.jsonl.gz").unwrap(), body);
}

#[test]
fn a_body_expanding_past_the_ceiling_refuses_naming_the_input() {
    let body = vec![b'a'; 4096];
    // Exactly at the ceiling decompresses whole.
    assert_eq!(gunzip(&gz(&body), 4096, "at-ceiling").unwrap().len(), 4096);
    let f = gunzip(&gz(&body), 4095, "s3://b/big.csv.gz").unwrap_err();
    assert!(f.message.contains("s3://b/big.csv.gz") && f.message.contains("4095"), "{f}");
}

#[test]
fn a_corrupt_gzip_body_is_unreadable_input() {
    let mut bytes = gz(b"{\"id\":1}\n");
    let n = bytes.len();
    bytes.truncate(n - 6);
    let f = gunzip(&bytes, 1 << 20, "s3://b/cut.jsonl.gz").unwrap_err();
    assert!(f.message.starts_with("PipelineUnreadableInput") && f.message.contains("s3://b/cut.jsonl.gz"), "{f}");
    let f = gunzip(b"not gzip", 1 << 20, "s3://b/plain.gz").unwrap_err();
    assert!(f.message.starts_with("PipelineUnreadableInput"), "{f}");
}
