//! `contextful export run` with an Iceberg block, through the built binary, against the
//! loopback Iceberg REST catalog over the loopback S3 bucket.
#![cfg(feature = "iceberg-export")]

use arrow_array::cast::AsArray;
use arrow_array::types::Int64Type;
use arrow_array::RecordBatch;
use contextful_acceptance::iceberg::{Catalog, Script};
use contextful_acceptance::s3::S3Server;
use iceberg::spec::Operation;
use std::path::Path;
use std::process::{Command, Output};

const AUD: &str = "contextful://acme-research";
const TOKEN: &str = "catalog-token-value";
const NS: &str = "mirror";
const TABLE: &str = "spans";

fn cf(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful")).args(args).envs(env.iter().copied()).current_dir(dir).env_remove("CONTEXTFUL_NODE_ID").output().unwrap()
}

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn err(out: &Output) -> String {
    assert!(!out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// A loopback bucket and the catalog over it.
struct Remote {
    s3: S3Server,
    catalog: Catalog,
}

impl Remote {
    fn start() -> Remote {
        let s3 = S3Server::start("exports");
        let catalog = Catalog::start(&s3, TOKEN);
        Remote { s3, catalog }
    }

    fn block(&self, name: &str, credential: &str, version: u8) -> String {
        format!(
            "[[export]]\nname = \"{name}\"\ntable = \"spans\"\nformat = \"iceberg\"\ncredential = \"{credential}\"\n\
             catalog = \"{}\"\nstorage = \"{}\"\nwarehouse = \"exports\"\nnamespace = \"{NS}\"\ntable_name = \"{TABLE}\"\n\
             format_version = {version}\ntoken = \"${{secret://catalog-token}}\"\n",
            self.catalog.endpoint,
            self.s3.endpoint
        )
    }

    fn scan(&self) -> Vec<RecordBatch> {
        self.catalog.scan(&self.s3, NS, TABLE)
    }

    /// Every object under the table's `data/` prefix.
    fn data_objects(&self) -> Vec<String> {
        self.s3.keys().into_iter().filter(|k| k.contains("/data/")).collect()
    }

    fn operations(&self) -> Vec<Operation> {
        let metadata = self.catalog.metadata(NS, TABLE).unwrap();
        let mut snapshots: Vec<_> = metadata.snapshots().collect();
        snapshots.sort_by_key(|s| s.sequence_number());
        snapshots.iter().map(|s| s.summary().operation.clone()).collect()
    }
}

fn strings(batches: &[RecordBatch], column: &str) -> Vec<String> {
    let mut out: Vec<String> = batches.iter().flat_map(|b| b.column_by_name(column).unwrap().as_string::<i32>().iter().map(|v| v.unwrap().to_string()).collect::<Vec<_>>()).collect();
    out.sort();
    out
}

fn project(manifest: &str) -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let store = p.join(".contextful/context/research");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    std::fs::write(p.join(".contextful/issuance.toml"), format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n")).unwrap();
    std::fs::write(p.join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{manifest}")).unwrap();
    let public = ok(&cf(p, &["token", "keygen", "--out", ".contextful/issuer.seed"], &[]));
    let token = mint(p, "svc://mirror");
    (dir, public, token)
}

fn mint(p: &Path, principal: &str) -> String {
    ok(&cf(p, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", principal, "--zone", "on-prem:hq", "--table", "spans", "--ttl", "600"], &[]))
}

/// Land `ids` into `spans` as run `run`.
fn land(dir: &Path, run: &str, now: &str, ids: &[String]) {
    let rows: String = ids.iter().enumerate().map(|(i, id)| format!("{{\"span_id\":\"{id}\",\"duration_ms\":{}}}\n", 10 + i)).collect();
    let file = format!("{run}.jsonl");
    std::fs::write(dir.join(&file), rows).unwrap();
    ok(&cf(dir, &["context", "land", "spans", "--project", "research", "--rows", &file, "--run-id", run, "--site-id", "site", "--now", now], &[]));
}

fn ids(prefix: &str, n: usize) -> Vec<String> {
    (0..n).map(|i| format!("{prefix}{i:03}")).collect()
}

fn export(dir: &Path, name: &str, public: &str, token: &str) -> Output {
    let env = [("CONTEXTFUL_TOKEN", token), ("CATALOG_TOKEN", TOKEN), ("CONTEXTFUL_SECRETS_ALLOW_ENV_TEMPLATES", "1")];
    cf(dir, &["export", "run", name, "--project", "research", "--public-key", public, "--audience", AUD], &env)
}

/// The first export creates the table and appends its files; each later export commits one `overwrite` snapshot that
/// marks every live parent file deleted, so the reader sees the latest export's rows alone.
#[test]
fn two_exports_commit_an_append_then_an_overwrite_and_the_reader_sees_the_second() {
    let remote = Remote::start();
    let (dir, public, token) = project(&remote.block("spans-lake", "svc://mirror", 3));
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 3));
    let out = ok(&export(p, "spans-lake", &public, &token));
    assert!(out.contains("spans-lake: committed 3 rows in 1 files"), "{out}");
    assert_eq!(strings(&remote.scan(), "span_id"), ids("a", 3));
    let first = remote.data_objects();
    assert_eq!(first.len(), 1);

    land(p, "load-2", "2030-01-01T00:01:00Z", &ids("b", 2));
    ok(&export(p, "spans-lake", &public, &token));
    let mut both = ids("a", 3);
    both.extend(ids("b", 2));
    let batches = remote.scan();
    assert_eq!(strings(&batches, "span_id"), both, "the second export carries every admitted row, and the first export's file is deleted");
    let durations: i64 = batches.iter().map(|b| b.column_by_name("duration_ms").unwrap().as_primitive::<Int64Type>().iter().flatten().sum::<i64>()).sum();
    assert_eq!(durations, 10 + 11 + 12 + 10 + 11);
    for batch in &batches {
        assert!(batch.schema().fields().iter().all(|f| !f.name().starts_with('_')), "the engine's own columns stay its own: {:?}", batch.schema());
    }
    assert_eq!(remote.operations(), [Operation::Append, Operation::Overwrite]);
    let live = remote.catalog.live_files(&remote.s3, NS, TABLE);
    assert_eq!(live.len(), 1);
    assert!(!live.contains(&first[0]), "the parent's file is no longer live");
    assert!(remote.s3.keys().contains(&first[0]), "the exporter deletes no committed file; catalog expiry owns retention");
    let metadata = remote.catalog.metadata(NS, TABLE).unwrap();
    assert_eq!(metadata.format_version(), iceberg::spec::FormatVersion::V3);
}

/// An export whose recorded snapshot, frontier, read identity and target all still hold commits nothing.
#[test]
fn an_unchanged_export_commits_nothing() {
    let remote = Remote::start();
    let (dir, public, token) = project(&remote.block("spans-lake", "svc://mirror", 2));
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 2));
    ok(&export(p, "spans-lake", &public, &token));
    assert_eq!(remote.catalog.commits(), 1);
    let out = ok(&export(p, "spans-lake", &public, &token));
    assert!(out.contains("unchanged"), "{out}");
    assert_eq!(remote.catalog.commits(), 1);
    assert_eq!(remote.data_objects().len(), 1);
    land(p, "load-2", "2030-01-01T00:01:00Z", &ids("b", 1));
    ok(&export(p, "spans-lake", &public, &token));
    assert_eq!(remote.catalog.commits(), 2);
}

/// A commit conflicting with a snapshot another writer committed, as catalog compaction does, reloads the table and
/// retries with the files it wrote; the last conflict raises `ExportCommitConflict`, and the exporter deletes every
/// file that export wrote.
#[test]
fn a_conflicting_commit_reloads_and_retries_then_refuses_and_removes_its_files() {
    let remote = Remote::start();
    let (dir, public, token) = project(&remote.block("spans-lake", "svc://mirror", 2));
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 2));
    ok(&export(p, "spans-lake", &public, &token));
    land(p, "load-2", "2030-01-01T00:01:00Z", &ids("b", 1));

    remote.catalog.script(&[Script::Compact]);
    ok(&export(p, "spans-lake", &public, &token));
    assert_eq!(remote.catalog.commits(), 3, "one create commit, one conflict, one retry");
    assert_eq!(strings(&remote.scan(), "span_id"), ["a000", "a001", "b000"]);
    assert_eq!(remote.operations(), [Operation::Append, Operation::Replace, Operation::Overwrite]);

    land(p, "load-3", "2030-01-01T00:02:00Z", &ids("c", 1));
    let before = remote.s3.keys();
    remote.catalog.script(&[Script::Compact, Script::Compact, Script::Compact]);
    let refused = err(&export(p, "spans-lake", &public, &token));
    assert!(refused.contains("ExportCommitConflict"), "{refused}");
    assert_eq!(remote.catalog.commits(), 6);
    let after = remote.s3.keys();
    assert_eq!(after, before, "the refused export leaves no data or manifest file behind");
    assert_eq!(strings(&remote.scan(), "span_id"), ["a000", "a001", "b000"]);
}

