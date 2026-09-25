//! The command source: a pull per child process, reaped under the run's token.

use contextful_core::run::ports::{PullRequest, Source};
use contextful_core::run::FailureTag;
use contextful_engine::cancel::CancelToken;
use contextful_engine::command::CommandSource;
use std::time::Duration;

fn request() -> PullRequest {
    PullRequest { step_label: "pull-0".into(), position: Some(serde_json::json!("p2")), idempotency_key: "k-123".into() }
}

fn alive(pid: i32) -> bool {
    std::process::Command::new("kill").args(["-0", &pid.to_string()]).stderr(std::process::Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

#[test]
fn a_pull_reads_its_request_from_the_environment_and_its_answer_from_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let mut src = CommandSource {
        argv: vec!["sh".into(), "-c".into(), "printf '{\"rows\":[{\"c\":%s,\"k\":\"%s\"}]}' \"$CONTEXTFUL_CURSOR\" \"$CONTEXTFUL_IDEMPOTENCY_KEY\"".into()],
        cwd: dir.path().to_path_buf(),
    };
    let out = src.pull(&request(), &CancelToken::default()).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), r#"{"rows":[{"c":"p2","k":"k-123"}]}"#);
    let mut failing = CommandSource {
        argv: vec!["sh".into(), "-c".into(), "printf '{\"error\":{\"tag\":\"RateLimited\",\"message\":\"slow\",\"retry_after_secs\":4}}'; exit 3".into()],
        cwd: dir.path().to_path_buf(),
    };
    let f = failing.pull(&request(), &CancelToken::default()).unwrap_err();
    assert_eq!((f.tag, f.retry_after_secs), (FailureTag::RateLimited, Some(4)));
    let mut untagged = CommandSource { argv: vec!["sh".into(), "-c".into(), "echo boom >&2; exit 1".into()], cwd: dir.path().to_path_buf() };
    let f = untagged.pull(&request(), &CancelToken::default()).unwrap_err();
    assert_eq!(f.tag, FailureTag::Permanent);
    assert!(f.message.contains("boom"), "{}", f.message);
}

/// A stop signals a running subprocess chain's process group, and the record is written `canceled` only after the
/// group is reaped.
// spec: run.cancel.child-reaped@8c92882f
#[test]
fn a_stop_signals_and_reaps_the_whole_process_group() {
    let dir = tempfile::tempdir().unwrap();
    // The child starts a grandchild that ignores SIGTERM, records its pid, and waits on it.
    let script = "trap '' TERM; sh -c 'trap \"\" TERM; echo $$ > grandchild.pid; while :; do sleep 1; done' & wait";
    let mut src = CommandSource { argv: vec!["sh".into(), "-c".into(), script.into()], cwd: dir.path().to_path_buf() };
    let token = CancelToken::default();
    let t = token.clone();
    let pidfile = dir.path().join("grandchild.pid");
    let p = pidfile.clone();
    std::thread::spawn(move || {
        while !p.exists() {
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(50));
        t.fire();
    });
    let f = src.pull(&request(), &token).unwrap_err();
    assert_eq!(f.tag, FailureTag::Canceled);
    let grandchild: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
    assert!(!alive(grandchild), "the pull returned while grandchild {grandchild} still ran");
}

#[test]
fn a_finished_pull_reaps_what_its_command_left_running() {
    let dir = tempfile::tempdir().unwrap();
    // The command answers and exits, leaving a sleeper that holds its stdout open.
    let script = "sh -c 'echo $$ > leftover.pid; exec sleep 30' & sleep 0.2; printf '{\"rows\":[]}'";
    let mut src = CommandSource { argv: vec!["sh".into(), "-c".into(), script.into()], cwd: dir.path().to_path_buf() };
    let started = std::time::Instant::now();
    let out = src.pull(&request(), &CancelToken::default()).unwrap();
    assert!(started.elapsed() < Duration::from_secs(10), "the pull waited on the leftover's pipe: {:?}", started.elapsed());
    assert_eq!(out, br#"{"rows":[]}"#);
    let leftover: i32 = std::fs::read_to_string(dir.path().join("leftover.pid")).unwrap().trim().parse().unwrap();
    assert!(!alive(leftover), "leftover {leftover} still runs");
}
