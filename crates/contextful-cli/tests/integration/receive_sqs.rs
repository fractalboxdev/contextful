//! Queued `receive` batches through an SQS queue: the face sends each batch to the queue a
//! loopback SQS endpoint serves, a body past the cap spills to the store's bucket, and
//! `contextful queue drain` lands what it receives and deletes each message once its outcome
//! records.
#![cfg(feature = "aws-sqs")]

use crate::receive_queued::{deliver, outcome, two_tables};
use crate::serve::{delivered, mint_write, receive_rows, receiving_face_with, serve_env, AUD};
use crate::sync::s3::{container_endpoint, NO_ROLE};
use contextful_acceptance::s3::S3Server;
use contextful_acceptance::sqs::SqsServer;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;

/// The environment selecting `sqs` behind the port, signing under the role `creds` answers.
fn queue_env(sqs: &SqsServer, creds: &str) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = NO_ROLE.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    env[1].1 = creds.to_string();
    env.extend([("CONTEXTFUL_QUEUE_URL".to_string(), sqs.queue_url()), ("AWS_REGION".to_string(), "us-east-1".to_string()), ("AWS_ENDPOINT_URL_SQS".to_string(), sqs.endpoint.clone())]);
    env
}

fn drain(root: &Path, public: &str, env: &[(String, String)]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["queue", "drain", "--project", "research", "--public-key", public, "--audience", AUD])
        .current_dir(root)
        .env_remove("CONTEXTFUL_ISSUER_PUBKEY")
        .env_remove("CONTEXTFUL_AUDIENCE")
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The `receive` batches the queue holds, by batch key.
fn queued(sqs: &SqsServer) -> Vec<Value> {
    sqs.pending().iter().filter_map(|b| serde_json::from_str::<Value>(b).ok()).filter(|m| m["kind"] == "receive").collect()
}

/// `CONTEXTFUL_QUEUE_URL` carries a queued batch through that SQS queue, signed under the workload role: the face
/// sends it, and a drain receives it, lands it, and deletes the message after the outcome records.
// spec: topology.compose.queue-sqs@28febfac
#[test]
fn a_queued_batch_travels_through_the_sqs_queue_signed_under_the_role() {
    let (dir, public) = receiving_face_with(2, 4096);
    let root = dir.path();
    let (sqs, (creds, _)) = (SqsServer::start(), container_endpoint());
    let env = queue_env(&sqs, &creds);
    let pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let both = mint_write(root, "otel_*");
    let (_listener, addr) = serve_env(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "4", "--project", "research", "--public-key", &public], &pairs);

    let (status, receipt) = deliver(&addr, &both, "flush-1", &two_tables(), true);
    assert_eq!(status, 202, "{receipt}");
    let sent = queued(&sqs);
    assert_eq!((sent.len(), sent[0]["batch_key"].as_str()), (1, Some("flush-1")), "{sent:?}");
    assert!(!root.join(".contextful/queue/research").read_dir().is_ok_and(|mut d| d.any(|e| e.is_ok_and(|e| e.path().extension().is_some_and(|x| x == "json")))), "nothing spools beside the configured queue");
    let request = &sqs.requests("SendMessage")[0];
    assert!(request.header("authorization").is_some_and(|a| a.contains("/us-east-1/sqs/aws4_request")), "{request:?}");
    assert_eq!(request.header("x-amz-security-token"), Some("task-role-session"));

    let said = drain(root, &public, &env);
    assert!(said.contains("1 batches"), "{said}");
    assert!(queued(&sqs).is_empty(), "the landed message is deleted");
    assert_eq!(receive_rows(root, "SELECT id FROM otel_spans ORDER BY id"), json!([["s1"], ["s2"]]));
    let (_, landed) = outcome(&addr, &both, "flush-1");
    assert_eq!(landed["outcome"].as_str(), Some("committed"), "{landed}");
}

/// A body past the 1 MiB SQS cap spills to the store's `[sync]` bucket beneath `queue/spill/`, the message carries
/// its reference, and the spill leaves the bucket once the batch's outcome records.
// spec: topology.compose.queue-sqs-cap@376b7bdd
#[test]
fn a_body_past_the_sqs_cap_spills_to_the_store_bucket() {
    let (dir, public) = receiving_face_with(2, 4 << 20);
    let root = dir.path();
    let bucket = S3Server::start("context-team");
    let config = root.join(".contextful/context/research/config.toml");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str(&format!("[sync]\nendpoint = \"{}\"\nbucket = \"context-team\"\nprefix = \"team\"\ncredentials = \"role\"\n", bucket.endpoint));
    std::fs::write(&config, text).unwrap();
    let (sqs, (creds, _)) = (SqsServer::start(), container_endpoint());
    let env = queue_env(&sqs, &creds);
    let pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let both = mint_write(root, "otel_*");
    let (_listener, addr) = serve_env(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "4", "--project", "research", "--public-key", &public], &pairs);
    let pad = "x".repeat(1024);
    let body: String = (0..1200).map(|i| delivered("spans", json!({ "id": format!("s{i:04}"), "pad": pad }))).collect();

    let (status, receipt) = deliver(&addr, &both, "big-1", &body, true);
    assert_eq!(status, 202, "{receipt}");
    let sent = queued(&sqs);
    assert_eq!(sent[0]["body"]["at"], "spilled", "{}", sent[0]);
    let spilled: Vec<String> = bucket.keys().into_iter().filter(|k| k.starts_with("team/queue/spill/")).collect();
    assert_eq!(spilled.len(), 1, "{:?}", bucket.keys());
    assert_eq!(bucket.object(&spilled[0]).map(|b| b.len()), Some(body.len()));

    drain(root, &public, &env);
    assert_eq!(receive_rows(root, "SELECT count(*) FROM otel_spans"), json!([["1200"]]));
    assert!(queued(&sqs).is_empty());
    assert!(bucket.object(&spilled[0]).is_none(), "the spill leaves once the outcome records");
}
