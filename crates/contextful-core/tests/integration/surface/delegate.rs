//! `surface.delegate`: the binding, the envelope, signed answers and the outcome vocabulary.

use contextful_core::job::{parse_jobs, JobError};
use contextful_core::surface::delegate::{bindings, sign_answer, sign_request, verify_answer, Answer, Binding, BindingError, Envelope, Mode, CALL_TIMEOUT_SECS};
use contextful_core::surface::worker::{sign_submission, SIGNATURE_HEADER, TIMESTAMP_HEADER};
use contextful_core::surface::SurfaceError;
use contextful_core::time::Instant;
use serde_json::json;

const KEY: &[u8] = b"a-deployment-worker-key";
const IDEMPOTENCY: &str = "9e2b-item-key";

fn control(delegates: &str) -> toml::value::Table {
    let value: toml::Value = toml::from_str(&format!("[control]\npool = 2\n{delegates}")).unwrap();
    value["control"].as_table().unwrap().clone()
}

fn binding(table: &str) -> Result<Binding, BindingError> {
    let control = control(&format!("[control.delegates.d]\n{table}"));
    Binding::parse("d", control["delegates"]["d"].as_table().unwrap())
}

fn tables() -> Vec<String> {
    vec!["screen_result".into()]
}

/// `[control.delegates.<name>]` binds a delegate by `url`, a `mode` of `call` or `submit`, and under
/// `call` a `timeout`; a `store-driven` `body = { delegate = "<name>" }` naming no bound delegate raises
/// `JobDelegateUnbound` at arm or fire.
// spec: surface.delegate.binding@ad98c205
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
    assert!(matches!(binding("mode = \"call\"\n"), Err(BindingError::Invalid(m)) if m.contains("`url`")));
    assert!(matches!(binding("url = \"http://127.0.0.1:1\"\nmode = \"poll\"\n"), Err(BindingError::Invalid(m)) if m.contains("`mode`")));
    assert!(matches!(binding("url = \"http://127.0.0.1:1\"\ntimeout = \"0s\"\n"), Err(BindingError::Invalid(m)) if m.contains("at least 1 s")));
    assert!(matches!(binding("url = \"http://127.0.0.1:1\"\nretries = 3\n"), Err(BindingError::Invalid(m)) if m.contains("`retries`")));

    let job = "[[job]]\nname = \"stage\"\nkind = \"store-driven\"\nbody = { delegate = \"stages\" }\nstatement = \"SELECT 1\"\ntables = [\"out\"]\nmax_in_flight = 2\n";
    assert!(parse_jobs(job, &|body: &str| body == stages.body_name()).is_ok());
    assert!(matches!(parse_jobs(job, &|_: &str| false), Err(JobError::JobDelegateUnbound(m)) if m.contains("`stages`")));
}

/// A delegate `url` is `http` to a loopback host or the `contextful.delegate` virtual host, carrying no
/// credential; any other spelling raises `DelegateHostRefused` at startup and arms nothing.
// spec: surface.delegate.host@339b75e3
#[test]
fn a_delegate_url_is_loopback_or_the_virtual_host_over_plain_http_without_a_credential() {
    for ok in ["http://127.0.0.1:9005/stage", "http://localhost:9005", "http://[::1]:9005/x", "http://contextful.delegate/stages"] {
        binding(&format!("url = \"{ok}\"\n")).unwrap_or_else(|e| panic!("{ok}: {e}"));
    }
    for refused in [
        "https://127.0.0.1:9005/stage",
        "http://delegates.example.org/stage",
        "http://10.0.0.7:9005",
        "http://user:pw@127.0.0.1:9005",
        "http://127.0.0.1:9005/stage?api_key=sk-live",
        "http://127.0.0.1:9005/stage#token",
        "not a url",
    ] {
        match binding(&format!("url = \"{refused}\"\n")) {
            Err(BindingError::Refused(SurfaceError::DelegateHostRefused(m))) => assert!(m.contains("[control.delegates.d]"), "{m}"),
            other => panic!("{refused}: expected DelegateHostRefused, got {other:?}"),
        }
    }
}

/// A `store-driven` fire whose delegate binds `mode = "submit"` raises `DelegateSubmitUnfilled`, naming
/// `call` as the mode that dispatches, before any execution opens, so no item receives a receipt.
// spec: surface.delegate.submit-unfilled@38c08f4d
#[test]
fn a_submit_binding_refuses_dispatch_naming_call() {
    let call = binding("url = \"http://127.0.0.1:1\"\nmode = \"call\"\n").unwrap();
    assert_eq!(call.dispatchable(), Ok(()));
    let submit = binding("url = \"http://contextful.delegate/long\"\nmode = \"submit\"\n").unwrap();
    match submit.dispatchable() {
        Err(SurfaceError::DelegateSubmitUnfilled(m)) => assert!(m.contains("[control.delegates.d]") && m.contains("mode = \"call\""), "{m}"),
        other => panic!("expected DelegateSubmitUnfilled, got {other:?}"),
    }
}

