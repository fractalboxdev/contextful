//! Maintenance jobs through the built binary: a `fold` job's pass on a local and a synced
//! store, a `sync-push` job's push, their dispatch under `pipeline serve`, and the kinds
//! validation refuses.
#![cfg(feature = "data-plane")]

use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output};

const STORE: &str = ".contextful/context/research";
const FOLD: &str = "[[job]]\nname = \"fold-filings\"\nkind = \"fold\"\ntarget = \"feed_filings\"\nschedule = \"every 1h\"\n";
const PUSH: &str = "[[job]]\nname = \"push-store\"\nkind = \"sync-push\"\nschedule = \"every 1h\"\n";

fn cf(dir: &Path, args: &[&str]) -> Output {
    cf_env(dir, args, &[])
}

fn cf_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful")).args(args).current_dir(dir).env_remove("CONTEXTFUL_NODE_ID").envs(env.iter().copied()).output().unwrap()
}

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn refused(out: &Output, error: &str) -> String {
    assert!(!out.status.success(), "expected {error}, got: {}", String::from_utf8_lossy(&out.stdout));
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(stderr.contains(error), "expected {error}, got: {stderr}");
    stderr
}

fn json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))).unwrap()
}

/// A project whose pipeline `feed` produces `feed_filings`, declaring `jobs`, whose node is `ingest-a`, and whose store
/// declares `sync` when one is given.
fn project(jobs: &str, sync: Option<&Path>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(STORE);
    std::fs::create_dir_all(&store).unwrap();
    let sync = sync.map(|b| format!("\n[sync]\nendpoint = \"file://{}\"\nbucket = \"context-team\"\nprefix = \"team\"\ncoordination = \"single-writer\"\n", b.display())).unwrap_or_default();
    std::fs::write(store.join("config.toml"), format!("[node]\nid = \"ingest-a\"\n{sync}")).unwrap();
    std::fs::write(dir.path().join("contextful.toml"), format!("authoring_posture = \"per_request\"\nsite_id = \"site\"\n\n[[pipeline]]\nid = \"feed\"\ntables = [\"filings\"]\n[pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"http://127.0.0.1:1/v1\" }}\n\n{jobs}")).unwrap();
    dir
}

fn land(dir: &Path, run: &str, now: &str) {
    std::fs::write(dir.join("batch.jsonl"), format!("{{\"id\":\"{run}\"}}\n")).unwrap();
    ok(&cf(dir, &["context", "land", "feed_filings", "--project", "research", "--rows", "batch.jsonl", "--run-id", run, "--site-id", "site", "--now", now]));
}

/// The runs the current snapshot of `feed_filings` folds past its origin, and the pointer
/// naming it.
fn folded(dir: &Path) -> (Vec<String>, Value) {
    let root = dir.join(STORE);
    let pointer = json(&root.join("tables/feed_filings/_pointer.json"));
    let snapshot = pointer["snapshot_id"].as_str().unwrap().to_string();
    let manifest = json(&root.join(format!("tables/feed_filings/data/snapshots/{snapshot}/_manifest.json")));
    let mut runs: Vec<String> = manifest["includes_runs"].as_array().unwrap().iter().map(|r| r.as_str().unwrap().to_string()).collect();
    runs.sort();
    (runs, pointer)
}

fn cycle(dir: &Path, now: &str, extra: &[&str]) -> Value {
    let mut args = vec!["pipeline", "serve", "--cycle", "--project", "research", "--now", now];
    args.extend_from_slice(extra);
    serde_json::from_str(&ok(&cf(dir, &args))).unwrap()
}

/// Import the declaration of a synced store under an admin credential, returning the
/// issuer's public key.
fn import_synced(dir: &Path) -> String {
    std::fs::write(dir.join(".contextful/issuance.toml"), "default_audience = 'contextful://research'\nmax_lifetime_secs = 3600\n").unwrap();
    let public = ok(&cf(dir, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let admin = ok(&cf(dir, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@example.test", "--ttl", "600", "--action", "admin", "--table", "*"]));
    let import = ["pipeline", "import", "--project", "research", "--issuer-key", ".contextful/issuer.seed", "--public-key", &public, "--audience", "contextful://research"];
    ok(&cf_env(dir, &import, &[("CONTEXTFUL_TOKEN", &admin)]));
    public
}

/// `contextful job fire` on a `fold` job runs a pass over its `target`, or over every table the store holds when it
/// names none, reporting each table's result.
// spec: store.fold.job-pass@54c9704f
#[test]
fn a_fired_fold_job_folds_its_targets_runs_into_a_snapshot() {
    let dir = project(FOLD, None);
    let p = dir.path();
    land(p, "run-1", "2030-01-01T00:00:00Z");
    land(p, "run-2", "2030-01-01T00:01:00Z");
    assert!(!p.join(STORE).join("tables/feed_filings/_pointer.json").exists());
    let out = ok(&cf(p, &["job", "fire", "fold-filings", "--project", "research", "--now", "2030-01-01T00:02:00Z"]));
    assert!(out.starts_with("feed_filings: folded"), "{out}");
    let (runs, pointer) = folded(p);
    assert_eq!(runs, ["run-1/ingest-a", "run-2/ingest-a"]);
    // A local pass publishes under no lease, so its pointer carries no fence.
    assert!(pointer["fence"].is_null(), "{pointer}");

    // A fold job naming no target folds every table the store holds.
    std::fs::write(p.join("contextful.toml"), std::fs::read_to_string(p.join("contextful.toml")).unwrap().replace("target = \"feed_filings\"\n", "")).unwrap();
    land(p, "run-3", "2030-01-01T00:03:00Z");
    let out = ok(&cf(p, &["job", "fire", "fold-filings", "--project", "research", "--now", "2030-01-01T00:04:00Z"]));
    assert!(out.starts_with("feed_filings: folded"), "{out}");
    assert_eq!(folded(p).0, ["run-3/ingest-a"]);
}

/// On a store declaring `[sync]`, a fold job's pass takes the table's compaction lease, pulls, folds, pushes and
/// publishes; on any other store it folds locally.
// spec: store.fold.job-pass-synced@a47c183e
#[test]
fn a_fold_job_on_a_synced_store_publishes_under_the_compaction_lease() {
    let bucket = tempfile::tempdir().unwrap();
    let dir = project(FOLD, Some(bucket.path()));
    let p = dir.path();
    land(p, "run-1", "2030-01-01T00:00:00Z");
    land(p, "run-2", "2030-01-01T00:01:00Z");
    let out = ok(&cf(p, &["job", "fire", "fold-filings", "--project", "research", "--now", "2030-01-01T00:02:00Z"]));
    assert!(out.starts_with("feed_filings: folded"), "{out}");
    let (runs, pointer) = folded(p);
    assert_eq!(runs, ["run-1/ingest-a", "run-2/ingest-a"]);
    assert!(pointer["fence"].as_u64().is_some_and(|f| f >= 1), "{pointer}");
    let remote = json(&bucket.path().join("context-team/team/research/tables/feed_filings/_pointer.json"));
    assert_eq!((&remote["snapshot_id"], &remote["fence"]), (&pointer["snapshot_id"], &pointer["fence"]));
    // The pass releases its lease, so a second fire takes it again.
    land(p, "run-3", "2030-01-01T00:03:00Z");
    ok(&cf(p, &["job", "fire", "fold-filings", "--project", "research", "--now", "2030-01-01T00:04:00Z"]));
    let (runs, next) = folded(p);
    assert_eq!(runs, ["run-3/ingest-a"]);
    assert!(next["fence"].as_u64() > pointer["fence"].as_u64(), "{next}");
}

/// A scheduled `fold` or `sync-push` job dispatches `job fire <name>` through the cadence lease and pool under
/// `job:<name>`.
// spec: surface.fire.scheduled-maintenance@1475f334
#[test]
fn serve_dispatches_a_fold_job_on_its_schedule() {
    let dir = project(FOLD, None);
    let p = dir.path();
    land(p, "run-1", "2030-01-01T00:00:00Z");
    land(p, "run-2", "2030-01-01T00:01:00Z");
    ok(&cf(p, &["pipeline", "import", "--project", "research"]));
    let first = cycle(p, "2030-01-01T00:02:00Z", &[]);
    assert_eq!(first["fired"], serde_json::json!(["job:fold-filings"]), "{first}");
    assert_eq!(folded(p).0, ["run-1/ingest-a", "run-2/ingest-a"]);
    // The dispatch record holds the next fire an hour out.
    land(p, "run-3", "2030-01-01T00:03:00Z");
    assert_eq!(cycle(p, "2030-01-01T00:04:00Z", &[])["fired"], serde_json::json!([]));
    assert_eq!(folded(p).0, ["run-1/ingest-a", "run-2/ingest-a"]);
    assert_eq!(cycle(p, "2030-01-01T01:03:00Z", &[])["fired"], serde_json::json!(["job:fold-filings"]));
    assert_eq!(folded(p).0, ["run-3/ingest-a"]);
}

/// `contextful job fire` on a `sync-push` job pushes the store as `contextful sync push` does.
// spec: store.push.job-push@a042b97d
#[test]
fn a_sync_push_job_pushes_when_fired_and_when_serve_dispatches_it() {
    let bucket = tempfile::tempdir().unwrap();
    let dir = project(PUSH, Some(bucket.path()));
    let p = dir.path();
    let part = |run: &str| bucket.path().join(format!("context-team/team/research/tables/feed_filings/data/runs/{run}/ingest-a/part-00000.parquet"));
    land(p, "run-1", "2030-01-01T00:00:00Z");
    let out = ok(&cf(p, &["job", "fire", "push-store", "--project", "research", "--now", "2030-01-01T00:01:00Z"]));
    assert!(out.starts_with("pushed "), "{out}");
    assert!(part("run-1").exists());

    land(p, "run-2", "2030-01-01T00:02:00Z");
    let public = import_synced(p);
    let answer = cycle(p, "2030-01-01T00:03:00Z", &["--public-key", &public]);
    assert_eq!(answer["fired"], serde_json::json!(["job:push-store"]), "{answer}");
    assert!(part("run-2").exists());

    // A store declaring no `[sync]` has nothing to push to, and the fire refuses.
    let local = project(PUSH, None);
    let e = String::from_utf8_lossy(&cf(local.path(), &["job", "fire", "push-store", "--project", "research"]).stderr).to_string();
    assert!(e.contains("[sync]"), "{e}");
}

/// A `[[job]]` of kind `sweep`, `rebuild-catalog` or `validate` raises `JobKindUndispatched` at `pipeline validate`,
/// `job validate`, import and full apply.
// spec: surface.fire.kind-undispatched@dc08aa63
#[test]
fn validation_refuses_each_maintenance_kind_with_no_dispatch_by_name() {
    for kind in ["sweep", "rebuild-catalog", "validate"] {
        let dir = project(&format!("[[job]]\nname = \"chore\"\nkind = \"{kind}\"\nschedule = \"every 1h\"\n"), None);
        let p = dir.path();
        for args in [&["pipeline", "validate"][..], &["job", "validate"], &["pipeline", "import", "--project", "research"]] {
            let e = refused(&cf(p, args), "JobKindUndispatched");
            assert!(e.contains(&format!("`{kind}`")) && e.contains("chore"), "{args:?}: {e}");
        }
        assert!(!p.join(".contextful/control/research/manifest@current").exists(), "{kind}: an import refused claims no version");
    }
    // The kinds serve dispatches validate.
    let dir = project(&format!("{FOLD}\n{PUSH}"), None);
    ok(&cf(dir.path(), &["pipeline", "validate"]));
    let out = ok(&cf(dir.path(), &["job", "validate"]));
    assert!(out.contains("fold-filings: valid (fold)") && out.contains("push-store: valid (sync-push)"), "{out}");
}
