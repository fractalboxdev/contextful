//! Milestone 6 — sync and replicas.
//!
//! Reach: two nodes share one bucket and converge without a coordinator, through a
//! filesystem bucket and through an S3 endpoint alike.

use contextful_acceptance::s3::{S3Server, ACCESS_KEY, SECRET_KEY};
use contextful_acceptance::{bin, GitRepo};
use std::path::{Path, PathBuf};
use std::process::Output;

const STORE: &str = ".contextful/context/research";

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn refused(out: &Output, error: &str) {
    assert!(!out.status.success(), "expected {error}, got success: {}", String::from_utf8_lossy(&out.stdout));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(error), "expected {error}, got: {stderr}");
}

/// The nodes' shared bucket: the `[sync]` lines naming it and the environment its credentials read.
struct Bucket {
    sync: String,
    env: Vec<(&'static str, &'static str)>,
    /// The directory a filesystem bucket lives in; an S3 bucket has none.
    root: Option<PathBuf>,
}

impl Bucket {
    fn manifest(&self) -> Option<serde_json::Value> {
        let path = self.root.as_ref()?.join("context-team/team/manifest.json");
        path.exists().then(|| serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap())
    }
}

/// One node: its own project directory and node id, and the shared bucket.
struct Node<'a> {
    repo: GitRepo,
    cf: &'a Path,
    bucket: &'a Bucket,
}

impl Node<'_> {
    fn run(&self, args: &[&str]) -> Output {
        self.repo.run_env(self.cf, args, &self.bucket.env)
    }

    fn land(&self, run: &str, rows: &str, now: &str) {
        self.repo.write("batch.jsonl", rows);
        ok(&self.run(&["context", "land", "filings", "--project", "research", "--rows", "batch.jsonl", "--run-id", run, "--site-id", "site", "--now", now]));
    }

    fn files(&self) -> Vec<String> {
        ok(&self.run(&["context", "files", "filings", "--project", "research"])).lines().map(str::to_string).collect()
    }

    fn sync(&self, args: &[&str]) -> Output {
        let mut all = vec!["sync"];
        all.extend_from_slice(args);
        all.extend_from_slice(&["--project", "research"]);
        self.run(&all)
    }

    fn pointer(&self) -> String {
        std::fs::read_to_string(self.repo.root.join(STORE).join("tables/filings/_pointer.json")).unwrap()
    }
}

fn node<'a>(cf: &'a Path, bucket: &'a Bucket, id: &str, extra: &str) -> Node<'a> {
    let repo = GitRepo::init();
    repo.write(&format!("{STORE}/config.toml"), &format!("[node]\nid = \"{id}\"\n\n[sync]\n{}{extra}", bucket.sync));
    repo.write("contextful.toml", "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"filings\"\nprimary_key = [\"document_id\"]\norder_by = \"revised_at\"\nretain_runs = \"0s\"\n");
    Node { repo, cf, bucket }
}

/// A plan firing the `feed` pipeline into `filings` through `script`.
fn feed(script: &str) -> String {
    format!("pipeline = \"feed\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1\"\ncommand = [\"sh\", \"{script}\"]\n[cursor]\nkind = \"opaque-token\"\n")
}