/// A `call` delegate's `timeout` defaults to 300 s.
// spec: surface.delegate.call-timeout@bede4158
#[test]
fn a_call_timeout_defaults_to_300_s() {
    assert_eq!((binding("url = \"http://127.0.0.1:1\"\n").unwrap().timeout_secs, CALL_TIMEOUT_SECS), (300, 300));
    assert_eq!(binding("url = \"http://127.0.0.1:1\"\ntimeout = \"5m\"\n").unwrap().timeout_secs, 300);
}

/// A `call` `timeout` above 3600 s raises `DelegateTimeoutRefused` at validation.
// spec: surface.delegate.call-timeout-ceiling@0b22103f
#[test]
fn a_call_timeout_above_the_ceiling_raises_delegate_timeout_refused() {
    assert_eq!(binding("url = \"http://127.0.0.1:1\"\ntimeout = \"1h\"\n").unwrap().timeout_secs, 3600);
    for over in ["3601s", "61m", "2h", "1d"] {
        match binding(&format!("url = \"http://127.0.0.1:1\"\ntimeout = \"{over}\"\n")) {
            Err(BindingError::Refused(SurfaceError::DelegateTimeoutRefused(m))) => assert!(m.contains("3600"), "{m}"),
            other => panic!("{over}: expected DelegateTimeoutRefused, got {other:?}"),
        }
    }
}

/// A binding's optional `dedup_window_secs` declares the seconds the delegate deduplicates a repeated
/// `idempotency_key`; a binding declaring none carries no window.
// spec: surface.delegate.dedup-window@10ae0bc6
#[test]
fn a_binding_declares_its_deduplication_window_in_whole_seconds_and_defaults_to_none() {
    assert_eq!(binding("url = \"http://127.0.0.1:1\"\ndedup_window_secs = 900\n").unwrap().dedup_window_secs, Some(900));
    assert_eq!(binding("url = \"http://127.0.0.1:1\"\n").unwrap().dedup_window_secs, None);
    for off in ["\"15m\"", "-1"] {
        assert!(matches!(binding(&format!("url = \"http://127.0.0.1:1\"\ndedup_window_secs = {off}\n")), Err(BindingError::Invalid(m)) if m.contains("`dedup_window_secs`")), "{off}");
    }
}

/// A request is signed under the worker key over `call`, its timestamp and body, and an answer over `answer`,
/// the request's `idempotency_key`, its timestamp and body, each as `submit-signed` signs a submission.
// spec: surface.delegate.signed@f406d001
#[test]
fn a_request_signs_over_call_and_an_answer_over_answer_and_its_request_key() {
    let body = b"{\"outcome\":\"transient\"}";
    assert_ne!(sign_request(KEY, 7, body), sign_submission(KEY, 7, body), "a request is no submission");
    assert_ne!(sign_request(KEY, 7, body), sign_answer(KEY, IDEMPOTENCY, 7, body), "an answer is no request");
    assert_ne!(sign_answer(KEY, IDEMPOTENCY, 7, body), sign_answer(KEY, "another-key", 7, body), "an answer binds its request");
    assert_ne!(sign_answer(KEY, IDEMPOTENCY, 7, body), sign_answer(KEY, IDEMPOTENCY, 8, body), "and its timestamp");
    assert_eq!(sign_answer(KEY, IDEMPOTENCY, 7, body).len(), 64, "lowercase hex of HMAC-SHA256");
}

