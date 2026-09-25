//! The exec driver and the derive source.

use crate::support::Never;
use contextful_connectors::derive::{resolve_step, run_chain, Chain, DeriveSource};
use contextful_core::run::derive::config::{bindings, Binding, DeriveConfig};
use contextful_core::run::journal::sha256_hex;
use contextful_core::run::ports::{PullRequest, Row, Source, TableReader};
use contextful_core::run::{Failure, RunError};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

fn binding(toml: &str) -> Binding {
    bindings(toml).unwrap().remove("reader").unwrap()
}

fn chain(dir: &Path, toml: &str) -> Result<Chain, RunError> {
    Chain::resolve("reader", &binding(toml), dir)
}

fn run(dir: &Path, toml: &str, media: &str) -> Result<String, RunError> {
    let c = chain(dir, toml)?;
    let env: Vec<(String, String)> = c.env.iter().map(|(k, t)| (k.clone(), t.render(|_| Ok::<_, ()>(contextful_core::connector::reference::Hydrated::new(""))).unwrap().reveal().to_string())).collect();
    let scratch = dir.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    run_chain(&c, &dir.join(media), &env, &scratch, &Never)
}

fn script(dir: &Path, name: &str, body: &str) -> String {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::process::Command::new("chmod").args(["755", p.to_str().unwrap()]).status().unwrap();
    sha256_hex(&std::fs::read(&p).unwrap())
}

/// A `command[0]` that is not an executable file on this machine raises `DeriveBinaryMissing`, naming the binary
/// and the step.
// spec: run.exec.missing-binary@bf83a423
#[test]
fn a_command_that_is_no_executable_file_refuses() {
    let dir = tempfile::tempdir().unwrap();
    for cmd in ["no-such-binary-contextful", "./absent.sh"] {
        match chain(dir.path(), &format!("[derive.reader.engine]\ncommand = [\"{cmd}\"]\nsha256 = \"00\"\n")) {
            Err(RunError::DeriveBinaryMissing(m)) => assert!(m.contains(cmd) && m.contains("engine"), "{m}"),
            other => panic!("{cmd}: {:?}", other.err()),
        }
    }
}

/// A path-form `command[0]` without `sha256` raises `DeriveUnpinnedPath`, quoting the digest just computed; a
/// bare search-path name carries none.
// spec: run.exec.unpinned-path@75300ef2
#[test]
fn a_path_form_command_without_a_digest_refuses_quoting_it() {
    let dir = tempfile::tempdir().unwrap();
    let digest = script(dir.path(), "engine.sh", "cat \"$1\"");
    match chain(dir.path(), "[derive.reader.engine]\ncommand = [\"./engine.sh\", \"{input}\"]\n") {
        Err(RunError::DeriveUnpinnedPath(m)) => assert!(m.contains(&digest), "{m}"),
        other => panic!("{:?}", other.err()),
    }
    // A bare search-path name carries no pin.
    assert!(chain(dir.path(), "[derive.reader.engine]\ncommand = [\"cat\", \"{input}\"]\n").is_ok());
}

/// A pinned file whose bytes differ from its recorded digest raises `DeriveDigestMismatch`.
// spec: run.exec.digest-mismatch@52f114c3
#[test]
fn a_pinned_file_whose_bytes_moved_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let digest = script(dir.path(), "engine.sh", "cat \"$1\"");
    let toml = format!("[derive.reader.engine]\ncommand = [\"./engine.sh\", \"{{input}}\"]\nsha256 = \"{digest}\"\n");
    assert!(chain(dir.path(), &toml).is_ok());
    script(dir.path(), "engine.sh", "rm -rf \"$1\"");
    assert!(matches!(chain(dir.path(), &toml), Err(RunError::DeriveDigestMismatch(_))));
}

