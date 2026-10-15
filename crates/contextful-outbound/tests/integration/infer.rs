//! The inference endpoint adapter against a loopback OpenAI-compatible server.

use crate::egress::{ok, Ledger, Recording};
use crate::support::{proxy_env, Response, Server};
use contextful_core::memory::synthesize::{CallRecord, Inference, Message};
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

/// A model call under a credential carrying {{authority.grant.horizon}} that declares a provider-hosted tool, such as search, browse or fetch, raises `InferHostedToolUnderHorizon`, naming the tool, before any byte leaves.
// spec: connector.infer.hosted-tools@c42993b6
#[test]
fn a_hosted_tool_under_a_horizon_is_refused_before_any_byte_leaves() {
    let _env = proxy_env();
    let server = Server::start(|_| Response::json(200, &json!({ "choices": [{ "message": { "role": "assistant", "content": "ok" } }] }).to_string()));
    let horizon = Some(contextful_core::time::Instant::parse("2015-01-01T00:00:00Z").unwrap());
    let searching = || Endpoint::new(&server.url("/v1"), "m", None).unwrap().with_hosted_tools(vec!["web_search".into()]);

    let err = searching().under_horizon(horizon).complete(&[Message::new("user", "x")]).unwrap_err();
    assert!(err.starts_with("InferHostedToolUnderHorizon") && err.contains("web_search"), "{err}");
    assert!(server.received("/v1/chat/completions").is_empty());

    // Without a horizon the tool is declared on the wire; a horizon without one sends as before.
    assert_eq!(searching().complete(&[Message::new("user", "x")]).unwrap(), "ok");
    let body: Value = serde_json::from_slice(&server.received("/v1/chat/completions")[0].body).unwrap();
    assert_eq!(body["tools"], json!([{ "type": "web_search" }]));
    let plain = Endpoint::new(&server.url("/v1"), "m", None).unwrap().under_horizon(horizon);
    assert_eq!(plain.complete(&[Message::new("user", "x")]).unwrap(), "ok");
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

/// A model call with no configured key declares no header and keeps the system proxy; a configured key is a declared
/// header and takes the hardened client under {{connector.attach.header-selects-client}}.
#[test]
fn a_completion_keeps_the_proxy_unless_a_key_is_configured() {
    let answer = json!({ "choices": [{ "message": { "content": "ok" } }] }).to_string();
    for (key, direct) in [(None, false), (Some("k-123".to_string()), true)] {
        let t = Recording::new(&[("models.example", "93.184.216.34:0")], vec![ok(&answer)], std::sync::Arc::default());
        let endpoint = Endpoint::new("https://models.example/v1", "m", key.clone()).unwrap().with_transport(t.clone());
        assert_eq!(endpoint.complete(&[Message::new("user", "x")]).unwrap(), "ok");
        assert_eq!(*t.directs.lock().unwrap(), [direct], "key {key:?}");
    }
}

/// A call to an endpoint exposing token log-probabilities lands them with the call record.
// spec: connector.infer.logprobs@9eefec74
#[test]
fn an_endpoint_exposing_logprobs_lands_them_with_the_call_record() {
    let _env = proxy_env();
    let logprobs = json!({ "content": [{ "token": "Yes", "logprob": -0.02, "top_logprobs": [] }] });
    let reply = json!({ "choices": [{ "message": { "content": "Yes" }, "logprobs": logprobs }] }).to_string();
    let server = Server::start(move |_| Response::json(200, &reply));
    let exposing = Endpoint::new(&server.url("/v1"), "m", None).unwrap().with_logprobs();
    let record = exposing.call(&[Message::new("user", "Did it close above 40?")]).unwrap();
    assert_eq!(record, CallRecord { content: "Yes".into(), logprobs: Some(logprobs.clone()) });
    let sent: Value = serde_json::from_slice(&server.received("/v1/chat/completions")[0].body).unwrap();
    assert_eq!(sent["logprobs"], json!(true));
    assert_eq!(serde_json::to_value(&record).unwrap()["logprobs"], logprobs);

    // An endpoint not declared as exposing them asks for none and lands none.
    let plain = Endpoint::new(&server.url("/v1"), "m", None).unwrap();
    assert_eq!(plain.call(&[Message::new("user", "x")]).unwrap().logprobs, None);
    let sent: Value = serde_json::from_slice(&server.received("/v1/chat/completions")[1].body).unwrap();
    assert!(sent.get("logprobs").is_none(), "{sent}");
}
