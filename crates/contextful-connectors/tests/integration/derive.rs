//! The exec driver and the derive source.

use crate::support::{Never, Response, Server};
use contextful_connectors::derive::{resolve_step, run_chain, Chain, ChainError, DeriveSource};
use contextful_core::run::derive::config::{bindings, Binding, DeriveConfig};
use contextful_core::run::journal::sha256_hex;
use contextful_core::run::ports::{Cancellation, PullRequest, Row, Source, TableReader};
use contextful_core::run::{Failure, FailureTag, RunError};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

struct Admitted;

impl contextful_outbound::PreSendHook for Admitted {
    fn admit(&self, _: &contextful_outbound::Intent) -> Result<(), String> {
        Ok(())
    }

    fn settle(&self, _: &contextful_outbound::Intent, _: &contextful_outbound::Outcome) {}
}

fn binding(toml: &str) -> Binding {
    bindings(toml).unwrap().remove("reader").unwrap()
}

fn chain(dir: &Path, toml: &str) -> Result<Chain, RunError> {
    Chain::resolve("reader", &binding(toml), dir)
}

fn env_of(c: &Chain) -> Vec<(String, String)> {
    c.env.iter().map(|(k, t)| (k.clone(), t.render(|_| Ok::<_, ()>(contextful_core::connector::reference::Hydrated::new(""))).unwrap().reveal().to_string())).collect()
}

fn run_resolved(dir: &Path, c: &Chain, media: &str, cancel: &dyn Cancellation) -> Result<String, ChainError> {
    let scratch = tempfile::tempdir().unwrap();
    run_chain(c, &dir.join(media), &env_of(c), scratch.path(), cancel)
}

fn run(dir: &Path, toml: &str, media: &str) -> Result<String, RunError> {
    let c = chain(dir, toml)?;
    run_resolved(dir, &c, media, &Never).map_err(|e| match e {
        ChainError::Unit(e) => e,
        ChainError::Canceled => panic!("no stop was requested"),
        ChainError::Unavailable(why) => panic!("the engine is present: {why}"),
    })
}

/// A cancellation requested once `after` has passed.
struct StopAfter(std::time::Instant);

impl StopAfter {
    fn new(after: std::time::Duration) -> StopAfter {
        StopAfter(std::time::Instant::now() + after)
    }
}

impl Cancellation for StopAfter {
    fn requested(&self) -> bool {
        std::time::Instant::now() >= self.0
    }
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
    assert!(std::env::var_os("PATH").is_some(), "the test process carries a variable a step must not inherit");
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
    fn rows(&self, table: &str, columns: &[&str]) -> Result<Vec<Row>, Failure> {
        let project = |r: &Row| r.iter().filter(|(k, _)| columns.contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect();
        Ok(self.0.iter().find(|(t, _)| t == table).map(|(_, rs)| rs.iter().map(project).collect()).unwrap_or_default())
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
        output_schema: json!({}),
        reader: Box::new(Rows(vec![("documents".into(), parents)])),
        resolver: Arc::new(contextful_outbound::Resolver::new(vec![], false, Arc::new(contextful_core::ports::FixedClock(contextful_core::time::Instant::from_unix_secs(0).unwrap())))),
        mediation: contextful_connectors::http::Mediation { hook: Some(Arc::new(Admitted)), run_id: Some("derive-test".into()), ..Default::default() },
        store_root: Some(dir.to_path_buf()),
        cwd: dir.to_path_buf(),
    }
}

fn pull(s: &mut DeriveSource, cancel: &dyn Cancellation) -> Result<Vec<Value>, Failure> {
    let req = PullRequest { step_label: "pull-0".into(), position: None, idempotency_key: "k".into() };
    let v: Value = serde_json::from_slice(&s.pull(&req, cancel)?).unwrap();
    Ok(v["rows"].as_array().unwrap().clone())
}

fn pulled(s: &mut DeriveSource) -> Vec<Value> {
    pull(s, &Never).unwrap()
}

/// A derive source needs its store root and pipeline id before it reads a table.
// spec: run.select.no-store-root@1100fcff
#[test]
fn a_derive_source_refuses_an_absent_store_root_or_pipeline_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut source = source(dir.path(), json!([]), "[derive.reader.engine]\ncommand = [\"cat\", \"{input}\"]\n");
    source.pipeline_id.clear();
    let failure = pull(&mut source, &Never).unwrap_err();
    assert!(failure.message.contains("DeriveNoStoreRoot") && failure.message.contains("pipeline id"), "{failure:?}");

    source.pipeline_id = "doc-text".into();
    source.store_root = None;
    let failure = pull(&mut source, &Never).unwrap_err();
    assert!(failure.message.contains("DeriveNoStoreRoot") && failure.message.contains("store root"), "{failure:?}");

    source.store_root = Some(Path::new("").to_path_buf());
    let failure = pull(&mut source, &Never).unwrap_err();
    assert!(failure.message.contains("DeriveNoStoreRoot") && failure.message.contains("store root"), "{failure:?}");
}