/// A step runs as its argument array with no shell, in a process group of its own, under a cleared environment
/// holding only the binding's `env` table.
// spec: run.exec.no-shell@a5b98206
#[test]
fn a_step_runs_its_argument_array_with_a_cleared_environment() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("doc.txt"), "x").unwrap();
    // SAFETY: the test binary sets no other variable concurrently with this read.
    unsafe { std::env::set_var("CONTEXTFUL_LEAK_PROBE", "leaked") };
    let toml = "[derive.reader.engine]\ncommand = [\"/usr/bin/env\"]\n[derive.reader.env]\nLANG = \"C\"\n";
    let digest = sha256_hex(&std::fs::read("/usr/bin/env").unwrap());
    let out = run(dir.path(), &toml.replace("[\"/usr/bin/env\"]", &format!("[\"/usr/bin/env\"]\nsha256 = \"{digest}\"")), "doc.txt").unwrap();
    assert_eq!(out.trim(), "LANG=C", "only the binding's env reaches a step");
    // An argument carrying shell syntax reaches the program as one literal argument.
    let echo = "[derive.reader.engine]\ncommand = [\"printf\", \"%s\", \"$(echo injected){input}\"]\n";
    let out = run(dir.path(), echo, "doc.txt").unwrap();
    assert!(out.starts_with("$(echo injected)"), "{out}");
}

/// `timeout_secs` bounds one unit's whole chain at 1800 s by default.
// spec: run.exec.chain-deadline@d42f55a9
#[test]
fn the_chain_runs_1800_s_by_default_and_the_declared_bound_otherwise() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(chain(dir.path(), "[derive.reader.engine]\ncommand = [\"cat\"]\n").unwrap().deadline.as_secs(), 1800);
    assert_eq!(chain(dir.path(), "[derive.reader]\ntimeout_secs = 7\n[derive.reader.engine]\ncommand = [\"cat\"]\n").unwrap().deadline.as_secs(), 7);
}

/// A chain outrunning its deadline raises `DeriveStepTimeout`, naming the running step.
// spec: run.exec.deadline-elapsed@f4347448
#[test]
fn a_chain_outrunning_its_deadline_refuses_naming_the_step() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("doc.txt"), "x").unwrap();
    let started = std::time::Instant::now();
    match run(dir.path(), "[derive.reader]\ntimeout_secs = 1\n[derive.reader.engine]\ncommand = [\"sleep\", \"30\"]\n", "doc.txt") {
        Err(RunError::DeriveStepTimeout(m)) => assert!(m.contains("`engine`"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(started.elapsed().as_secs() < 10);
}

/// `max_output_bytes` bounds each step's captured standard output and error at 8 MiB by default.
// spec: run.exec.captured-output@b46b1b34
#[test]
fn a_step_captures_8_mib_by_default_and_the_declared_bound_otherwise() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(chain(dir.path(), "[derive.reader.engine]\ncommand = [\"cat\"]\n").unwrap().max_output, 8 * 1024 * 1024);
    assert_eq!(chain(dir.path(), "[derive.reader]\nmax_output_bytes = 512\n[derive.reader.engine]\ncommand = [\"cat\"]\n").unwrap().max_output, 512);
}

/// Output past the bound is counted and discarded, never buffered; crossing it kills the step and raises
/// `DeriveOutputCap` with the byte count, draining continuing so the child never blocks.
// spec: run.exec.output-cap@004fc225
#[test]
fn output_past_the_bound_stops_the_step_with_the_byte_count() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("big.txt"), "y".repeat(200_000)).unwrap();
    match run(dir.path(), "[derive.reader]\nmax_output_bytes = 1000\n[derive.reader.engine]\ncommand = [\"cat\", \"{input}\"]\n", "big.txt") {
        Err(RunError::DeriveOutputCap(m)) => {
            let count: u64 = m.split_whitespace().find_map(|w| w.parse().ok()).unwrap();
            assert!(count > 1000, "{m}");
        }
        other => panic!("{other:?}"),
    }
}

/// A preprocess step exiting zero without writing its output file raises `DeriveStepProducedNothing`.
// spec: run.exec.silent-step@116cf075
#[test]
fn a_preprocess_step_writing_no_output_refuses() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("doc.txt"), "x").unwrap();
    let toml = "[[derive.reader.preprocess]]\ncommand = [\"true\"]\noutput_path = \"{output_stem}.wav\"\n[derive.reader.engine]\ncommand = [\"cat\", \"{input}\"]\n";
    assert!(matches!(run(dir.path(), toml, "doc.txt"), Err(RunError::DeriveStepProducedNothing(m)) if m.contains("preprocess-0")));
    let copying = "[[derive.reader.preprocess]]\ncommand = [\"cp\", \"{input}\", \"{output}\"]\noutput_path = \"{output_stem}.txt\"\n[derive.reader.engine]\ncommand = [\"cat\", \"{input}\"]\n";
    assert_eq!(run(dir.path(), copying, "doc.txt").unwrap(), "x");
}

