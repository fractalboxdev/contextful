//! `surface.delegate`: the binding, the envelope, signed answers and the outcome vocabulary.

use contextful_core::surface::delegate::{bindings, sign_answer, Answer, Binding, BindingError, Envelope, Mode, CALL_TIMEOUT_SECS, verify_answer};
use contextful_core::surface::worker::{SIGNATURE_HEADER, TIMESTAMP_HEADER};
use contextful_core::surface::SurfaceError;
use contextful_core::time::Instant;
use serde_json::json;

const KEY: &[u8] = b"a-deployment-worker-key";

fn control(delegates: &str) -> toml::value::Table {
    let value: toml::Value = toml::from_str(&format!("[control]\npool = 2\n{delegates}")).unwrap();
    value["control"].as_table().unwrap().clone()
}

fn tables() -> Vec<String> {
    vec!["screen_result".into()]
}

/// `[control.delegates.<name>]` binds a delegate by `url`, a `mode` of `call` or `submit`, and under
/// `call` a `timeout`; a `store-driven` `body = { delegate = "<name>" }` naming no bound delegate raises
/// `JobDelegateUnbound` at validation.
// spec: surface.delegate.binding@2f877eb8
#[test]
fn a_binding_reads_its_url_mode_and_timeout_and_names_its_body() {
    let bound = bindings(&control("[control.delegates.stages]\nurl = \"http://127.0.0.1:9005/stage\"\nmode = \"call\"\ntimeout = \"30s\"\nversion = \"v7\"\n")).unwrap();
    let stages = &bound["stages"];
    assert_eq!((stages.mode, stages.timeout_secs, stages.version.as_deref()), (Mode::Call, 30, Some("v7")));
    assert_eq!(stages.url.as_str(), "http://127.0.0.1:9005/stage");
    assert_eq!(stages.body_name(), "delegate:stages");

    let submit = bindings(&control("[control.delegates.long]\nurl = \"http://contextful.delegate/long\"\nmode = \"submit\"\n")).unwrap();
    assert_eq!(submit["long"].mode, Mode::Submit);
    assert!(bindings(&control("")).unwrap().is_empty());
    assert!(matches!(bindings(&control("[control.delegates.x]\nmode = \"call\"\n")), Err(BindingError::Invalid(m)) if m.contains("`url`")));
    assert!(matches!(bindings(&control("[control.delegates.x]\nurl = \"http://127.0.0.1:1\"\nmode = \"poll\"\n")), Err(BindingError::Invalid(m)) if m.contains("`mode`")));
}

/// A delegate `url` is `http` to a loopback host or the `contextful.delegate` virtual host, carrying no
/// credential; any other spelling raises `DelegateHostRefused` at startup and arms nothing.
// spec: surface.delegate.host@339b75e3
#[test]
fn a_delegate_url_is_loopback_or_the_virtual_host_over_plain_http_without_a_credential() {
    for ok in ["http://127.0.0.1:9005/stage", "http://localhost:9005", "http://[::1]:9005/x", "http://contextful.delegate/stages"] {
        Binding::parse("d", control(&format!("[control.delegates.d]\nurl = \"{ok}\"\n"))["delegates"]["d"].as_table().unwrap()).unwrap_or_else(|e| panic!("{ok}: {e}"));
    }
    for refused in ["https://127.0.0.1:9005/stage", "http://delegates.example.org/stage", "http://10.0.0.7:9005", "http://user:pw@127.0.0.1:9005", "not a url"] {
        match Binding::parse("d", control(&format!("[control.delegates.d]\nurl = \"{refused}\"\n"))["delegates"]["d"].as_table().unwrap()) {
            Err(BindingError::Refused(SurfaceError::DelegateHostRefused(m))) => assert!(m.contains("[control.delegates.d]"), "{m}"),
            other => panic!("{refused}: expected DelegateHostRefused, got {other:?}"),
        }
    }
}

/// A `call` delegate's `timeout` defaults to 300 s.
// spec: surface.delegate.call-timeout@bede4158
#[test]
fn a_call_timeout_defaults_to_300_s() {
    let bound = bindings(&control("[control.delegates.d]\nurl = \"http://127.0.0.1:1\"\n")).unwrap();
    assert_eq!((bound["d"].timeout_secs, CALL_TIMEOUT_SECS), (300, 300));
    assert_eq!(bindings(&control("[control.delegates.d]\nurl = \"http://127.0.0.1:1\"\ntimeout = \"5m\"\n")).unwrap()["d"].timeout_secs, 300);
}

/// A `call` `timeout` above 3600 s raises `DelegateTimeoutRefused` at validation.
// spec: surface.delegate.call-timeout-ceiling@0b22103f
#[test]
fn a_call_timeout_above_the_ceiling_raises_delegate_timeout_refused() {
    assert_eq!(bindings(&control("[control.delegates.d]\nurl = \"http://127.0.0.1:1\"\ntimeout = \"1h\"\n")).unwrap()["d"].timeout_secs, 3600);
    for over in ["3601s", "61m", "2h"] {
        match bindings(&control(&format!("[control.delegates.d]\nurl = \"http://127.0.0.1:1\"\ntimeout = \"{over}\"\n"))) {
            Err(BindingError::Refused(SurfaceError::DelegateTimeoutRefused(m))) => assert!(m.contains("3600"), "{m}"),
            other => panic!("{over}: expected DelegateTimeoutRefused, got {other:?}"),
        }
    }
}