/// Two nodes land, push at once, pull each other's runs, compact under the bucket lease,
/// and refuse a publish fenced out by a later holder.
fn converge(cf: &Path, bucket: &Bucket) {
    let a = node(cf, bucket, "ingest-a", "");
    let b = node(cf, bucket, "ingest-b", "");

    // Each node lands its own runs.
    a.land("run-1", "{\"document_id\":\"d1\",\"revised_at\":1,\"title\":\"draft\"}\n{\"document_id\":\"d2\",\"revised_at\":1,\"title\":\"memo\"}\n", "2030-01-01T00:00:00Z");
    b.land("run-1", "{\"document_id\":\"d3\",\"revised_at\":1,\"title\":\"brief\"}\n", "2030-01-01T00:00:01Z");

    // The backend demonstrates the conditional write the declared `cas` coordination rests on.
    assert!(ok(&a.sync(&["probe"])).contains("cas"));

    // A plan emitted before the push names what the push commits, and the bucket holds nothing yet.
    let plan: serde_json::Value = serde_json::from_str(&ok(&a.sync(&["manifest", "--emit"]))).unwrap();
    assert!(bucket.manifest().is_none());

    // Both push at once: the bucket manifest commits by compare-and-set, and neither loses the other's entries.
    let (pa, pb) = std::thread::scope(|s| {
        let ha = s.spawn(|| a.sync(&["push"]));
        let hb = s.spawn(|| b.sync(&["push"]));
        (ha.join().unwrap(), hb.join().unwrap())
    });
    ok(&pa);
    ok(&pb);
    if let Some(committed) = bucket.manifest() {
        for (key, entry) in plan["entries"].as_object().unwrap().iter().filter(|(k, _)| k.contains("/ingest-a/")) {
            assert_eq!(committed["entries"][key], *entry, "{key}");
        }
        assert_eq!(committed["generation"], 2);
    }

    // Generation 2 holds both pushes; a fresh node restores exactly it.
    let c = node(cf, bucket, "restore-c", "");
    ok(&c.sync(&["pull", "--generation", "2"]));
    refused(&c.sync(&["pull", "--generation", "9"]), "SyncGenerationAbsent");
    assert_eq!(
        c.files(),
        ["tables/filings/data/runs/run-1/ingest-a/part-00000.parquet", "tables/filings/data/runs/run-1/ingest-b/part-00000.parquet"]
    );

    // Each pulls the other's runs; one run id on two nodes stays two runs.
    ok(&a.sync(&["pull"]));
    ok(&b.sync(&["pull"]));
    let (fa, fb) = (a.files(), b.files());
    assert_eq!(fa, fb);
    assert_eq!(
        fa,
        ["tables/filings/data/runs/run-1/ingest-a/part-00000.parquet", "tables/filings/data/runs/run-1/ingest-b/part-00000.parquet"]
    );

    // B's feed run commits its cursor; the fold below collects the run and its manifest with it.
    b.repo.write("vendor.sh", "printf '{\"rows\":[{\"document_id\":\"d5\",\"revised_at\":1,\"title\":\"feed\"}],\"cursor\":\"p9\",\"more\":false}'\n");
    b.repo.write("feed.toml", &feed("vendor.sh"));
    ok(&b.run(&["run", "start", "--plan", "feed.toml", "--project", "research", "--run-id", "f1", "--site-id", "site", "--now", "2030-01-01T00:30:00Z"]));

    // Compaction takes the table's bucket lease; a second node finding it held is skipped.
    ok(&a.sync(&["lease", "acquire", "filings", "--now", "2030-01-01T01:00:00Z"]));
    refused(&b.sync(&["compact", "filings", "--now", "2030-01-01T01:01:00Z"]), "LeaseHeld");
    ok(&a.sync(&["lease", "release", "filings", "--now", "2030-01-01T01:02:00Z"]));
    let folded = ok(&b.sync(&["compact", "filings", "--now", "2030-01-01T01:03:00Z"]));
    assert!(folded.contains("folded"), "{folded}");

    // A pulls the snapshot B published under its fence: both nodes read the one snapshot.
    ok(&a.sync(&["pull"]));
    let (fa, fb) = (a.files(), b.files());
    assert_eq!(fa, fb, "the two nodes converge");
    assert_eq!(fa.len(), 1, "{fa:?}");
    assert!(fa[0].starts_with("tables/filings/data/snapshots/snapshot-"), "{fa:?}");
    assert_eq!(a.pointer(), b.pointer());

    // A holder whose lease passed to a later fence publishes nothing.
    ok(&a.sync(&["lease", "acquire", "filings", "--now", "2030-01-01T02:00:00Z"]));
    ok(&b.sync(&["lease", "acquire", "filings", "--now", "2030-01-01T02:11:00Z"]));
    a.land("run-2", "{\"document_id\":\"d4\",\"revised_at\":2,\"title\":\"order\"}\n", "2030-01-01T02:12:00Z");
    refused(&a.sync(&["compact", "filings", "--held", "--now", "2030-01-01T02:12:30Z"]), "LeaseFenced");
    ok(&b.sync(&["pull"]));
    assert_eq!(b.files(), fb, "the fenced snapshot stays unreadable");

    // A cold node declaring `pull_before_run` starts its first run on the converged snapshot.
    let cold = node(cf, bucket, "ingest-c", "pull_before_run = true\n");
    cold.repo.write("empty.sh", "printf '%s' \"${CONTEXTFUL_CURSOR:-start}\" > cursor.log\nprintf '{\"rows\":[],\"more\":false}'\n");
    cold.repo.write("feed.toml", &feed("empty.sh"));
    ok(&cold.run(&["run", "start", "--plan", "feed.toml", "--project", "research", "--run-id", "c1", "--site-id", "site", "--now", "2030-01-01T03:00:00Z"]));
    assert_eq!(cold.files(), fb, "the cold node reads what the others converged on");
    assert_eq!(cold.pointer(), b.pointer());
    // The run state each pushing node recorded travels with the store, and the cold node's first
    // feed run resumes from the cursor B's collected run committed.
    for id in ["ingest-a", "ingest-b"] {
        let state: serde_json::Value = serde_json::from_slice(&std::fs::read(cold.repo.root.join(STORE).join(format!("nodes/{id}/run-state.json"))).unwrap()).unwrap();
        assert_eq!(state["node_id"], id, "{state}");
    }
    assert!(!cold.repo.root.join(STORE).join("tables/filings/data/runs/f1").exists(), "the fold collected the feed run");
    assert_eq!(std::fs::read_to_string(cold.repo.root.join("cursor.log")).unwrap(), "\"p9\"");
}