/// An answer unsigned or skewed past the callback skew raises `DelegateAnswerRejected` and closes the attempt
/// `Transient`; one outside the outcome vocabulary or over the step-result cap raises it and closes the attempt
/// `Permanent`.
// spec: surface.delegate.rejected@45da8170
#[test]
fn an_answer_verifies_for_its_request_within_the_skew_and_inside_the_vocabulary() {
    let now = Instant::parse("2030-01-01T00:00:00Z").unwrap();
    let body = Answer::Ok { rows: [("screen_result".to_string(), vec![json!({"entity_id": "e-1"}).as_object().unwrap().clone()])].into() }.encode();
    let headers = |timestamp: i64, signature: String| move |name: &str| match name {
        TIMESTAMP_HEADER => Some(timestamp.to_string()),
        SIGNATURE_HEADER => Some(signature.clone()),
        _ => None,
    };
    let at = now.unix_secs();
    verify_answer(KEY, IDEMPOTENCY, headers(at, sign_answer(KEY, IDEMPOTENCY, at, &body)), &body, now).unwrap();
    verify_answer(KEY, IDEMPOTENCY, headers(at - 300, sign_answer(KEY, IDEMPOTENCY, at - 300, &body)), &body, now).unwrap();

    let rejected = |r: Result<(), SurfaceError>| matches!(r, Err(SurfaceError::DelegateAnswerRejected(_)));
    assert!(rejected(verify_answer(KEY, IDEMPOTENCY, headers(at, sign_answer(b"another-key", IDEMPOTENCY, at, &body)), &body, now)), "wrong key");
    assert!(rejected(verify_answer(KEY, IDEMPOTENCY, headers(at, sign_answer(KEY, "another-item", at, &body)), &body, now)), "another item's answer");
    assert!(rejected(verify_answer(KEY, IDEMPOTENCY, headers(at - 301, sign_answer(KEY, IDEMPOTENCY, at - 301, &body)), &body, now)), "skew");
    assert!(rejected(verify_answer(KEY, IDEMPOTENCY, |_| None, &body, now)), "unsigned");
    assert!(rejected(verify_answer(KEY, IDEMPOTENCY, headers(at, sign_answer(KEY, IDEMPOTENCY, at, b"other")), &body, now)), "body differs");

    let decoded = |text: &str| Answer::decode(text.as_bytes(), &tables());
    assert!(matches!(decoded("{\"outcome\":\"retry\"}"), Err(SurfaceError::DelegateAnswerRejected(_))), "outside the vocabulary");
    assert!(matches!(decoded("{\"outcome\":\"ok\",\"rows\":{\"other\":[]}}"), Err(SurfaceError::DelegateAnswerRejected(m)) if m.contains("`other`")), "undeclared table");
    assert!(matches!(decoded("{\"outcome\":\"defer\",\"retry_at\":\"soon\"}"), Err(SurfaceError::DelegateAnswerRejected(_))), "retry_at");
    let over = format!("{{\"outcome\":\"permanent\",\"reason\":\"{}\"}}", "x".repeat(1024 * 1024));
    assert!(matches!(decoded(&over), Err(SurfaceError::DelegateAnswerRejected(m)) if m.contains("cap")), "over the cap");
}

/// An answer is `ok` with rows per declared output table, `defer` with a `retry_at` instant, `transient`, or
/// `permanent` with a reason.
// spec: surface.delegate.outcome@a60f7b39
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

/// A delegate request carries {{type:contextful_core::surface::delegate::Envelope}}: `idempotency_key`
/// under {{run.journal.idempotency-key}}, `entity` with namespace, key and fence, and `version` when fired
/// from an applied snapshot.
// spec: surface.delegate.envelope@f63f7b4d
#[test]
fn the_envelope_serializes_every_field_it_names() {
    let mut envelope = Envelope {
        job: "stage-screen".into(),
        execution_id: "x-1".into(),
        step: "row/p-1/delegate/1".into(),
        input_hash: "ab".repeat(32),
        attempt: 1,
        idempotency_key: IDEMPOTENCY.into(),
        entity: Some(contextful_core::surface::delegate::Entity { namespace: "entity".into(), key: "e-1".into(), fence: 4 }),
        as_of: "2030-01-01T00:00:10Z".into(),
        version: Some(3),
        pins: [("input".to_string(), "cd".repeat(32))].into(),
        tables: tables(),
        row: json!({"entity_id": "e-1", "pass_id": "p-1"}).as_object().unwrap().clone(),
        deadline: "2030-01-01T00:05:10Z".into(),
    };
    let value: serde_json::Value = serde_json::to_value(&envelope).unwrap();
    for field in ["job", "execution_id", "step", "input_hash", "idempotency_key", "attempt", "entity", "as_of", "version", "pins", "tables", "row", "deadline"] {
        assert!(!value[field].is_null(), "{field}");
    }
    assert_eq!(value["entity"]["fence"], 4);
    assert_eq!(serde_json::from_value::<Envelope>(value).unwrap(), envelope);

    // A fire from the local declaration names no applied version, and the key is absent.
    envelope.version = None;
    let value: serde_json::Value = serde_json::to_value(&envelope).unwrap();
    assert!(value.get("version").is_none(), "{value}");
}