/// A media resolution directory is independent of the source's store root.
#[test]
fn a_media_directory_does_not_decide_whether_the_store_root_exists() {
    let dir = tempfile::tempdir().unwrap();
    let mut source = source(dir.path(), json!([]), "[derive.reader.engine]\ncommand = [\"cat\", \"{input}\"]\n");
    source.cwd = Path::new("").to_path_buf();
    assert!(source.derivation().is_ok(), "the store root exists independently of media resolution");
}

/// Missing or blank parent keys and media values count as skipped inputs in the pull.
#[test]
fn incomplete_parent_rows_enter_the_derive_pull_skipped_count() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("memo.txt"), "Hello.\n").unwrap();
    let parents = json!([
        {"doc_id": null, "path": "memo.txt"},
        {"doc_id": "blank", "path": ""},
        {"doc_id": "space", "path": "  "},
        {"doc_id": "memo", "path": "memo.txt"}
    ]);
    let mut s = source(dir.path(), parents, SRT_ENGINE);
    let request = PullRequest { step_label: "pull-0".into(), position: None, idempotency_key: "k".into() };
    let pull = contextful_core::run::ports::Pull::decode(&s.pull(&request, &Never).unwrap()).unwrap();
    assert_eq!(pull.skipped, 3);
    assert_eq!(pull.rows.len(), 1);
    assert_eq!(pull.rows[0]["unit_ref"], "memo");
}

#[test]
fn a_link_preview_fetches_a_head_document_through_the_mediated_client() {
    let site = Server::start(|_| Response {
        status: 200,
        headers: vec![("Content-Type".into(), "text/html; charset=utf-8".into())],
        body: b"<html><head><title>Example article</title><meta name=\"description\" content=\"A short summary\"></head><body>ignored</body></html>".to_vec(),
    });
    let address = format!("http://localhost:{}/article?private=1", site.port);
    let dir = tempfile::tempdir().unwrap();
    let mut s = source(dir.path(), json!([{"doc_id": "article", "path": address}]), "[derive.reader]\ndriver = \"fetch\"\nallow_hosts = [\"localhost\"]\n");
    s.config.task = contextful_core::run::derive::config::Task::LinkPreview;
    let rows = pulled(&mut s);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["unit_status"], "ok");
    assert_eq!(rows[0]["title"], "Example article");
    assert_eq!(rows[0]["description"], "A short summary");
    assert_eq!(site.received("/article").len(), 1);
}

#[test]
fn a_link_preview_refuses_a_missing_mediation_hook_before_a_socket() {
    let site = Server::start(|_| Response::json(200, "{}"));
    let dir = tempfile::tempdir().unwrap();
    let address = format!("http://localhost:{}/article", site.port);
    let mut source = link_source(dir.path(), &address, "");
    source.mediation.hook = None;
    let failure = pull(&mut source, &Never).unwrap_err();
    assert!(failure.message.contains("DeriveMeteredClient"), "{failure:?}");
    assert!(site.received("/article").is_empty());
}

fn link_source(dir: &Path, address: &str, extra: &str) -> DeriveSource {
    let binding = format!("[derive.reader]\ndriver = \"fetch\"\nallow_hosts = [\"localhost\"]\n{extra}");
    let mut s = source(dir, json!([{"doc_id": "article", "path": address}]), &binding);
    s.config.task = contextful_core::run::derive::config::Task::LinkPreview;
    s
}

// spec: run.fetch.scheme@b63853e1
// spec: run.fetch.address-literal@9291e664
#[test]
fn a_link_preview_refuses_non_http_schemes_and_address_literals_before_a_socket() {
    let site = Server::start(|_| Response::json(200, "{}"));
    let dir = tempfile::tempdir().unwrap();
    for (address, error) in [
        ("file:///etc/passwd".to_string(), "DeriveSchemeUnsupported"),
        (site.url("/private"), "DeriveAddressLiteral"),
    ] {
        let rows = pulled(&mut link_source(dir.path(), &address, ""));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["unit_status"], "failed");
        assert!(rows[0]["last_error"].as_str().unwrap().contains(error), "{rows:?}");
    }
    assert!(site.received("/private").is_empty());
}

