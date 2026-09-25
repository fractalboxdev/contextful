//! The inference endpoint adapter against a loopback OpenAI-compatible server.

use crate::support::{Response, Server};
use contextful_core::memory::synthesize::{Inference, Message};
use contextful_runtime::infer::Endpoint;
use serde_json::{json, Value};

#[test]
fn a_completion_posts_the_conversation_and_returns_the_content() {
    let server = Server::start(|_| Response::json(200, &json!({ "choices": [{ "message": { "role": "assistant", "content": "{\"claims\": []}" } }] }).to_string()));
    let endpoint = Endpoint::new(&server.url("/v1/"), "fixture", Some("k-123".into())).unwrap();
    let content = endpoint.complete(&[Message::new("system", "rules"), Message::new("user", "data")]).unwrap();
    assert_eq!(content, "{\"claims\": []}");
    let sent = server.received("/v1/chat/completions");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].method, "POST");
    assert_eq!(sent[0].header("authorization"), Some("Bearer k-123"));
    let body: Value = serde_json::from_slice(&sent[0].body).unwrap();
    assert_eq!(body["model"], json!("fixture"));
    assert_eq!(body["messages"], json!([{ "role": "system", "content": "rules" }, { "role": "user", "content": "data" }]));
}

#[test]
fn a_failed_or_malformed_answer_is_an_error() {
    let failing = Server::start(|_| Response::json(503, "{\"error\": \"overloaded\"}"));
    let err = Endpoint::new(&failing.url("/v1"), "m", None).unwrap().complete(&[Message::new("user", "x")]).unwrap_err();
    assert!(err.contains("503"), "{err}");
    let empty = Server::start(|_| Response::json(200, "{\"choices\": []}"));
    assert!(Endpoint::new(&empty.url("/v1"), "m", None).unwrap().complete(&[Message::new("user", "x")]).is_err());
    assert!(Endpoint::new("ftp://example.org", "m", None).is_err());
    assert!(failing.received("/v1/chat/completions")[0].header("authorization").is_none());
}
