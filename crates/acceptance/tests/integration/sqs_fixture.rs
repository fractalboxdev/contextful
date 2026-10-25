//! The loopback SQS queue: send, receive under a visibility timeout, and delete.

use contextful_acceptance::sqs::SqsServer;
use serde_json::{json, Value};

/// One JSON-protocol call of `action` against `sqs`: the status and the answer.
fn call(sqs: &SqsServer, action: &str, body: Value) -> (u16, Value) {
    let response = ureq::post(&sqs.endpoint)
        .header("x-amz-target", &format!("AmazonSQS.{action}"))
        .header("content-type", "application/x-amz-json-1.0")
        .config()
        .http_status_as_error(false)
        .build()
        .send(body.to_string().as_bytes())
        .unwrap();
    let status = response.status().as_u16();
    let text = response.into_body().read_to_string().unwrap();
    (status, serde_json::from_str(&text).unwrap())
}

/// A sent message is received once and hidden until its visibility ends; a delete by its receipt removes it, and a
/// stale receipt answers `ReceiptHandleIsInvalid`.
#[test]
fn the_loopback_queue_hides_a_received_message_until_deleted_or_expired() {
    let sqs = SqsServer::start();
    let url = sqs.queue_url();
    assert_eq!(call(&sqs, "SendMessage", json!({ "QueueUrl": url, "MessageBody": "one" })).0, 200);
    sqs.seed("two");
    let (_, first) = call(&sqs, "ReceiveMessage", json!({ "QueueUrl": url, "MaxNumberOfMessages": 1 }));
    assert_eq!(first["Messages"][0]["Body"], "one");
    let (_, second) = call(&sqs, "ReceiveMessage", json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }));
    assert_eq!(second["Messages"].as_array().map(Vec::len), Some(1), "the received message stays hidden: {second}");
    sqs.expire_visibility();
    let (_, again) = call(&sqs, "ReceiveMessage", json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }));
    let receipts: Vec<&str> = again["Messages"].as_array().unwrap().iter().map(|m| m["ReceiptHandle"].as_str().unwrap()).collect();
    assert_eq!(receipts.len(), 2);
    for receipt in &receipts {
        assert_eq!(call(&sqs, "DeleteMessage", json!({ "QueueUrl": url, "ReceiptHandle": receipt })).0, 200);
    }
    assert!(sqs.pending().is_empty());
    let (status, stale) = call(&sqs, "DeleteMessage", json!({ "QueueUrl": url, "ReceiptHandle": receipts[0] }));
    assert_eq!((status, stale["__type"].as_str()), (400, Some("com.amazonaws.sqs#ReceiptHandleIsInvalid")));
    assert_eq!(sqs.requests("ReceiveMessage").len(), 3);
}
