//! The store's components through the built binary: what is canonical, what each catalog
//! holds, and what a sync or a compaction carries.

use super::{cf, file_sync, files, ok, project, start, STORE};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn land(at: &Path, run: &str, rows: &str, now: &str) {
    std::fs::write(at.join("batch.jsonl"), rows).unwrap();
    ok(&cf(at, &["context", "land", "filings", "--project", "research", "--rows", "batch.jsonl", "--run-id", run, "--site-id", "site", "--now", now], &[]));
}

fn keys(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p.strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out.sort();
    out
}

fn json(path: PathBuf) -> Value {
    serde_json::from_slice(&std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))).unwrap()
}

fn count(at: &Path) -> String {
    ok(&cf(at, &["query", "--json", "--project", "research", "SELECT count(*) FROM filings"], &[]))
}

/// A store holds Parquet table data, JSON run and snapshot manifests, one pointer object per table, landed blobs,
/// and two SQLite catalogs, `derived.sqlite` and `machine.sqlite`. Parquet, manifests and pointers are canonical.
// spec: store.lay-out.components@d1b043d3
#[test]
fn a_store_holds_its_canonical_tree_beside_two_disposable_catalogs() {
    let bucket = tempfile::tempdir().unwrap();
    let a = project("ingest-a", &file_sync(bucket.path(), ""));
    land(a.path(), "run-1", "{\"id\":1}\n{\"id\":2}\n", "2030-01-01T00:00:00Z");
    ok(&start(a.path(), "run-2"));
    ok(&cf(a.path(), &["context", "compact", "filings", "--project", "research", "--now", "2030-01-01T02:00:00Z"], &[]));
    ok(&cf(a.path(), &["context", "rebuild-catalog", "--project", "research"], &[]));
    let root = a.path().join(STORE);
    let tree = keys(&root);
    for catalog in ["derived.sqlite", "machine.sqlite"] {
        assert!(tree.iter().any(|k| k == catalog), "{catalog} in {tree:?}");
    }
    let table = |suffix: &str| tree.iter().filter(|k| k.starts_with("tables/filings/") && k.ends_with(suffix)).count();
    assert_eq!(tree.iter().filter(|k| k.ends_with("_pointer.json")).count(), 1, "{tree:?}");
    assert!(table(".parquet") >= 1, "{tree:?}");
    let pointer = json(root.join("tables/filings/_pointer.json"));
    let snapshot = pointer["snapshot_id"].as_str().unwrap();
    assert_eq!(json(root.join(format!("tables/filings/data/snapshots/{snapshot}/_manifest.json")))["table"], "filings");
    assert_eq!(json(root.join("tables/filings/data/runs/run-1/ingest-a/_manifest.json"))["run_id"], "run-1");
    let before = count(a.path());
    assert!(before.contains("[[\"2\"]]") || before.contains("[[2]]"), "{before}");

    // Without either catalog the tree still answers: Parquet, manifests and pointers are the store.
    std::fs::remove_file(root.join("derived.sqlite")).unwrap();
    std::fs::remove_file(root.join("machine.sqlite")).unwrap();
    assert_eq!(count(a.path()), before);
    ok(&cf(a.path(), &["context", "rebuild-catalog", "--project", "research"], &[]));
    assert!(root.join("derived.sqlite").is_file());
    assert_eq!(count(a.path()), before);
}

/// `machine.sqlite` holds one machine's journal, cursor cache, lease rows and run-row cache. It is never synced or
/// replaced by a pull; a rebuild refills it from {{run.record.reserved-table}} only while it holds no run row, and
/// rebuilds nothing else.
// spec: store.lay-out.machine-catalog@31c18d65
#[test]
fn the_machine_catalog_stays_on_its_machine_through_push_pull_and_rebuild() {
    let bucket = tempfile::tempdir().unwrap();
    let sync = file_sync(bucket.path(), "");
    let a = project("ingest-a", &sync);
    land(a.path(), "run-1", "{\"id\":1}\n", "2030-01-01T00:00:00Z");
    ok(&start(a.path(), "run-a"));
    assert!(a.path().join(STORE).join("machine.sqlite").is_file());
    ok(&cf(a.path(), &["sync", "push", "--project", "research"], &[]));
    let pushed = keys(bucket.path());
    assert!(pushed.iter().any(|k| k.ends_with("part-00000.parquet")), "{pushed:?}");
    assert!(pushed.iter().all(|k| !k.contains("machine.sqlite") && !k.contains("derived.sqlite")), "{pushed:?}");

    let b = project("ingest-b", &sync);
    ok(&start(b.path(), "run-b"));
    let machine = b.path().join(STORE).join("machine.sqlite");
    let held = std::fs::read(&machine).unwrap();
    ok(&cf(b.path(), &["sync", "pull", "--project", "research"], &[]));
    assert_eq!(files(b.path()), ["tables/filings/data/runs/run-1/ingest-a/part-00000.parquet"]);
    assert_eq!(std::fs::read(&machine).unwrap(), held, "a pull replaced the machine catalog");
    ok(&cf(b.path(), &["context", "rebuild-catalog", "--project", "research"], &[]));
    assert_eq!(std::fs::read(&machine).unwrap(), held, "a rebuild rewrote the machine catalog");
    let shown: Value = serde_json::from_str(&ok(&cf(b.path(), &["run", "show", "run-b", "--project", "research"], &[]))).unwrap();
    assert_eq!(shown["run_id"], "run-b");

    // An empty catalog is refilled from the run record: every run the pulled store recorded.
    std::fs::remove_file(&machine).unwrap();
    ok(&cf(b.path(), &["context", "rebuild-catalog", "--project", "research"], &[]));
    for run in ["run-a", "run-b"] {
        let shown: Value = serde_json::from_str(&ok(&cf(b.path(), &["run", "show", run, "--project", "research"], &[]))).unwrap();
        assert_eq!(shown["run_id"], run);
    }
}

