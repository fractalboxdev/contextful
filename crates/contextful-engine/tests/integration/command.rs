//! The command source: a pull per child process, reaped under the run's token.

use contextful_core::run::ports::{PullRequest, Source};
use contextful_core::run::{Failure, FailureTag};
use contextful_engine::cancel::CancelToken;
use contextful_engine::command::CommandSource;
use std::time::Duration;

fn request() -> PullRequest {
    PullRequest { step_label: "pull-0".into(), position: Some(serde_json::json!("p2")), idempotency_key: "k-123".into() }
}

/// Whether `pid` still runs; a zombie waiting on a PID 1 that never reaps does not.
fn alive(pid: i32) -> bool {
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        return stat.rsplit_once(')').is_some_and(|(_, rest)| !matches!(rest.split_whitespace().next(), Some("Z" | "X" | "x")));
    }
    std::process::Command::new("kill").args(["-0", &pid.to_string()]).stderr(std::process::Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

/// Run one pull on a thread of its own, failing the test rather than hanging past 30 s.
fn bounded(mut src: CommandSource, token: CancelToken) -> Result<Vec<u8>, Failure> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(src.pull(&request(), &token));
    });
    rx.recv_timeout(Duration::from_secs(30)).expect("the pull returned within 30 s")
}

#[test]
fn a_pull_reads_its_request_from_the_environment_and_its_answer_from_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let src = CommandSource {
        argv: vec!["sh".into(), "-c".into(), "printf '{\"rows\":[{\"c\":%s,\"k\":\"%s\"}]}' \"$CONTEXTFUL_CURSOR\" \"$CONTEXTFUL_IDEMPOTENCY_KEY\"".into()],
        cwd: dir.path().to_path_buf(),
    };
    let out = bounded(src, CancelToken::default()).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), r#"{"rows":[{"c":"p2","k":"k-123"}]}"#);
    let failing = CommandSource {
        argv: vec!["sh".into(), "-c".into(), "printf '{\"error\":{\"tag\":\"RateLimited\",\"message\":\"slow\",\"retry_after_secs\":4}}'; exit 3".into()],
        cwd: dir.path().to_path_buf(),
    };
    let f = bounded(failing, CancelToken::default()).unwrap_err();
    assert_eq!((f.tag, f.retry_after_secs), (FailureTag::RateLimited, Some(4)));
    let untagged = CommandSource { argv: vec!["sh".into(), "-c".into(), "echo boom >&2; exit 1".into()], cwd: dir.path().to_path_buf() };
    let f = bounded(untagged, CancelToken::default()).unwrap_err();
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
    let src = CommandSource { argv: vec!["sh".into(), "-c".into(), script.into()], cwd: dir.path().to_path_buf() };
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
    let f = bounded(src, token).unwrap_err();
    assert_eq!(f.tag, FailureTag::Canceled);
    let grandchild: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
    assert!(!alive(grandchild), "the pull returned while grandchild {grandchild} still ran");
}

#[test]
fn a_finished_pull_reaps_what_its_command_left_running() {
    let dir = tempfile::tempdir().unwrap();
    // The command answers and exits, leaving a sleeper that holds its stdout open.
    let script = "sh -c 'echo $$ > leftover.pid; exec sleep 30' & sleep 0.2; printf '{\"rows\":[]}'";
    let src = CommandSource { argv: vec!["sh".into(), "-c".into(), script.into()], cwd: dir.path().to_path_buf() };
    let started = std::time::Instant::now();
    let out = bounded(src, CancelToken::default()).unwrap();
    assert!(started.elapsed() < Duration::from_secs(10), "the pull waited on the leftover's pipe: {:?}", started.elapsed());
    assert_eq!(out, br#"{"rows":[]}"#);
    let leftover: i32 = std::fs::read_to_string(dir.path().join("leftover.pid")).unwrap().trim().parse().unwrap();
    assert!(!alive(leftover), "leftover {leftover} still runs");
}

#[test]
fn a_pipe_held_outside_the_group_fails_the_pull_instead_of_blocking_it() {
    let dir = tempfile::tempdir().unwrap();
    // The leftover leaves the group with `setsid`, so reaping the group never closes its stdout. It writes its pid
    // file only after `setsid`, and the shell answers only once that file exists, polling for up to 10 s in 10 ms steps.
    let script = "perl -MPOSIX -e 'POSIX::setsid(); open(my $f, \">\", \"escaped.pid\"); print $f $$; close $f; exec \"sleep\", 30' & \
                  i=0; while [ ! -e escaped.pid ] && [ $i -lt 1000 ]; do sleep 0.01; i=$((i+1)); done; printf '{\"rows\":[]}'";
    let src = CommandSource { argv: vec!["sh".into(), "-c".into(), script.into()], cwd: dir.path().to_path_buf() };
    let started = std::time::Instant::now();
    let f = bounded(src, CancelToken::default()).unwrap_err();
    assert_eq!(f.tag, FailureTag::Transient, "{}", f.message);
    assert!(f.message.contains("stdout") && f.message.contains("outside the group"), "{}", f.message);
    assert!(started.elapsed() < Duration::from_secs(15), "{:?}", started.elapsed());
    if let Ok(pid) = std::fs::read_to_string(dir.path().join("escaped.pid")) {
        let _ = std::process::Command::new("kill").args(["-9", pid.trim()]).status();
    }
}
