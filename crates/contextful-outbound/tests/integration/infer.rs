//! The inference endpoint adapter against a loopback OpenAI-compatible server.

use crate::egress::{ok, Ledger, Recording};
use crate::support::{proxy_env, Response, Server};
use contextful_core::memory::synthesize::{Inference, Message};
use contextful_outbound::infer::Endpoint;
use serde_json::{json, Value};

#[test]
fn a_completion_posts_the_conversation_and_returns_the_content() {
    let _env = proxy_env();
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
    let _env = proxy_env();
    let failing = Server::start(|_| Response::json(503, "{\"error\": \"overloaded\"}"));
    let err = Endpoint::new(&failing.url("/v1"), "m", None).unwrap().complete(&[Message::new("user", "x")]).unwrap_err();
    assert!(err.contains("503"), "{err}");
    let empty = Server::start(|_| Response::json(200, "{\"choices\": []}"));
    assert!(Endpoint::new(&empty.url("/v1"), "m", None).unwrap().complete(&[Message::new("user", "x")]).is_err());
    assert!(Endpoint::new("ftp://example.org", "m", None).is_err());
    assert!(failing.received("/v1/chat/completions")[0].header("authorization").is_none());
}

/// A model call sends through the mediated client under {{connector.attach.mediation-covers-every-egress}}, its
/// allowlist the configured endpoint host alone, and passes {{connector.meter.pre-send-hook}} like a vendor hop.
// spec: connector.infer.mediated-call@efeea03a
#[test]
fn a_completion_passes_the_hook_and_the_address_check() {
    let _env = proxy_env();
    let server = Server::start(|_| Response::json(200, &json!({ "choices": [{ "message": { "content": "ok" } }] }).to_string()));
    let events = std::sync::Arc::default();
    let refusing = Ledger::new(&["127.0.0.1"], events);
    let err = Endpoint::new(&server.url("/v1"), "m", Some("k-123".into())).unwrap().with_hook(refusing).complete(&[Message::new("user", "x")]).unwrap_err();
    assert!(err.starts_with("ConnectorEgressRefused"), "{err}");
    assert!(server.requests.lock().unwrap().is_empty(), "a refused completion never leaves the process");

    let admitting = Ledger::new(&[], std::sync::Arc::default());
    let endpoint = Endpoint::new(&server.url("/v1"), "m", None).unwrap().with_hook(admitting.clone());
    assert_eq!(endpoint.complete(&[Message::new("user", "x")]).unwrap(), "ok");
    let intent = admitting.intents.lock().unwrap()[0].clone();
    assert_eq!((intent.method.as_str(), intent.host.as_str()), ("POST", "127.0.0.1"));
    assert_eq!(intent.url, server.url("/v1/chat/completions"));
    assert_eq!(intent.body_bytes as usize, server.received("/v1/chat/completions")[0].body.len());

    // The endpoint's name resolves through the port and its address is vetted like a vendor host's.
    let inward = Recording::new(&[("models.example", "10.0.0.7:0")], vec![ok("{}")], std::sync::Arc::default());
    let err = Endpoint::new("https://models.example/v1", "m", None).unwrap().with_transport(inward.clone()).complete(&[Message::new("user", "x")]).unwrap_err();
    assert!(err.starts_with("ConnectorPrivateAddress"), "{err}");
    assert_eq!((inward.lookups(), inward.sends()), (1, 0));
}