// spec: run.fetch.redirect-chain@d274af14
// spec: run.fetch.charset@71537e22
// spec: run.fetch.not-utf8@bcfcbecf
#[test]
fn a_link_preview_bounds_redirects_and_refuses_non_utf8_documents() {
    let site = Server::start(|r| {
        let n: usize = r.path().trim_start_matches('/').parse().unwrap_or(0);
        if n < 7 {
            Response { status: 302, headers: vec![("Location".into(), format!("/{}", n + 1))], body: Vec::new() }
        } else {
            Response { status: 200, headers: vec![("Content-Type".into(), "text/html; Charset=iso-8859-1".into())], body: b"<title>old</title>".to_vec() }
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let first = format!("http://localhost:{}/0", site.port);
    let rows = pulled(&mut link_source(dir.path(), &first, ""));
    assert!(rows[0]["last_error"].as_str().unwrap().contains("5 hops"), "{rows:?}");
    assert_eq!(site.requests.lock().unwrap().len(), 6, "initial request plus five hops");

    let charset = format!("http://localhost:{}/7", site.port);
    let rows = pulled(&mut link_source(dir.path(), &charset, ""));
    assert!(rows[0]["last_error"].as_str().unwrap().contains("DeriveCharsetUnsupported"), "{rows:?}");
    let invalid = Server::start(|_| Response { status: 200, headers: vec![], body: vec![0xff, 0xfe] });
    let address = format!("http://localhost:{}/invalid", invalid.port);
    let rows = pulled(&mut link_source(dir.path(), &address, ""));
    assert!(rows[0]["last_error"].as_str().unwrap().contains("DeriveBytesNotUtf8"), "{rows:?}");
}

// spec: run.fetch.retry-after-default@d278358c
#[test]
fn a_link_preview_uses_sixty_seconds_for_a_429_without_retry_after() {
    let site = Server::start(|_| Response { status: 429, headers: vec![], body: vec![] });
    let dir = tempfile::tempdir().unwrap();
    let address = format!("http://localhost:{}/limited", site.port);
    let failure = pull(&mut link_source(dir.path(), &address, ""), &Never).unwrap_err();
    assert_eq!(failure.tag, FailureTag::RateLimited);
    assert_eq!(failure.retry_after_secs, Some(60));
}

/// Each fetch hop has its own `request_timeout_secs` wall clock.
#[test]
fn a_link_preview_times_out_one_slow_hop_but_allows_two_short_hops() {
    let site = Server::start(|request| match request.path() {
        "/slow" => {
            std::thread::sleep(std::time::Duration::from_secs(2));
            Response { status: 200, headers: vec![], body: b"<title>Too late</title>".to_vec() }
        }
        "/redirect" => {
            std::thread::sleep(std::time::Duration::from_millis(650));
            Response { status: 302, headers: vec![("Location".into(), "/fast".into())], body: vec![] }
        }
        "/fast" => {
            std::thread::sleep(std::time::Duration::from_millis(650));
            Response { status: 200, headers: vec![], body: b"<title>Within each hop</title>".to_vec() }
        }
        _ => Response::json(404, "{}"),
    });
    let dir = tempfile::tempdir().unwrap();
    let address = format!("http://localhost:{}/slow", site.port);
    let started = std::time::Instant::now();
    let rows = pulled(&mut link_source(dir.path(), &address, "request_timeout_secs = 1\n"));
    assert_eq!(rows[0]["unit_status"], "failed", "{rows:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(2));

    let address = format!("http://localhost:{}/redirect", site.port);
    let rows = pulled(&mut link_source(dir.path(), &address, "request_timeout_secs = 1\n"));
    assert_eq!(rows[0]["title"], "Within each hop", "{rows:?}");
}

// spec: run.fetch.probe-prefix@4b8736f9
// spec: run.fetch.head-rows@ca1a4fd4
#[test]
fn a_link_preview_probes_advertised_images_with_a_bounded_range() {
    let site = Server::start(|r| match r.path() {
        "/article" => Response {
            status: 200,
            headers: vec![("Content-Type".into(), "text/html; charset=utf-8".into())],
            body: b"<head><title>Illustrated note</title><meta property=\"og:image\" content=\"/picture.jpg\"></head>".to_vec(),
        },
        "/picture.jpg" => Response { status: 206, headers: vec![("Content-Type".into(), "image/jpeg".into())], body: b"jpeg".to_vec() },
        _ => Response::json(404, "{}"),
    });
    let dir = tempfile::tempdir().unwrap();
    let address = format!("http://localhost:{}/article", site.port);
    let rows = pulled(&mut link_source(dir.path(), &address, "allow_image_hosts = [\"localhost\"]\n"));
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[1]["cue_seq"], 1);
    assert_eq!(rows[1]["image_url"], format!("http://localhost:{}/picture.jpg", site.port));
    assert_eq!(rows[0]["url"], address);
    assert_eq!(rows[1]["url"], address);
    assert_eq!(rows[1]["probe_status"], "ok");
    assert_eq!(rows[0]["_modality"], "text");
    assert_eq!(rows[1]["_modality"], "image");
    let probes = site.received("/picture.jpg");
    assert_eq!(probes.len(), 1);
    assert_eq!(probes[0].header("range"), Some("bytes=0-65535"));
}

// spec: run.fetch.document-prefix@1386432c
#[test]
fn a_link_preview_scans_a_megabyte_prefix_and_drops_the_remainder() {
    let site = Server::start(|_| {
        let mut body = b"<head><title>Within prefix</title></head>".to_vec();
        body.extend(vec![b'x'; 1024 * 1024 + 10]);
        Response { status: 200, headers: vec![("Content-Type".into(), "text/html".into())], body }
    });
    let dir = tempfile::tempdir().unwrap();
    let address = format!("http://localhost:{}/large", site.port);
    let rows = pulled(&mut link_source(dir.path(), &address, ""));
    assert_eq!(rows[0]["title"], "Within prefix", "{rows:?}");
    assert_eq!(rows[0]["unit_status"], "ok");
}

fn unit<'a>(rows: &'a [Value], key: &str) -> &'a Value {
    rows.iter().find(|r| r["unit_ref"] == key).unwrap_or_else(|| panic!("no row for `{key}` in {rows:?}"))
}

const SRT_ENGINE: &str = "[derive.reader.engine]\ncommand = [\"awk\", \"NF { n++; printf \\\"%d\\\\n00:00:0%d,000 --> 00:00:0%d,500\\\\n%s\\\\n\\\\n\\\", n, n, n, $0 }\", \"{input}\"]\noutput_format = \"srt\"\n";

/// A video transcript passage names its text output modality.
#[test]
fn a_transcript_passage_carries_text_modality() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("clip.txt"), "Spoken words.\n").unwrap();
    let rows = pulled(&mut source(dir.path(), json!([{"doc_id": "clip", "path": "clip.txt"}]), SRT_ENGINE));
    assert_eq!(rows[0]["text"], "Spoken words.");
    assert_eq!(rows[0]["_modality"], "text");
}

