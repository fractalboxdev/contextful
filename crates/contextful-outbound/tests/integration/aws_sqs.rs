//! The SQS adapter behind the `Queue` port against a loopback SQS queue and container
//! credential endpoint.

use crate::support::{proxy_env, Response, Server, SetClock};
use contextful_acceptance::sqs::SqsServer;
use contextful_core::run::queue::{BodyRef, Queue, QueueMessage};
use contextful_outbound::aws_sqs::{parse_body, AwsSqs, MESSAGE_CAP};
use std::collections::BTreeMap;
use std::sync::Arc;

fn role() -> Server {
    Server::start(|_| Response::json(200, r#"{"AccessKeyId":"ASIAROLE","SecretAccessKey":"role-secret","Token":"role-token","Expiration":"2999-01-01T00:00:00Z"}"#))
}

fn vars(sqs: &SqsServer, role: &Server) -> BTreeMap<String, String> {
    [
        ("CONTEXTFUL_QUEUE_URL", sqs.queue_url()),
        ("AWS_REGION", "eu-west-1".to_string()),
        ("AWS_ENDPOINT_URL_SQS", sqs.endpoint.clone()),
        ("AWS_CONTAINER_CREDENTIALS_FULL_URI", role.url("/creds")),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

fn queue(sqs: &SqsServer, role: &Server) -> AwsSqs {
    AwsSqs::from_vars(&vars(sqs, role), Arc::new(SetClock::new())).unwrap().expect("the queue URL selects the adapter")
}

fn arrival(key: &str) -> QueueMessage {
    QueueMessage::ObjectArrival { bucket: "lake".into(), key: key.into() }
}

/// `CONTEXTFUL_QUEUE_URL` selects the SQS queue, and every call signs with Signature Version 4 under the workload role;
/// an acknowledgement deletes the message, and a stale receipt is no error.
#[test]
fn the_sqs_queue_sends_receives_and_deletes_signed_under_the_role() {
    let _env = proxy_env();
    let (sqs, creds) = (SqsServer::start(), role());
    let mut unset = vars(&sqs, &creds);
    unset.remove("CONTEXTFUL_QUEUE_URL");
    assert!(AwsSqs::from_vars(&unset, Arc::new(SetClock::new())).unwrap().is_none(), "unset, the local spool serves");

    let q = queue(&sqs, &creds);
    assert_eq!(q.message_cap(), MESSAGE_CAP);
    let batch = QueueMessage::Receive { pipeline: "otel".into(), batch_key: "b-1".into(), credential: None, queued_at: 7, body: BodyRef::Inline { body: b"{}\n".to_vec() } };
    q.send(&batch).unwrap();
    let sent = &sqs.requests("SendMessage")[0];
    assert_eq!(sent.body["QueueUrl"], sqs.queue_url());
    assert_eq!(sent.header("content-type"), Some("application/x-amz-json-1.0"));
    assert_eq!(sent.header("x-amz-security-token"), Some("role-token"));
    let authorization = sent.header("authorization").unwrap();
    assert!(
        authorization.starts_with("AWS4-HMAC-SHA256 Credential=ASIAROLE/20300101/eu-west-1/sqs/aws4_request, SignedHeaders=content-type;host;x-amz-date;x-amz-security-token;x-amz-target, Signature="),
        "{authorization}"
    );

    let got = q.receive(10).unwrap();
    assert_eq!(got.iter().map(|d| d.message.clone()).collect::<Vec<_>>(), [batch]);
    q.ack(&got[0].receipt).unwrap();
    assert!(sqs.pending().is_empty(), "an acknowledgement deletes the message");
    q.ack(&got[0].receipt).unwrap();
}

/// A receive long-polls for 20 s on its first call alone and stops at the first empty answer.
// spec: topology.compose.queue-sqs-wait@5d6ec703
#[test]
fn a_receive_long_polls_once_then_drains_what_waits() {
    let _env = proxy_env();
    let (sqs, creds) = (SqsServer::start(), role());
    let q = queue(&sqs, &creds);
    for n in 0..12 {
        sqs.seed(&serde_json::to_string(&arrival(&format!("feed/{n:02}.jsonl"))).unwrap());
    }
    assert_eq!(q.receive(25).unwrap().len(), 12);
    let waits: Vec<u64> = sqs.requests("ReceiveMessage").iter().map(|r| r.body["WaitTimeSeconds"].as_u64().unwrap()).collect();
    assert_eq!(waits, [20, 0, 0], "the third call answers empty and ends the receive");
}

/// One receive call asks for at most 10 messages, and fewer when the caller wants fewer.
// spec: topology.compose.queue-sqs-batch@e1624082
#[test]
fn one_receive_call_asks_for_at_most_ten_messages() {
    let _env = proxy_env();
    let (sqs, creds) = (SqsServer::start(), role());
    let q = queue(&sqs, &creds);
    for n in 0..12 {
        sqs.seed(&serde_json::to_string(&arrival(&format!("feed/{n:02}.jsonl"))).unwrap());
    }
    assert_eq!(q.receive(3).unwrap().len(), 3);
    assert_eq!(q.receive(25).unwrap().len(), 9);
    let asked: Vec<u64> = sqs.requests("ReceiveMessage").iter().map(|r| r.body["MaxNumberOfMessages"].as_u64().unwrap()).collect();
    assert_eq!(asked, [3, 10, 10]);
}

/// A received message stays hidden for 300 s; one left undeleted redelivers once that window ends.
// spec: topology.compose.queue-sqs-visibility@ff551d71
#[test]
fn an_undeleted_message_stays_hidden_then_redelivers() {
    let _env = proxy_env();
    let (sqs, creds) = (SqsServer::start(), role());
    let q = queue(&sqs, &creds);
    sqs.seed(&serde_json::to_string(&arrival("feed/01.jsonl")).unwrap());
    let first = q.receive(10).unwrap();
    assert_eq!(first.len(), 1);
    assert!(sqs.requests("ReceiveMessage").iter().all(|r| r.body["VisibilityTimeout"] == 300));
    assert!(q.receive(10).unwrap().is_empty(), "a received message stays hidden");
    sqs.expire_visibility();
    let again = q.receive(10).unwrap();
    assert_eq!(again.iter().map(|d| d.message.clone()).collect::<Vec<_>>(), [arrival("feed/01.jsonl")], "an undeleted message redelivers");
    q.ack(&again[0].receipt).unwrap();
    assert!(sqs.pending().is_empty());
}

fn record(event: &str, key: &str) -> serde_json::Value {
    serde_json::json!({ "eventVersion": "2.1", "eventSource": "aws:s3", "eventName": event, "s3": { "bucket": { "name": "lake" }, "object": { "key": key, "size": 8 } } })
}

/// An S3 event notification delivers one arrival per `ObjectCreated` record with its key URL-decoded; the test
/// event is deleted and delivers nothing, and a body neither form reads stays for the redrive policy.
// spec: topology.compose.queue-object-notification@e179e9b1
#[test]
fn an_s3_notification_delivers_one_arrival_per_created_record() {
    let _env = proxy_env();
    let (sqs, creds) = (SqsServer::start(), role());
    let q = queue(&sqs, &creds);
    let notice = serde_json::json!({ "Records": [record("ObjectCreated:Put", "feed/a+b%281%29.jsonl"), record("ObjectRemoved:Delete", "feed/old.jsonl"), record("ObjectCreated:CompleteMultipartUpload", "feed/big.jsonl")] });
    sqs.seed(&notice.to_string());
    sqs.seed(r#"{"Service":"Amazon S3","Event":"s3:TestEvent","Time":"2030-01-01T00:00:00.000Z","Bucket":"lake"}"#);
    sqs.seed("not a message");

    let got = q.receive(10).unwrap();
    assert_eq!(got.iter().map(|d| d.message.clone()).collect::<Vec<_>>(), [arrival("feed/a b(1).jsonl"), arrival("feed/big.jsonl")]);
    assert_eq!(got[0].receipt, got[1].receipt, "the records share their message's receipt");
    assert_eq!(sqs.pending().len(), 2, "the test event is deleted");
    q.ack(&got[0].receipt).unwrap();
    q.ack(&got[1].receipt).unwrap();
    assert_eq!(sqs.pending(), ["not a message"], "an unreadable body waits for the redrive policy");
    assert_eq!(parse_body("not a message"), None);
}
