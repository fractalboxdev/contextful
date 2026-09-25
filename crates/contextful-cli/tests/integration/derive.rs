//! `contextful derive test-engine` through the built binary.

use std::process::{Command, Output};

fn cf(dir: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful")).args(args).current_dir(dir).output().unwrap()
}

/// A `fetch` engine named to the verb raises `DeriveTestEngineUnsupported`.
// spec: run.test-engine.unsupported-driver@48b970c1
#[test]
fn the_verb_refuses_a_fetch_engine_and_runs_a_transcriber_over_one_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("contextful.toml"),
        "[derive.cards]\ndriver = \"fetch\"\nallow_hosts = [\"example.com\"]\n\n[derive.reader]\ndriver = \"exec\"\n[derive.reader.engine]\ncommand = [\"cat\", \"{input}\"]\noutput_format = \"srt\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("caption.srt"), "1\n00:00:01,000 --> 00:00:02,000\nhello\n").unwrap();
    let out = cf(dir.path(), &["derive", "test-engine", "cards", "--file", "caption.srt"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("DeriveTestEngineUnsupported"));
    let out = cf(dir.path(), &["derive", "test-engine", "reader", "--file", "caption.srt"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let line: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(line, serde_json::json!({"start_ms": 1000, "end_ms": 2000, "text": "hello"}));
}
