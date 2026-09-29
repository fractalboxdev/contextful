//! `run.land` over the decode process boundary.
#![cfg(unix)]

use contextful_connectors::boundary::{Boundary, REFUSED};
use contextful_core::run::FailureTag;
use std::time::{Duration, Instant};

fn sh(script: &str) -> Boundary {
    Boundary::new("/bin/sh", &["-c", script])
}

/// A decode that can die runs outside the serving process, which bounds its wall clock and memory and makes it
/// killable.
// spec: run.land.parse-boundary@29514269
#[test]
fn a_decode_runs_in_a_child_the_parent_kills_at_its_deadline() {
    // The child reads its input and answers on standard output.
    assert_eq!(sh("cat").run(b"[\"page one\"]", "Team/Plan").unwrap(), b"[\"page one\"]");
    let started = Instant::now();
    let f = sh("sleep 30").with_deadline(Duration::from_millis(300)).run(b"%PDF-1.4", "Team/Plan").unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(10), "the child is killed at its deadline, not awaited");
    assert!(f.message.starts_with("PipelineParseCrashed") && f.message.contains("Team/Plan") && f.message.contains("wall clock"), "{f}");
    // The parent outlives the killed child and runs the next decode.
    assert_eq!(sh("cat").run(b"[]", "Team/Budget").unwrap(), b"[]");
    // A child growing its data segment past the bound dies of it.
    #[cfg(target_os = "linux")]
    {
        let grow = sh("x=$(head -c 268435456 /dev/zero | tr '\\0' a); echo ${#x}").with_memory(64 * 1024 * 1024);
        let f = grow.run(b"", "Team/huge.pdf").unwrap_err();
        assert!(f.message.starts_with("PipelineParseCrashed") && f.message.contains("Team/huge.pdf"), "{f}");
    }
}

/// A non-zero exit or fatal signal from the decode process raises `PipelineParseCrashed` naming the input; no
/// input ends the serving process.
// spec: run.land.parse-crashed@4a2e1ed3
#[test]
fn a_crashed_decode_is_named_and_a_refusal_crosses_intact() {
    for script in ["kill -9 $$", "exit 3", "cat >/dev/null; exit 101"] {
        let f = sh(script).run(b"%PDF-1.4 truncated", "Finance/Board/report.pdf").unwrap_err();
        assert!(f.message.starts_with("PipelineParseCrashed") && f.message.contains("Finance/Board/report.pdf"), "{script}: {f}");
        assert_eq!(f.tag, FailureTag::Permanent, "{script}");
        assert!(f.deterministic, "{script}: a crash is the same diagnosis on every retry");
    }
    let signal = sh("kill -9 $$").run(b"", "x.pdf").unwrap_err();
    assert!(signal.message.contains("signal"), "{signal}");
    // A typed refusal on the refusal status crosses as itself.
    let refused = sh(&format!("echo 'PipelineUnreadableInput: `x.pdf` at pages 1..2: no page carries extractable text' >&2; exit {REFUSED}")).run(b"", "x.pdf").unwrap_err();
    assert!(refused.message.starts_with("PipelineUnreadableInput"), "{refused}");
    // Any other message on that status is a crash, never a refusal the child spelled.
    let spoofed = sh(&format!("echo 'no pages' >&2; exit {REFUSED}")).run(b"", "x.pdf").unwrap_err();
    assert!(spoofed.message.starts_with("PipelineParseCrashed"), "{spoofed}");
}