/// Changing canonical file bytes under one media path selects a new derivation key.
#[test]
fn changed_local_bytes_reselect_the_same_media_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clip.txt");
    std::fs::write(&path, "First words.\n").unwrap();
    let parents = json!([{"doc_id": "clip", "path": "clip.txt"}]);
    let first = pulled(&mut source(dir.path(), parents.clone(), SRT_ENGINE));
    let first_key = first[0]["derivation_key"].as_str().unwrap().to_string();
    std::fs::write(&path, "Second words.\n").unwrap();
    let mut next = source(dir.path(), parents, SRT_ENGINE);
    let landed: Vec<Row> = first.into_iter().map(|r| r.as_object().unwrap().clone()).collect();
    next.reader = Box::new(Rows(vec![("documents".into(), vec![json!({"doc_id": "clip", "path": "clip.txt"}).as_object().unwrap().clone()]), ("doc_text_passages".into(), landed)]));
    let rows = pulled(&mut next);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["text"], "Second words.");
    assert_ne!(rows[0]["derivation_key"], first_key);
}

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

/// A preprocess step runs only while its `when` holds: `media_is_url` for an `http` or `https` input,
/// `engine_requires_pcm16_wav` for an input other than 16-bit PCM WAV; another name raises
/// `DeriveStepConditionUnknown`.
// spec: run.exec.step-condition@726bdffa
#[test]
fn a_preprocess_step_runs_only_while_its_condition_holds() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("doc.txt"), "x").unwrap();
    // A 16-bit PCM WAV header: RIFF, WAVE, a `fmt ` chunk of format 1 and 16 bits per sample.
    let mut wav = b"RIFF\x24\x00\x00\x00WAVEfmt \x10\x00\x00\x00\x01\x00\x01\x00\x80\x3e\x00\x00\x00\x7d\x00\x00\x02\x00\x10\x00data\x00\x00\x00\x00".to_vec();
    std::fs::write(dir.path().join("clip.wav"), &wav).unwrap();
    wav[34] = 0x08;
    std::fs::write(dir.path().join("clip8.wav"), &wav).unwrap();
    let echo_input = "[derive.reader.engine]\ncommand = [\"printf\", \"%s\", \"{input}\"]\n";
    let convert = format!("[[derive.reader.preprocess]]\nwhen = \"engine_requires_pcm16_wav\"\ncommand = [\"cp\", \"{{input}}\", \"{{output}}\"]\noutput_path = \"{{output_stem}}.wav\"\n{echo_input}");
    assert!(run(dir.path(), &convert, "doc.txt").unwrap().ends_with("step-0.wav"), "a text input is converted");
    assert!(run(dir.path(), &convert, "clip8.wav").unwrap().ends_with("step-0.wav"), "8-bit audio is converted");
    assert!(run(dir.path(), &convert, "clip.wav").unwrap().ends_with("clip.wav"), "16-bit PCM passes through unconverted");
    let fetch = format!("[[derive.reader.preprocess]]\nwhen = \"media_is_url\"\ncommand = [\"false\"]\n{echo_input}");
    assert!(run(dir.path(), &fetch, "doc.txt").unwrap().ends_with("doc.txt"), "a local input skips the fetch step");
    let typo = format!("[[derive.reader.preprocess]]\nwhen = \"media_is_uri\"\ncommand = [\"true\"]\n{echo_input}");
    match chain(dir.path(), &typo) {
        Err(RunError::DeriveStepConditionUnknown(m)) => assert!(m.contains("media_is_uri") && m.contains("preprocess-0"), "{m}"),
        other => panic!("{:?}", other.err()),
    }
}

