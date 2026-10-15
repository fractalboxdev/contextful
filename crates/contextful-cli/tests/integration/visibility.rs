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

/// `visibility sweep --directory <file>` lands the identity provider's links and memberships
/// in the same run as the grants, and a later run without the flag keeps them.
#[test]
fn a_directory_file_lands_identity_links_and_group_members_in_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join("contextful.toml"), MANIFEST).unwrap();
    std::fs::write(d.join("pages.jsonl"), lines(&[json!({"page_id": "p1"})])).unwrap();
    ok(&cf(d, &["context", "land", "wiki/pages", "--project", "wiki", "--rows", "pages.jsonl", "--run-id", "run-0001", "--site-id", "site-a"]));
    std::fs::write(d.join("grants.jsonl"), lines(&[json!({"resource_id": "p1", "principal": "g-eng", "principal_kind": "group", "level": "viewer"})])).unwrap();
    std::fs::write(
        d.join("directory.jsonl"),
        lines(&[
            json!({"source_principal": "w-17", "subject": "user://dana@acme.example", "method": "scim_email", "confidence": 1.0}),
            json!({"group": "g-eng", "member": "w-17", "member_kind": "user"}),
        ]),
    )
    .unwrap();
    let swept = |extra: &[&str]| {
        let mut args = vec!["visibility", "sweep", "--project", "wiki", "--source", "wiki", "--kind", "full", "--grants", "grants.jsonl"];
        args.extend_from_slice(extra);
        cf(d, &args)
    };
    let out = ok(&swept(&["--directory", "directory.jsonl"]));
    assert!(out.contains("1 group members") && out.contains("1 identity links"), "{out}");

    let files = |t: &str| ok(&cf(d, &["context", "files", t, "--project", "wiki"]));
    let run_of = |listing: &str| listing.lines().next().and_then(|l| l.split('/').find(|s| s.starts_with("sweep-"))).map(str::to_string);
    let run = run_of(&files("_visibility/wiki/freshness")).expect("freshness run");
    for t in ["group_members", "identity_links"] {
        assert_eq!(run_of(&files(&format!("_visibility/wiki/{t}"))).as_deref(), Some(run.as_str()), "{t}");
    }

    let out = ok(&swept(&[]));
    assert!(out.contains("epoch 2") && out.contains("1 group members") && out.contains("1 identity links"), "{out}");

    std::fs::write(d.join("directory.jsonl"), lines(&[json!({"source_principal": "w-17", "subject": "user://dana@acme.example", "method": "scim_email"})])).unwrap();
    let bad = swept(&["--directory", "directory.jsonl"]);
    assert!(!bad.status.success());
    let stderr = String::from_utf8_lossy(&bad.stderr);
    assert!(stderr.contains("directory file line 1"), "{stderr}");
}

/// A binding at a servable level lacking a declared sweep for its source or a `max_acl_staleness` raises `VisibilityBindingIncomplete`.
// spec: disclosure.mirror.incomplete-binding@8ffc1120
#[test]
fn pipeline_validate_refuses_a_servable_binding_without_a_sweep_or_a_budget() {
    let (root, _) = MANIFEST.split_once("[[acl_sweep]]").unwrap();
    for (manifest, missing) in [
        (root.to_string(), "no `[[acl_sweep]]` block"),
        (MANIFEST.replace("max_acl_staleness = \"15m\"\n", ""), "no `max_acl_staleness`"),
        (format!("{root}{SWEEP}").replace("fidelity          = \"mirrored\"", "fidelity          = \"coarse\"").replace("max_acl_staleness = \"15m\"\n", ""), "no `max_acl_staleness`"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("contextful.toml"), &manifest).unwrap();
        let out = cf(dir.path(), &["pipeline", "validate"]);
        assert!(!out.status.success(), "{manifest}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("VisibilityBindingIncomplete") && stderr.contains("wiki/pages") && stderr.contains(missing), "{stderr}");
    }
    // A federated binding is served from no store and needs neither.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("contextful.toml"), root.replace("\"mirrored\"", "\"federated\"").replace("max_acl_staleness = \"15m\"\n", "")).unwrap();
    ok(&cf(dir.path(), &["pipeline", "validate"]));
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("contextful.toml"), MANIFEST).unwrap();
    ok(&cf(dir.path(), &["pipeline", "validate"]));
}

fn land_pages(d: &Path, ids: &[&str]) {
    let rows: Vec<serde_json::Value> = ids.iter().map(|id| json!({"page_id": id})).collect();
    std::fs::write(d.join("pages.jsonl"), lines(&rows)).unwrap();
    ok(&cf(d, &["context", "land", "wiki/pages", "--project", "wiki", "--rows", "pages.jsonl", "--run-id", "run-0001", "--site-id", "site-a"]));
}

/// The `watermark_at` a sweep's summary line reports.
fn watermark(out: &str) -> String {
    out.rsplit("watermark_at ").next().unwrap().trim().to_string()
}