/// A request is signed as `submit-signed` signs a submission; an answer unsigned, skewed past the callback
/// skew, outside the outcome vocabulary or over the step-result cap raises `DelegateAnswerRejected`, and the
/// attempt closes `Transient`.
// spec: surface.delegate.signed@53c9c992
#[test]
fn an_answer_verifies_under_the_key_within_the_skew_and_inside_the_vocabulary() {
    let now = Instant::parse("2030-01-01T00:00:00Z").unwrap();
    let body = Answer::Ok { rows: [("screen_result".to_string(), vec![json!({"entity_id": "e-1"}).as_object().unwrap().clone()])].into() }.encode();
    let headers = |timestamp: i64, signature: String| move |name: &str| match name {
        TIMESTAMP_HEADER => Some(timestamp.to_string()),
        SIGNATURE_HEADER => Some(signature.clone()),
        _ => None,
    };
    verify_answer(KEY, headers(now.unix_secs(), sign_answer(KEY, now.unix_secs(), &body)), &body, now).unwrap();
    verify_answer(KEY, headers(now.unix_secs() - 300, sign_answer(KEY, now.unix_secs() - 300, &body)), &body, now).unwrap();

    let rejected = |r: Result<(), SurfaceError>| matches!(r, Err(SurfaceError::DelegateAnswerRejected(_)));
    assert!(rejected(verify_answer(KEY, headers(now.unix_secs(), sign_answer(b"another-key", now.unix_secs(), &body)), &body, now)), "wrong key");
    assert!(rejected(verify_answer(KEY, headers(now.unix_secs() - 301, sign_answer(KEY, now.unix_secs() - 301, &body)), &body, now)), "skew");
    assert!(rejected(verify_answer(KEY, |_| None, &body, now)), "unsigned");
    assert!(rejected(verify_answer(KEY, headers(now.unix_secs(), sign_answer(KEY, now.unix_secs(), b"other")), &body, now)), "body differs");

    let decoded = |text: &str| Answer::decode(text.as_bytes(), &tables());
    assert!(matches!(decoded("{\"outcome\":\"retry\"}"), Err(SurfaceError::DelegateAnswerRejected(_))), "outside the vocabulary");
    assert!(matches!(decoded("{\"outcome\":\"ok\",\"rows\":{\"other\":[]}}"), Err(SurfaceError::DelegateAnswerRejected(m)) if m.contains("`other`")), "undeclared table");
    assert!(matches!(decoded("{\"outcome\":\"defer\",\"retry_at\":\"soon\"}"), Err(SurfaceError::DelegateAnswerRejected(_))), "retry_at");
    let over = format!("{{\"outcome\":\"permanent\",\"reason\":\"{}\"}}", "x".repeat(1024 * 1024));
    assert!(matches!(decoded(&over), Err(SurfaceError::DelegateAnswerRejected(m)) if m.contains("cap")), "over the cap");
}

/// An answer is `ok` with rows per declared output table, `defer` with a `retry_at` instant, `transient`, or
/// `permanent` with a reason.
// spec: surface.delegate.outcome@326a0759
#[test]
fn the_outcome_vocabulary_round_trips() {
    let answers = [
        Answer::Ok { rows: [("screen_result".to_string(), vec![json!({"entity_id": "e-1", "proceed": true}).as_object().unwrap().clone()])].into() },
        Answer::Defer { retry_at: "2030-01-01T00:10:00Z".into(), reason: Some("budget".into()) },
        Answer::Transient { reason: None },
        Answer::Permanent { reason: Some("subject withdrawn".into()) },
    ];
    for answer in answers {
        assert_eq!(Answer::decode(&answer.encode(), &tables()).unwrap(), answer);
    }
    let text: serde_json::Value = serde_json::from_slice(&Answer::Transient { reason: None }.encode()).unwrap();
    assert_eq!(text, json!({"outcome": "transient"}));
}

/// A delegate request carries job name, execution id, step label, input hash, the idempotency key, the input
/// row, entity namespace, key and fence, attempt, resolved `as_of`, applied version, owner pins, declared
/// output tables and the item deadline.
// spec: surface.delegate.envelope@fe98cf00
#[test]
fn the_envelope_serializes_every_field_it_names() {
    let envelope = Envelope {
        job: "stage-screen".into(),
        execution_id: "x-1".into(),
        step: "row/p-1/delegate/1".into(),
        input_hash: "ab".repeat(32),
        attempt: 1,
        idempotency_key: "k".into(),
        entity: Some(contextful_core::surface::delegate::Entity { namespace: "entity".into(), key: "e-1".into(), fence: 4 }),
        as_of: "2030-01-01T00:00:10Z".into(),
        version: 3,
        pins: [("input".to_string(), "cd".repeat(32))].into(),
        tables: tables(),
        row: json!({"entity_id": "e-1", "pass_id": "p-1"}).as_object().unwrap().clone(),
        deadline: "2030-01-01T00:05:10Z".into(),
    };
    let value: serde_json::Value = serde_json::to_value(&envelope).unwrap();
    for field in ["job", "execution_id", "step", "input_hash", "attempt", "idempotency_key", "entity", "as_of", "version", "pins", "tables", "row", "deadline"] {
        assert!(!value[field].is_null(), "{field}");
    }
    assert_eq!(value["entity"]["fence"], 4);
    assert_eq!(serde_json::from_value::<Envelope>(value).unwrap(), envelope);
}