/// The declaration of the carry case: a keyed table and a model over it.
const NOTES: &str = "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"notes\"\nprimary_key = [\"id\"]\n\n\
[[model]]\nid = \"note_kinds\"\nsql = \"SELECT kind, CAST(count(*) AS BIGINT) AS n FROM notes GROUP BY kind\"\nunique_key = [\"kind\"]\n\n\
[model.contract]\nversion = \"1.0.0\"\ncolumns = [{ name = \"kind\", type = \"utf8\", nullable = false }, { name = \"n\", type = \"int64\", nullable = false }]\n";

/// Run `args` against the research project at the instant `now`.
fn at(n: &Node, args: &[&str], now: &str) -> String {
    let mut all = args.to_vec();
    all.extend_from_slice(&["--project", "research", "--now", now]);
    ok(&n.run(&all))
}

fn rows(n: &Node, sql: &str) -> serde_json::Value {
    let read: serde_json::Value = serde_json::from_str(&ok(&n.run(&["query", "--json", "--project", "research", sql]))).unwrap();
    read["rows"].clone()
}

/// A built model and a locally folded table, whose runs retention collected, reach a
/// second node through the bucket: the push carries their pointers, and the pull writes them.
fn carry(cf: &Path, bucket: &Bucket) {
    let d = node(cf, bucket, "build-d", "");
    d.repo.write("contextful.toml", NOTES);
    d.repo.write("notes.jsonl", "{\"id\":\"n1\",\"kind\":\"a\"}\n{\"id\":\"n2\",\"kind\":\"a\"}\n{\"id\":\"n3\",\"kind\":\"b\"}\n");
    at(&d, &["context", "land", "notes", "--rows", "notes.jsonl", "--run-id", "n-1", "--site-id", "site"], "2030-02-01T00:00:00Z");
    at(&d, &["build", "note_kinds", "--site-id", "site"], "2030-02-01T01:00:00Z");
    let folded = at(&d, &["context", "compact", "notes"], "2030-02-01T02:00:00Z");
    assert!(folded.contains("folded"), "{folded}");
    // Past `retain_runs`, the folded run is collected and the snapshot alone holds the rows.
    at(&d, &["context", "compact", "notes"], "2030-02-09T02:00:00Z");
    let pushed = ok(&d.sync(&["push"]));
    assert!(pushed.contains("and 2 pointers"), "{pushed}");

    let e = node(cf, bucket, "read-e", "");
    e.repo.write("contextful.toml", NOTES);
    ok(&e.sync(&["pull"]));
    assert_eq!(rows(&e, "SELECT kind, n FROM note_kinds ORDER BY kind"), serde_json::json!([["a", "2"], ["b", "1"]]));
    assert_eq!(rows(&e, "SELECT id FROM notes ORDER BY id"), serde_json::json!([["n1"], ["n2"], ["n3"]]));
}

#[test]
fn m06_sync() {
    let cf = bin("contextful");
    let dir = tempfile::tempdir().unwrap();
    let bucket = Bucket {
        sync: format!("endpoint = \"file://{}\"\nbucket = \"context-team\"\nprefix = \"team\"\ncoordination = \"cas\"\n", dir.path().display()),
        env: Vec::new(),
        root: Some(dir.path().to_path_buf()),
    };
    converge(&cf, &bucket);
    carry(&cf, &bucket);
}

#[test]
fn m06_sync_over_s3() {
    let cf: PathBuf = bin("contextful");
    let server = S3Server::start("context-team");
    let bucket = Bucket {
        sync: format!(
            "endpoint = \"{}\"\nbucket = \"context-team\"\nprefix = \"team\"\ncoordination = \"cas\"\naccess_key_id = \"env://CONTEXTFUL_SYNC_ACCESS_KEY_ID\"\nsecret_access_key = \"env://CONTEXTFUL_SYNC_SECRET_ACCESS_KEY\"\n",
            server.endpoint
        ),
        env: vec![("CONTEXTFUL_SYNC_ACCESS_KEY_ID", ACCESS_KEY), ("CONTEXTFUL_SYNC_SECRET_ACCESS_KEY", SECRET_KEY)],
        root: None,
    };
    converge(&cf, &bucket);
    carry(&cf, &bucket);
    assert!(server.keys().iter().any(|k| k == "team/manifest.json"), "the bucket manifest sits under the prefix");
    assert!(server.keys().iter().all(|k| k.starts_with("team/")), "{:?}", server.keys());
}