/// A full sweep whose grants file marks a governed resource unread refuses with
/// `VisibilityWatermarkUncovered` and commits nothing; the next run finds the watermark where
/// the last covering run left it.
#[test]
fn an_unread_resource_refuses_a_full_sweep_and_the_watermark_holds() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join("contextful.toml"), MANIFEST).unwrap();
    land_pages(d, &["p1", "p2"]);
    let grant = |r: &str| json!({"resource_id": r, "principal": "dana", "principal_kind": "user", "level": "viewer"});
    std::fs::write(d.join("grants.jsonl"), lines(&[grant("p1"), grant("p2")])).unwrap();
    let first = ok(&sweep(d, "full"));
    let held = watermark(&first);
    assert_ne!(held, "unset");

    std::fs::write(d.join("grants.jsonl"), lines(&[grant("p1"), json!({"resource_id": "p2", "unread": true})])).unwrap();
    let refused = sweep(d, "full");
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("VisibilityWatermarkUncovered") && stderr.contains("`p2`"), "{stderr}");

    // The incremental run reading the same partial file commits epoch 2 over the first run's watermark.
    let inc = ok(&sweep(d, "incremental"));
    assert!(inc.contains("epoch 2"), "{inc}");
    assert_eq!(watermark(&inc), held);
}

const EVENTS: &str = "events      = { stream = \"wiki.permissions\", gap_detectable = true }\n";

/// `visibility sweep --kind webhook` applies a delivery of a gap-detectable stream and moves
/// the watermark; a source whose stream is not declared gap-detectable refuses.
#[test]
fn a_webhook_sweep_needs_a_gap_detectable_stream() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join("contextful.toml"), MANIFEST).unwrap();
    land_pages(d, &["p1", "p2"]);
    let grant = |r: &str, p: &str| json!({"resource_id": r, "principal": p, "principal_kind": "user", "level": "viewer"});
    std::fs::write(d.join("grants.jsonl"), lines(&[grant("p1", "dana"), grant("p2", "dana")])).unwrap();
    ok(&sweep(d, "full"));

    std::fs::write(d.join("grants.jsonl"), lines(&[grant("p2", "erin")])).unwrap();
    let refused = sweep(d, "webhook");
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("VisibilityUngappedStream") && stderr.contains("wiki"), "{stderr}");

    std::fs::write(d.join("contextful.toml"), format!("{MANIFEST}{EVENTS}")).unwrap();
    let out = ok(&sweep(d, "webhook"));
    assert!(out.contains("webhook sweep") && out.contains("epoch 2") && out.contains("2 grants") && out.contains("1 tombstones"), "{out}");
}

const PACK: &str = r#"
[pack.wiki]
tables   = ["wiki/pages", "wiki/comments"]
excluded = []

[pack.wiki.access."wiki/pages"]
resource_key   = "page_id"
resource_kind  = "page"
family         = "item-exception"
fidelity       = "mirrored"
principal_kind = { user = "user", group = "group" }
level          = { viewer = "read" }
"#;

/// `pipeline validate` loads each pack and diagnoses the tables it lands.
#[test]
fn pipeline_validate_refuses_a_pack_table_with_no_mapping_or_exclusion() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("contextful.toml"), format!("{MANIFEST}{PACK}")).unwrap();
    let out = cf(dir.path(), &["pipeline", "validate"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("VisibilityMappingAbsent") && stderr.contains("wiki") && stderr.contains("wiki/comments"), "{stderr}");

    std::fs::write(dir.path().join("contextful.toml"), format!("{MANIFEST}{}", PACK.replace("excluded = []", "excluded = [\"wiki/comments\"]"))).unwrap();
    ok(&cf(dir.path(), &["pipeline", "validate"]));
    std::fs::write(dir.path().join("contextful.toml"), format!("{MANIFEST}{}", PACK.replace("excluded = []", "excluded = []\nevaluate_audience = \"all\""))).unwrap();
    let out = cf(dir.path(), &["pipeline", "validate"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && stderr.contains("VisibilityPackAssertsAccess") && stderr.contains("evaluate_audience"), "{stderr}");
}

const ORG: &str = "[project]\nname = \"wiki\"\nface_scope = \"organization\"\n";

/// On an organization-wide face, a table with no visibility block raises `VisibilityUnboundTable` from diagnose and from the serving guardrail at start, naming the table.
// spec: disclosure.mirror.unbound-table@ac94893a
#[test]
fn an_unbound_table_on_an_organization_wide_face_refuses_diagnose_and_start() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let unbound = format!("{MANIFEST}\n[[pipeline.tables]]\nname = \"wiki/notes\"\n\n{ORG}");
    std::fs::write(d.join("contextful.toml"), &unbound).unwrap();
    for out in [
        cf(d, &["pipeline", "validate"]),
        cf(d, &["mcp"]),
        cf(d, &["serve", "--http", "127.0.0.1:0", "--max-in-flight", "4", "--audience", "contextful://wiki"]),
    ] {
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("VisibilityUnboundTable") && stderr.contains("wiki/notes") && !stderr.contains("`wiki/pages`"), "{stderr}");
    }
    // A personal face serves the same declarations, and binding the table clears diagnose.
    std::fs::write(d.join("contextful.toml"), unbound.replace("face_scope = \"organization\"", "face_scope = \"personal\"")).unwrap();
    ok(&cf(d, &["pipeline", "validate"]));
    std::fs::write(d.join("contextful.toml"), format!("{MANIFEST}\n{ORG}")).unwrap();
    ok(&cf(d, &["pipeline", "validate"]));
    std::fs::write(d.join("contextful.toml"), format!("{MANIFEST}\n[project]\nname = \"wiki\"\nface_scope = \"team\"\n")).unwrap();
    let out = cf(d, &["pipeline", "validate"]);
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("face_scope"), "{}", String::from_utf8_lossy(&out.stderr));
}