/// A local media value resolves, canonicalized, under the binding's `media_root`, the working directory by
/// default; a path escaping it raises `DeriveMediaOutsideRoot`, failing that unit alone.
// spec: run.bind.media-root@8ff61916
#[test]
fn media_outside_the_root_fails_that_unit_alone() {
    let top = tempfile::tempdir().unwrap();
    let project = top.path().join("project");
    let media = top.path().join("media");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&media).unwrap();
    std::fs::write(top.path().join("secret.txt"), "Private key.\n").unwrap();
    std::fs::write(project.join("memo.txt"), "Hello there.\n").unwrap();
    std::fs::write(media.join("clip.txt"), "From the root.\n").unwrap();
    std::os::unix::fs::symlink(top.path().join("secret.txt"), project.join("link.txt")).unwrap();
    let secret = top.path().join("secret.txt").to_string_lossy().into_owned();
    let parents = json!([
        {"doc_id": "dotdot", "path": "../secret.txt"}, {"doc_id": "absolute", "path": secret},
        {"doc_id": "link", "path": "link.txt"}, {"doc_id": "memo", "path": "memo.txt"}
    ]);
    let rows = pulled(&mut source(&project, parents, SRT_ENGINE));
    for key in ["dotdot", "absolute", "link"] {
        let r = unit(&rows, key);
        assert_eq!(r["unit_status"], "failed", "{r}");
        assert!(r["last_error"].as_str().unwrap().starts_with("DeriveMediaOutsideRoot"), "{r}");
        assert!(!rows.is_empty(), "the exclusion below ranges over no element");
        assert!(!rows.iter().any(|r| r["text"] == "Private key."), "{rows:?}");
    }
    assert_eq!(unit(&rows, "memo")["text"], "Hello there.");
    let rooted = format!("[derive.reader]\nmedia_root = \"../media\"\n{SRT_ENGINE}");
    let rows = pulled(&mut source(&project, json!([{"doc_id": "clip", "path": "clip.txt"}, {"doc_id": "memo", "path": "memo.txt"}]), &rooted));
    assert_eq!(unit(&rows, "clip")["text"], "From the root.");
    assert!(unit(&rows, "memo")["last_error"].as_str().unwrap().starts_with("DeriveMediaUnreadable"), "{rows:?}");
}

/// A unit whose chain a run stop interrupts lands no row and charges no attempt; the pull ends `Canceled` once
/// {{run.cancel.child-reaped}}.
// spec: run.emit.canceled-unit@1e635838
#[test]
fn a_stopped_chain_lands_no_row_and_the_pull_ends_canceled() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("doc.txt"), "x").unwrap();
    let slow = "[derive.reader.engine]\ncommand = [\"sleep\", \"30\"]\n";
    let started = std::time::Instant::now();
    let c = chain(dir.path(), slow).unwrap();
    assert!(matches!(run_resolved(dir.path(), &c, "doc.txt", &StopAfter::new(std::time::Duration::from_millis(200))), Err(ChainError::Canceled)));
    let mut s = source(dir.path(), json!([{"doc_id": "doc", "path": "doc.txt"}]), slow);
    let failure = pull(&mut s, &StopAfter::new(std::time::Duration::from_millis(200))).unwrap_err();
    assert_eq!(failure.tag, FailureTag::Canceled, "{failure:?}");
    assert!(started.elapsed().as_secs() < 10);
}

/// Each spawn re-reads its step's binary and runs it only while those bytes match the digest resolved at run
/// start, else {{run.exec.digest-mismatch}}.
// spec: run.exec.verified-spawn@f4827a6e
#[test]
fn a_binary_replaced_after_resolution_never_runs() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("doc.txt"), "x").unwrap();
    let digest = script(dir.path(), "engine.sh", "cat \"$1\"");
    let c = chain(dir.path(), &format!("[derive.reader.engine]\ncommand = [\"./engine.sh\", \"{{input}}\"]\nsha256 = \"{digest}\"\n")).unwrap();
    assert_eq!(run_resolved(dir.path(), &c, "doc.txt", &Never).unwrap(), "x");
    script(dir.path(), "engine.sh", "echo tampered > \"$1\".owned");
    match run_resolved(dir.path(), &c, "doc.txt", &Never) {
        Err(ChainError::Unit(RunError::DeriveDigestMismatch(m))) => assert!(m.contains("engine"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(!dir.path().join("doc.txt.owned").exists(), "the replaced binary never ran");
}

/// The captured-output bound holds over standard output and error together, and after the step exits.
#[test]
fn the_bound_holds_over_both_streams_together_and_after_the_step_exits() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("doc.txt"), "x").unwrap();
    let both = script(dir.path(), "both.sh", "head -c 600 /dev/zero | tr '\\0' a\nhead -c 600 /dev/zero | tr '\\0' b >&2");
    match run(dir.path(), &format!("[derive.reader]\nmax_output_bytes = 1000\n[derive.reader.engine]\ncommand = [\"./both.sh\"]\nsha256 = \"{both}\"\n"), "doc.txt") {
        Err(RunError::DeriveOutputCap(m)) => assert!(m.contains("1200"), "{m}"),
        other => panic!("{other:?}"),
    }
    for _ in 0..20 {
        let quick = run(dir.path(), "[derive.reader]\nmax_output_bytes = 1000\n[derive.reader.engine]\ncommand = [\"head\", \"-c\", \"1001\", \"/dev/zero\"]\n", "doc.txt");
        assert!(matches!(quick, Err(RunError::DeriveOutputCap(_))), "{quick:?}");
    }
}

