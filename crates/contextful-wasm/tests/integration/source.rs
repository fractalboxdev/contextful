//! The runner's [`Source`] port over a guest session: a failed pull retries from the
//! position the runner asks for, whatever state the failure left the guest in.

use crate::support::{loopback, open, open_with, Response, Server};
use contextful_core::run::ports::{Cancellation, Pull, PullRequest, Source};
use contextful_core::run::FailureTag;
use contextful_wasm::source::cursor_from;
use contextful_wasm::{GuestSource, Limits};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

struct Never;
impl Cancellation for Never {
    fn requested(&self) -> bool {
        false
    }
}

fn request(position: &Option<Value>) -> PullRequest {
    PullRequest { step_label: "pull".into(), position: position.clone(), idempotency_key: "k".into() }
}

/// Drive `source` to exhaustion, retrying each failed pull at the same position once.
fn drain(source: &mut GuestSource) -> (Vec<i64>, Vec<FailureTag>) {
    let (mut position, mut ids, mut failures) = (None, Vec::new(), Vec::new());
    loop {
        let bytes = match source.pull(&request(&position), &Never) {
            Ok(b) => b,
            Err(f) => {
                failures.push(f.tag);
                source.pull(&request(&position), &Never).expect("the retry reads on")
            }
        };
        let pull = Pull::decode(&bytes).unwrap();
        ids.extend(pull.rows.iter().map(|r| r["id"].as_i64().unwrap()));
        position = pull.cursor;
        if !pull.more {
            return (ids, failures);
        }
    }
}

#[test]
fn a_pull_retried_after_a_guest_error_reopens_at_the_requested_position() {
    // The guest's handle runs to its end before failing the second batch, so a retry
    // on the same handle would report exhaustion and lose row 3.
    let (ids, failures) = drain(&mut GuestSource::new(open(), "flaky"));
    assert_eq!(failures, [FailureTag::Transient]);
    assert_eq!(ids, [1, 2, 3]);
}

#[test]
fn a_pull_retried_after_a_trap_runs_on_a_fresh_instance() {
    let served = Arc::new(AtomicUsize::new(0));
    let n = served.clone();
    let server = Server::start(move |_| {
        if n.fetch_add(1, Ordering::SeqCst) == 0 {
            std::thread::sleep(Duration::from_secs(2));
        }
        Response::text(200, "ok")
    });
    let short = Limits { read_deadline: Duration::from_millis(300), ..Limits::default() };
    let mut source = GuestSource::new(open_with(loopback(), &short, None).unwrap(), format!("fetch {}", server.url("/v1")));
    let f = source.pull(&request(&None), &Never).unwrap_err();
    assert_eq!(f.tag, FailureTag::Transient, "{f}");
    let pull = Pull::decode(&source.pull(&request(&None), &Never).expect("the retry runs on a fresh instance")).unwrap();
    assert_eq!(pull.rows[0]["body"], json!("200 ok"));
    assert_eq!(served.load(Ordering::SeqCst), 2);
}

#[test]
fn a_position_holding_non_hex_text_is_refused_not_a_panic() {
    for bytes in ["a€", "zz", "abc"] {
        let f = cursor_from(&json!({ "kind": "monotonic", "bytes": bytes })).unwrap_err();
        assert_eq!(f.tag, FailureTag::SchemaIncompatible, "{bytes}: {f}");
    }
    assert_eq!(cursor_from(&json!({ "kind": "monotonic", "bytes": "3132" })).unwrap().bytes, b"12".to_vec());
}