/// A pass holds the table's compaction lease and stamps its fence into the snapshot manifest and the pointer.
// spec: store.fold.compaction-lease@6b473e0b
#[test]
fn a_compaction_stamps_its_lease_fence_into_the_snapshot_manifest_and_the_pointer() {
    let bucket = tempfile::tempdir().unwrap();
    let sync = file_sync(bucket.path(), "");
    let a = project("ingest-a", &sync);
    land(a.path(), "run-1", "{\"id\":1}\n", "2030-01-01T00:00:00Z");
    ok(&cf(a.path(), &["sync", "push", "--project", "research"], &[]));
    // A first lease taken and released moves the fence past 1, so the stamp is the lease's own.
    ok(&cf(a.path(), &["sync", "lease", "acquire", "filings", "--project", "research", "--now", "2030-01-01T00:10:00Z"], &[]));
    ok(&cf(a.path(), &["sync", "lease", "release", "filings", "--project", "research", "--now", "2030-01-01T00:11:00Z"], &[]));
    let folded = ok(&cf(a.path(), &["sync", "compact", "filings", "--project", "research", "--now", "2030-01-01T01:00:00Z"], &[]));
    assert!(folded.contains("folded"), "{folded}");

    let root = a.path().join(STORE);
    let pointer = json(root.join("tables/filings/_pointer.json"));
    let snapshot = pointer["snapshot_id"].as_str().unwrap();
    let manifest = json(root.join(format!("tables/filings/data/snapshots/{snapshot}/_manifest.json")));
    assert_eq!(pointer["fence"], 2, "{pointer}");
    assert_eq!(manifest["fence"], 2, "{manifest}");
    let remote: Value = serde_json::from_slice(&std::fs::read(bucket.path().join("context-team/team/research/tables/filings/_pointer.json")).unwrap()).unwrap();
    assert_eq!((remote["snapshot_id"].as_str(), &remote["fence"]), (Some(snapshot), &Value::from(2)));
    let pushed: Value = serde_json::from_slice(&std::fs::read(bucket.path().join(format!("context-team/team/research/tables/filings/data/snapshots/{snapshot}/_manifest.json"))).unwrap()).unwrap();
    assert_eq!(pushed["fence"], 2, "{pushed}");
}

/// A pipeline's committed position is the cursor inside its newest commit: the newest commit-log entry for a
/// leased pipeline, the highest run-manifest cursor otherwise. `machine.sqlite` caches it.
// spec: store.lay-out.cursor-in-commit@6cc0f1e2
#[test]
fn a_lost_machine_catalog_resumes_from_the_cursor_inside_the_newest_commit() {
    let bucket = tempfile::tempdir().unwrap();
    let a = project("ingest-a", &file_sync(bucket.path(), ""));
    let p = a.path();
    // Each pull logs the position it was handed, then serves one row and the next position.
    std::fs::write(
        p.join("paged.sh"),
        "echo \"$CONTEXTFUL_CURSOR\" >> handed.log\nn=$(wc -l < handed.log | tr -d ' ')\nprintf '{\"rows\":[{\"id\":\"r%s\",\"n\":%s}],\"cursor\":\"p%s\",\"more\":false}' \"$n\" \"$n\" \"$n\"\n",
    )
    .unwrap();
    let plan = |pipeline: &str, extra: &str| {
        let body = format!("pipeline = \"{pipeline}\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1\"\ncommand = [\"sh\", \"paged.sh\"]\n{extra}");
        std::fs::write(p.join(format!("{pipeline}.toml")), body).unwrap();
    };
    plan("leased", "");
    plan("unleased", "[cursor]\nkind = \"monotonic\"\nfield = \"n\"\n");
    let run = |plan: &str, id: &str| {
        ok(&cf(p, &["run", "start", "--plan", plan, "--project", "research", "--run-id", id, "--site-id", "site", "--now", "2030-01-01T01:00:00Z"], &[]))
    };
    let handed = || std::fs::read_to_string(p.join("handed.log")).unwrap().lines().map(str::to_string).collect::<Vec<_>>();
    let root = p.join(STORE);

    // A leased (opaque-token) pipeline: its commit-log entry carries the cursor.
    run("leased.toml", "l1");
    let log_dir = root.join("cursors/leased/ingest-a");
    let entries: Vec<Value> = keys(&log_dir).into_iter().map(|k| json(log_dir.join(k))).collect();
    assert!(entries.iter().any(|e| e["kind"] == "commit" && e["cursor"] == "p1" && e["cursor_kind"] == "opaque-token"), "{entries:?}");
    std::fs::remove_file(root.join("machine.sqlite")).unwrap();
    run("leased.toml", "l2");
    assert_eq!(handed(), ["", "\"p1\""], "the second run starts at the committed position, not the start");

    // An unleased (monotonic) pipeline: the highest run-manifest cursor is the position.
    run("unleased.toml", "u1");
    let manifest = json(root.join("tables/filings/data/runs/u1/ingest-a/_manifest.json"));
    let position = manifest["cursor"].clone();
    assert_eq!(position["at"], 3, "{manifest}");
    std::fs::remove_file(root.join("machine.sqlite")).unwrap();
    run("unleased.toml", "u2");
    let resumed: Value = serde_json::from_str(handed().last().unwrap()).unwrap();
    assert_eq!(resumed, position);
}