/// An engine printing nothing lands a retryable `unavailable` marker; a cue-free WebVTT document lands `empty`.
#[test]
fn silence_lands_unavailable_and_a_cue_free_webvtt_lands_empty() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("doc.txt"), "x").unwrap();
    let parents = json!([{"doc_id": "doc", "path": "doc.txt"}]);
    let silent = pulled(&mut source(dir.path(), parents.clone(), "[derive.reader.engine]\ncommand = [\"true\"]\n"));
    assert_eq!((silent[0]["unit_status"].as_str(), silent[0]["retryable"].as_bool(), silent[0]["attempts"].as_i64()), (Some("unavailable"), Some(true), Some(1)));
    let empty = pulled(&mut source(dir.path(), parents, "[derive.reader.engine]\ncommand = [\"printf\", \"WEBVTT\\n\"]\n"));
    assert_eq!((empty[0]["unit_status"].as_str(), empty[0]["retryable"].as_bool()), (Some("empty"), Some(false)));
}

/// A parent table, and an output table whose rows appear from its second read on, as a concurrent tick landing
/// them while this one derives.
struct Racing {
    parents: Vec<Row>,
    landed: Vec<Row>,
    output_reads: std::sync::Mutex<usize>,
}

impl TableReader for Racing {
    fn rows(&self, table: &str, _columns: &[&str]) -> Result<Vec<Row>, Failure> {
        if table == "documents" {
            return Ok(self.parents.clone());
        }
        let mut reads = self.output_reads.lock().unwrap();
        *reads += 1;
        Ok(if *reads > 1 { self.landed.clone() } else { Vec::new() })
    }
}

/// A unit that settles under its current key while a tick derives it raises `DeriveSettledUnitRevived`, and that
/// tick lands none of its rows; a changed key revives nothing.
// spec: run.emit.settled-revived@29ec5fda
#[test]
fn a_unit_settled_by_a_concurrent_tick_lands_none_of_its_rows() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("memo.txt"), "Hello there.\n").unwrap();
    std::fs::write(dir.path().join("brief.txt"), "Filing is due.\n").unwrap();
    let parents = json!([{"doc_id": "memo", "path": "memo.txt"}, {"doc_id": "brief", "path": "brief.txt"}]);
    let mut s = source(dir.path(), parents.clone(), SRT_ENGINE);
    let memo_key = pulled(&mut source(dir.path(), json!([{"doc_id": "memo", "path": "memo.txt"}]), SRT_ENGINE))[0]["derivation_key"].as_str().unwrap().to_string();
    let brief_key = pulled(&mut source(dir.path(), json!([{"doc_id": "brief", "path": "brief.txt"}]), SRT_ENGINE))[0]["derivation_key"].as_str().unwrap().to_string();
    let settled = |unit: &str, key: String| {
        json!({"unit_ref": unit, "cue_seq": 0, "kind": "passage", "derivation_key": key, "_ingested_at": "2030-01-01T00:00:00.000000000Z", "_run_id": "other", "_row_seq": 0})
            .as_object()
            .unwrap()
            .clone()
    };
    let parent_rows: Vec<Row> = parents.as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect();
    s.reader = Box::new(Racing { parents: parent_rows.clone(), landed: vec![settled("memo", memo_key)], output_reads: Default::default() });
    let rows = pulled(&mut s);
    assert!(!rows.is_empty(), "the exclusion below ranges over no element");
    assert!(rows.iter().all(|r| r["unit_ref"] != "memo"), "the revived unit lands nothing: {rows:?}");
    assert_eq!(unit(&rows, "brief")["text"], "Filing is due.");
    assert_eq!(unit(&rows, "brief")["derivation_key"], brief_key);
    // A concurrent landing under another key settles nothing under this one.
    s.reader = Box::new(Racing { parents: parent_rows, landed: vec![settled("memo", "0".repeat(64))], output_reads: Default::default() });
    assert_eq!(unit(&pulled(&mut s), "memo")["text"], "Hello there.");
}