/// A commit whose outcome is unknown deletes no file; the next run reloads the table and records the commit as taken
/// when `main` names its snapshot id.
#[test]
fn an_unknown_commit_outcome_keeps_its_files_and_the_next_run_records_it() {
    let remote = Remote::start();
    let (dir, public, token) = project(&remote.block("spans-lake", "svc://mirror", 2));
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 2));
    remote.catalog.script(&[Script::AppliedThenAnswer(503)]);
    let failed = err(&export(p, "spans-lake", &public, &token));
    assert!(failed.contains("outcome is unknown"), "{failed}");
    assert_eq!(remote.data_objects().len(), 1, "no file is deleted");
    assert_eq!(strings(&remote.scan(), "span_id"), ids("a", 2));

    let out = ok(&export(p, "spans-lake", &public, &token));
    assert!(out.contains("unchanged"), "{out}");
    assert_eq!(remote.catalog.commits(), 1, "the taken commit is recorded, not repeated");

    // A refused commit takes no effect: its files go, and the next run commits afresh.
    land(p, "load-2", "2030-01-01T00:01:00Z", &ids("b", 1));
    let before = remote.s3.keys();
    remote.catalog.script(&[Script::Refuse(400)]);
    err(&export(p, "spans-lake", &public, &token));
    assert_eq!(remote.s3.keys(), before);
    ok(&export(p, "spans-lake", &public, &token));
    assert_eq!(strings(&remote.scan(), "span_id"), ["a000", "a001", "b000"]);
}