struct Rows(Vec<(String, Vec<Row>)>);

impl TableReader for Rows {
    fn rows(&self, table: &str) -> Result<Vec<Row>, Failure> {
        Ok(self.0.iter().find(|(t, _)| t == table).map(|(_, r)| r.clone()).unwrap_or_default())
    }
}

fn source(dir: &Path, parents: Value, engine_toml: &str) -> DeriveSource {
    let config = DeriveConfig::parse("doc-text", &json!({"engine": "reader", "source_table": "documents", "media_column": "path", "parent_id_column": "doc_id"})).unwrap();
    let parents: Vec<Row> = parents.as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect();
    DeriveSource {
        pipeline_id: "doc-text".into(),
        config,
        binding: binding(engine_toml),
        output_table: "doc_text_passages".into(),
        reader: Box::new(Rows(vec![("documents".into(), parents)])),
        resolver: Arc::new(contextful_runtime::Resolver::new(vec![], false, Arc::new(contextful_core::ports::FixedClock(contextful_core::time::Instant::from_unix_secs(0).unwrap())))),
        cwd: dir.to_path_buf(),
    }
}

fn pulled(s: &mut DeriveSource) -> Vec<Value> {
    let req = PullRequest { step_label: "pull-0".into(), position: None, idempotency_key: "k".into() };
    let v: Value = serde_json::from_slice(&s.pull(&req, &Never).unwrap()).unwrap();
    v["rows"].as_array().unwrap().clone()
}

const SRT_ENGINE: &str = "[derive.reader.engine]\ncommand = [\"awk\", \"NF { n++; printf \\\"%d\\\\n00:00:0%d,000 --> 00:00:0%d,500\\\\n%s\\\\n\\\\n\\\", n, n, n, $0 }\", \"{input}\"]\noutput_format = \"srt\"\n";

/// A media value that is neither an address nor a readable local file raises `DeriveMediaUnreadable`, failing
/// that unit alone.
// spec: run.bind.media-unreadable@d9205180
#[test]
fn unreadable_media_fails_that_unit_alone() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("memo.txt"), "Hello there.\n").unwrap();
    let mut s = source(dir.path(), json!([{"doc_id": "lost", "path": "nowhere.txt"}, {"doc_id": "memo", "path": "memo.txt"}]), SRT_ENGINE);
    let rows = pulled(&mut s);
    let lost = rows.iter().find(|r| r["unit_ref"] == "lost").unwrap();
    assert_eq!((lost["kind"].as_str(), lost["unit_status"].as_str()), (Some("marker"), Some("failed")));
    assert!(lost["last_error"].as_str().unwrap().starts_with("DeriveMediaUnreadable"), "{lost}");
    let memo = rows.iter().find(|r| r["unit_ref"] == "memo").unwrap();
    assert_eq!((memo["kind"].as_str(), memo["text"].as_str()), (Some("passage"), Some("Hello there.")));
}

/// An `http` or `https` media value for an engine declining remote addresses raises `DeriveRemoteUrlUnsupported`,
/// naming the `when = "media_is_url"` preprocess step.
// spec: run.bind.remote-url-unsupported@4a017929
#[test]
fn an_address_for_an_engine_reading_local_files_refuses_naming_the_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = source(dir.path(), json!([{"doc_id": "web", "path": "https://docs.example/memo.txt"}]), SRT_ENGINE);
    let rows = pulled(&mut s);
    let err = rows[0]["last_error"].as_str().unwrap();
    assert!(err.starts_with("DeriveRemoteUrlUnsupported") && err.contains("media_is_url"), "{err}");
}

#[test]
fn the_step_resolver_reads_a_bare_name_off_the_search_path() {
    let dir = tempfile::tempdir().unwrap();
    let b = binding("[derive.reader.engine]\ncommand = [\"cat\", \"{input}\"]\n");
    let step = resolve_step("reader", "engine", b.engine.as_ref().unwrap(), dir.path()).unwrap();
    assert!(step.binary.is_absolute() && step.binary.ends_with("cat"));
}