/// A transcriber whose engine is gone mid-run ends the run; a link reader that cannot reach its site costs that one
/// unit a `failed`, retryable marker.
#[test]
fn an_unavailable_transcriber_ends_the_run_and_an_unreachable_link_costs_one_unit() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("memo.txt"), "Hello there.\n").unwrap();
    std::fs::write(dir.path().join("brief.txt"), "Filing is due.\n").unwrap();
    let digest = script(dir.path(), "engine.sh", "cat \"$1\"\nrm -f \"$0\"");
    let engine = format!("[derive.reader.engine]\ncommand = [\"./engine.sh\", \"{{input}}\"]\nsha256 = \"{digest}\"\n");
    let parents = json!([{"doc_id": "memo", "path": "memo.txt"}, {"doc_id": "brief", "path": "brief.txt"}]);
    let failure = pull(&mut source(dir.path(), parents, &engine), &Never).unwrap_err();
    assert_eq!(failure.tag, FailureTag::Transient);
    assert!(failure.message.contains("EngineUnavailable") && failure.message.contains("brief"), "{failure:?}");

    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let address = format!("http://localhost:{closed}/gone");
    let rows = pulled(&mut link_source(dir.path(), &address, ""));
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["unit_status"], "failed");
    assert_eq!(rows[0]["attempts"], 1);
    assert_eq!(rows[0]["retryable"], true);
    assert!(rows[0]["last_error"].as_str().unwrap().starts_with("DeriveLinkEngineUnavailable"), "{rows:?}");
}

/// A site answering a non-success status lands its unit's error with at most 4 KiB of the response.
#[test]
fn a_failing_site_answer_lands_a_bounded_upstream_excerpt() {
    let site = Server::start(|_| Response { status: 503, headers: vec![], body: format!("overloaded {}", "z".repeat(10 * 1024)).into_bytes() });
    let dir = tempfile::tempdir().unwrap();
    let address = format!("http://localhost:{}/busy", site.port);
    let rows = pulled(&mut link_source(dir.path(), &address, ""));
    let error = rows[0]["last_error"].as_str().unwrap();
    assert!(error.contains("upstream answered 503: overloaded"), "{error}");
    assert!(error.len() < 4096 + 128, "{}", error.len());
}

fn full_pull(s: &mut DeriveSource) -> contextful_core::run::ports::Pull {
    let request = PullRequest { step_label: "pull-0".into(), position: None, idempotency_key: "k".into() };
    contextful_core::run::ports::Pull::decode(&s.pull(&request, &Never).unwrap()).unwrap()
}

/// A chain contributes at most 64 entries of captured output to the run record.
// spec: run.exec.audit-entries@e41d7d98
#[test]
fn a_chain_adds_at_most_64_captured_output_entries_to_the_run_record() {
    let dir = tempfile::tempdir().unwrap();
    let mut parents = Vec::new();
    for i in 0..70 {
        std::fs::write(dir.path().join(format!("doc{i:02}.txt")), "x").unwrap();
        parents.push(json!({"doc_id": format!("doc{i:02}"), "path": format!("doc{i:02}.txt")}));
    }
    let digest = script(dir.path(), "engine.sh", "echo \"warming up for $1\" >&2\ncat \"$1\"");
    let engine = format!("[derive.reader.engine]\ncommand = [\"./engine.sh\", \"{{input}}\"]\nsha256 = \"{digest}\"\n");
    let mut s = source(dir.path(), json!(parents), &engine);
    s.config.max_rows_per_run = 70;
    let pull = full_pull(&mut s);
    assert_eq!(pull.rows.len(), 70, "every unit lands");
    assert_eq!(pull.audit.len(), 64, "{:?}", pull.audit);
    assert!(pull.audit[0].starts_with("doc00: step `engine`: warming up for"), "{}", pull.audit[0]);
    assert!(pull.audit[63].starts_with("doc63: "), "{}", pull.audit[63]);

    // A chain writing no standard error adds no entry.
    let quiet = format!("[derive.reader.engine]\ncommand = [\"cat\", \"{{input}}\"]\n");
    let mut s = source(dir.path(), json!([{"doc_id": "doc00", "path": "doc00.txt"}]), &quiet);
    assert!(full_pull(&mut s).audit.is_empty());
}

/// A step exiting non-zero raises `DeriveStepExit` carrying its bounded error text into the run audit.
// spec: run.exec.non-zero-exit@bfb1a94e
#[test]
fn a_step_exiting_non_zero_carries_its_error_text_into_the_run_audit() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("memo.txt"), "x").unwrap();
    let digest = script(dir.path(), "engine.sh", "echo \"boom with $VENDOR_KEY\" >&2\nexit 3");
    let engine = format!(
        "[derive.reader.engine]\ncommand = [\"./engine.sh\", \"{{input}}\"]\nsha256 = \"{digest}\"\n[derive.reader.env]\nVENDOR_KEY = \"sk-literal-vendor-value\"\n"
    );
    let pull = full_pull(&mut source(dir.path(), json!([{"doc_id": "memo", "path": "memo.txt"}]), &engine));
    let marker = &pull.rows[0];
    assert_eq!(marker["unit_status"], "failed");
    let error = marker["last_error"].as_str().unwrap();
    assert!(error.starts_with("DeriveStepExit") && error.contains("boom with") && error.contains("exit status: 3"), "{error}");
    assert_eq!(pull.audit.len(), 1, "{:?}", pull.audit);
    assert!(pull.audit[0].starts_with("memo: DeriveStepExit") && pull.audit[0].contains("boom with"), "{:?}", pull.audit);
    for text in [error, pull.audit[0].as_str()] {
        assert!(!text.contains("sk-literal-vendor-value"), "an environment value never reaches the audit: {text}");
    }
}