/// A table configuration naming a storage host the block does not declare raises `ExportStorageUndeclared` before any
/// file is written.
#[test]
fn a_table_configuration_naming_an_undeclared_storage_host_refuses() {
    let remote = Remote::start();
    let (dir, public, token) = project(&remote.block("spans-lake", "svc://mirror", 2));
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 2));
    remote.catalog.name_storage("http://localhost:9");
    let refused = err(&export(p, "spans-lake", &public, &token));
    assert!(refused.contains("ExportStorageUndeclared"), "{refused}");
    assert!(remote.s3.keys().is_empty());
    assert_eq!(remote.s3.puts(), 0);
}

/// A token reference no adapter resolves refuses at preflight, before any row is read or any catalog request leaves.
#[test]
fn an_unresolved_token_refuses_before_any_catalog_request() {
    let remote = Remote::start();
    let (dir, public, token) = project(&remote.block("spans-lake", "svc://mirror", 2));
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 2));
    let out = cf(p, &["export", "run", "spans-lake", "--project", "research", "--public-key", &public, "--audience", AUD], &[("CONTEXTFUL_TOKEN", &token)]);
    let refused = err(&out);
    assert!(refused.contains("catalog-token"), "{refused}");
    assert!(remote.catalog.metadata(NS, TABLE).is_none());
    assert_eq!(remote.catalog.commits(), 0);
}

/// An export publishes for its one declared credential: another admitted credential refuses before any catalog request.
#[test]
fn an_export_publishes_for_its_declared_credential_alone() {
    let remote = Remote::start();
    let (dir, public, _) = project(&remote.block("spans-lake", "svc://mirror", 2));
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 2));
    let other = mint(p, "svc://other");
    let refused = err(&export(p, "spans-lake", &public, &other));
    assert!(refused.contains("svc://mirror") && refused.contains("svc://other"), "{refused}");
    assert!(remote.catalog.metadata(NS, TABLE).is_none());
}

/// The export reads through the read face under the admitted credential, so a mask holds in the published copy.
#[test]
fn the_published_copy_holds_the_credential_s_masks() {
    let remote = Remote::start();
    let manifest = format!("[[pipeline.tables]]\nname = \"spans\"\n[pipeline.tables.policy.columns]\nspan_id = {{ strategy = \"hash\" }}\n\n{}", remote.block("spans-lake", "svc://mirror", 2));
    let (dir, public, token) = project(&manifest);
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 2));
    ok(&export(p, "spans-lake", &public, &token));
    let published = strings(&remote.scan(), "span_id");
    assert_eq!(published.len(), 2);
    for id in &published {
        assert!(!ids("a", 2).contains(id), "the mask did not hold: `{id}` left unmasked");
    }
}
