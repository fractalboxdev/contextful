//! `contextful visibility sweep` through the built command (`disclosure.sweep.sweep-verb`).
#![cfg(feature = "data-plane")]

use serde_json::json;
use std::path::Path;
use std::process::{Command, Output};

const MANIFEST: &str = r#"
authoring_posture = "per_request"
[[pipeline.tables]]
name = "wiki/pages"

[pipeline.tables.visibility]
source            = "wiki"
resource_key      = "page_id"
resource_kind     = "page"
fidelity          = "mirrored"
family            = "item-exception"
max_acl_staleness = "15m"

[[acl_sweep]]
source      = "wiki"
enumerate   = "pages"
full        = { every = "1h" }
incremental = { every = "5m" }
"#;

fn cf(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful")).current_dir(dir).args(args).output().unwrap()
}

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn lines(values: &[serde_json::Value]) -> String {
    values.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("\n")
}

fn sweep(dir: &Path, kind: &str) -> Output {
    cf(dir, &["visibility", "sweep", "--project", "wiki", "--source", "wiki", "--kind", kind, "--grants", "grants.jsonl"])
}

/// `visibility sweep --grants <file>` commits the file's read of a declared source as one run.
#[test]
fn a_sweep_of_a_declared_source_commits_the_grants_file_as_one_run() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join("contextful.toml"), MANIFEST).unwrap();
    std::fs::write(d.join("pages.jsonl"), lines(&[json!({"page_id": "p1", "title": "Roadmap"}), json!({"page_id": "p2", "title": "Payroll"})])).unwrap();
    ok(&cf(d, &["context", "land", "wiki/pages", "--project", "wiki", "--rows", "pages.jsonl", "--run-id", "run-0001", "--site-id", "site-a"]));

    let grant = |r: &str| json!({"resource_id": r, "principal": "dana@acme.example", "principal_kind": "user", "level": "viewer"});
    std::fs::write(d.join("grants.jsonl"), lines(&[grant("p1"), grant("p2")])).unwrap();
    let out = ok(&sweep(d, "full"));
    assert!(out.contains("wiki: full sweep") && out.contains("epoch 1") && out.contains("2 grants"), "{out}");

    // One run: every access table the sweep landed carries the same run directory.
    let files = |t: &str| ok(&cf(d, &["context", "files", t, "--project", "wiki"]));
    let run_of = |listing: &str| listing.lines().next().and_then(|l| l.split('/').find(|s| s.starts_with("sweep-"))).map(str::to_string);
    let run = run_of(&files("_visibility/wiki/freshness")).expect("freshness run");
    for t in ["resources", "grants", "principals"] {
        assert_eq!(run_of(&files(&format!("_visibility/wiki/{t}"))).as_deref(), Some(run.as_str()), "{t}");
    }

    std::fs::write(d.join("grants.jsonl"), lines(&[grant("p1")])).unwrap();
    let out = ok(&sweep(d, "incremental"));
    assert!(out.contains("incremental sweep") && out.contains("epoch 2") && out.contains("1 tombstones"), "{out}");

    // An undeclared source, a missing grants file and an orphan grant refuse.
    let undeclared = cf(d, &["visibility", "sweep", "--project", "wiki", "--source", "drive", "--kind", "full", "--grants", "grants.jsonl"]);
    assert!(!undeclared.status.success());
    assert!(String::from_utf8_lossy(&undeclared.stderr).contains("drive"));
    let endpoint = cf(d, &["visibility", "sweep", "--project", "wiki", "--source", "wiki", "--kind", "full"]);
    assert!(!endpoint.status.success());
    assert!(String::from_utf8_lossy(&endpoint.stderr).contains("--grants"));
    std::fs::write(d.join("grants.jsonl"), lines(&[grant("p9")])).unwrap();
    let orphan = sweep(d, "full");
    assert!(!orphan.status.success());
    assert!(String::from_utf8_lossy(&orphan.stderr).contains("VisibilityOrphanGrant"), "{}", String::from_utf8_lossy(&orphan.stderr));
}

/// `pipeline validate` refuses a sweep cadence outside the budget grammar.
#[test]
fn pipeline_validate_refuses_a_malformed_cadence() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("contextful.toml"), MANIFEST.replace("\"5m\"", "\"5m30s\"")).unwrap();
    let out = cf(dir.path(), &["pipeline", "validate"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("VisibilityBudgetMalformed") && stderr.contains("5m30s"), "{stderr}");
}

const SWEEP: &str = "[[acl_sweep]]\nsource = \"wiki\"\nenumerate = \"pages\"\nfull = { every = \"1h\" }\nincremental = { every = \"5m\" }\n";

/// A sweep block under `pipelines/` is the sweep both commands see; a source declared in two
/// files refuses in both.
#[test]
fn a_sweep_block_under_pipelines_is_one_validate_and_sweep_both_read() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let (root, block) = MANIFEST.split_once("[[acl_sweep]]").unwrap();
    assert!(block.contains("enumerate"));
    std::fs::write(d.join("contextful.toml"), root).unwrap();
    std::fs::create_dir(d.join("pipelines")).unwrap();
    std::fs::write(d.join("pipelines/wiki.toml"), SWEEP).unwrap();
    ok(&cf(d, &["pipeline", "validate"]));
    std::fs::write(d.join("pages.jsonl"), lines(&[json!({"page_id": "p1"})])).unwrap();
    ok(&cf(d, &["context", "land", "wiki/pages", "--project", "wiki", "--rows", "pages.jsonl", "--run-id", "run-0001", "--site-id", "site-a"]));
    std::fs::write(d.join("grants.jsonl"), lines(&[json!({"resource_id": "p1", "principal": "dana", "principal_kind": "user", "level": "viewer"})])).unwrap();
    let out = ok(&sweep(d, "full"));
    assert!(out.contains("epoch 1"), "{out}");

    std::fs::write(d.join("pipelines/drive.toml"), SWEEP).unwrap();
    for out in [cf(d, &["pipeline", "validate"]), sweep(d, "full")] {
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("second sweep") && stderr.contains("wiki"), "{stderr}");
    }
}