/// A vendor engine, a binding naming an `endpoint_host`, holds `request_timeout_secs` as its engine step's deadline,
/// clipped to the time {{run.exec.chain-deadline}} leaves, so a stalled vendor fails its unit before the chain elapses.
// spec: run.exec.vendor-deadline@5bf89e80
#[test]
fn a_stalled_vendor_fails_its_unit_at_its_own_deadline() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("doc.txt"), "x").unwrap();
    let started = std::time::Instant::now();
    let vendor = "[derive.reader]\ntimeout_secs = 60\nendpoint_host = \"api.speech.example\"\nrequest_timeout_secs = 1\n[derive.reader.engine]\ncommand = [\"sleep\", \"30\"]\n";
    match run(dir.path(), vendor, "doc.txt") {
        Err(RunError::DeriveStepTimeout(m)) => assert!(m.contains("`engine`"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(started.elapsed().as_secs() < 10, "{:?}", started.elapsed());
    // The chain's remaining time clips a longer vendor deadline.
    let started = std::time::Instant::now();
    let clipped = "[derive.reader]\ntimeout_secs = 1\nendpoint_host = \"api.speech.example\"\nrequest_timeout_secs = 600\n[derive.reader.engine]\ncommand = [\"sleep\", \"30\"]\n";
    assert!(matches!(run(dir.path(), clipped, "doc.txt"), Err(RunError::DeriveStepTimeout(_))));
    assert!(started.elapsed().as_secs() < 10);
    // A binding reaching no vendor holds the chain deadline alone.
    let local = "[derive.reader]\nrequest_timeout_secs = 1\n[derive.reader.engine]\ncommand = [\"sh\", \"-c\", \"sleep 2; cat \\\"$0\\\"\", \"{input}\"]\n";
    assert_eq!(run(dir.path(), local, "doc.txt").unwrap(), "x");
}

/// A reader that counts `count` parent rows and serves them whole or in two batches, tallying which read ran.
struct Counted {
    parents: Vec<Row>,
    count: u64,
    whole_reads: Arc<std::sync::atomic::AtomicU32>,
    batch_reads: Arc<std::sync::atomic::AtomicU32>,
}

impl TableReader for Counted {
    fn rows(&self, table: &str, _columns: &[&str]) -> Result<Vec<Row>, Failure> {
        if table == "documents" {
            self.whole_reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            return Ok(self.parents.clone());
        }
        Ok(Vec::new())
    }

    fn row_count(&self, _table: &str) -> Result<Option<u64>, Failure> {
        Ok(Some(self.count))
    }

    fn row_batches(&self, _table: &str, _columns: &[&str], each: &mut dyn FnMut(Vec<Row>) -> Result<(), Failure>) -> Result<(), Failure> {
        self.batch_reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (first, second) = self.parents.split_at(2);
        each(first.to_vec())?;
        each(second.to_vec())
    }
}

/// A parent table holding at most 1000000 rows is selected by an in-memory scan; a larger one streams through the
/// read engine in batches, so selection holds one batch in memory.
// spec: run.select.parent-scan@0dc2f678
#[test]
fn a_parent_table_past_a_million_rows_streams_in_batches() {
    let dir = tempfile::tempdir().unwrap();
    let mut parents: Vec<Row> = (0..4).map(|i| json!({"doc_id": format!("d{i}"), "path": format!("d{i}.txt")}).as_object().unwrap().clone()).collect();
    // A key repeated across batches counts once.
    parents.push(parents[0].clone());
    for (count, streamed) in [(1_000_000, false), (1_000_001, true)] {
        let (whole, batched) = (Arc::new(std::sync::atomic::AtomicU32::new(0)), Arc::new(std::sync::atomic::AtomicU32::new(0)));
        let mut s = source(dir.path(), json!([]), "[derive.reader.engine]\ncommand = [\"cat\", \"{input}\"]\n");
        s.config.max_rows_per_run = 3;
        s.reader = Box::new(Counted { parents: parents.clone(), count, whole_reads: whole.clone(), batch_reads: batched.clone() });
        let counts = s.plan().unwrap();
        assert_eq!((counts.eligible, counts.derived, counts.outstanding), (4, 0, 4), "{count} rows");
        let reads = (whole.load(std::sync::atomic::Ordering::SeqCst), batched.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(reads, if streamed { (0, 1) } else { (1, 0) }, "{count} rows");
    }
}
