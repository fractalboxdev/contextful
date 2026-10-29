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
        self.block_in(name, credential, NS, version)
    }

    fn block_in(&self, name: &str, credential: &str, namespace: &str, version: u8) -> String {
        format!(
            "[[export]]\nname = \"{name}\"\ntable = \"spans\"\nformat = \"iceberg\"\ncredential = \"{credential}\"\n\
             catalog = \"{}\"\nstorage = \"{}\"\nwarehouse = \"exports\"\nnamespace = \"{namespace}\"\ntable_name = \"{TABLE}\"\n\
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
// spec: run.export-iceberg.create@44e94a58
// spec: run.export-iceberg.overwrite@428b32d8
// spec: run.export-iceberg.retention@1dc99459
// spec: run.export-iceberg.read@f96b87c1
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
        let names: Vec<&str> = batch.schema_ref().fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(names, ["duration_ms", "span_id"], "the engine's own columns stay its own");
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
// spec: run.export-iceberg.frontier@b4d4def0
#[test]
fn an_unchanged_export_commits_nothing() {
    let remote = Remote::start();
    // This catalog vends its credentials in the table `config`.
    remote.catalog.vend_in_config();
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
// spec: run.export-iceberg.commit-retry@1c61cfa9
// spec: run.export-iceberg.orphans@6fa7ecf4
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

/// A commit whose outcome is unknown deletes no file while it may still apply; a later run records it as taken when
/// the table holds its snapshot, and deletes its files once another table stands or `main` leaves its parent.
// spec: run.export-iceberg.commit-unknown@2c6ded8c
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

    // An unknown outcome the catalog never applied: the next run finds no such snapshot and deletes its files.
    land(p, "load-3", "2030-01-01T00:02:00Z", &ids("c", 1));
    let before = remote.s3.keys();
    remote.catalog.script(&[Script::NotAppliedThenAnswer(504)]);
    assert!(err(&export(p, "spans-lake", &public, &token)).contains("outcome is unknown"));
    let unknown: Vec<String> = remote.s3.keys().into_iter().filter(|k| !before.contains(k)).collect();
    assert!(!unknown.is_empty(), "the unknown attempt keeps its files");
    ok(&export(p, "spans-lake", &public, &token));
    let after = remote.s3.keys();
    assert!(unknown.iter().all(|k| !after.contains(k)), "the untaken attempt's files are deleted: {unknown:?}");
    assert_eq!(strings(&remote.scan(), "span_id"), ["a000", "a001", "b000", "c000"]);
}

/// A source whose columns no longer match the table's current schema raises `ExportSchemaChanged` before any file is
/// written; the export evolves no schema.
// spec: run.export-iceberg.schema-changed@1d69fa1b
#[test]
fn a_source_schema_change_refuses_and_writes_no_file() {
    let remote = Remote::start();
    let (dir, public, token) = project(&remote.block("spans-lake", "svc://mirror", 2));
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 2));
    ok(&export(p, "spans-lake", &public, &token));
    std::fs::write(p.join("load-2.jsonl"), "{\"span_id\":\"b000\",\"duration_ms\":3,\"status\":\"ok\"}\n").unwrap();
    ok(&cf(p, &["context", "land", "spans", "--project", "research", "--rows", "load-2.jsonl", "--run-id", "load-2", "--site-id", "site", "--now", "2030-01-01T00:01:00Z"], &[]));
    let before = remote.s3.keys();
    let refused = err(&export(p, "spans-lake", &public, &token));
    assert!(refused.contains("ExportSchemaChanged"), "{refused}");
    assert_eq!(remote.s3.keys(), before, "no file is written");
    assert_eq!(remote.catalog.commits(), 1);
    assert_eq!(strings(&remote.scan(), "span_id"), ids("a", 2));
}

/// A table configuration naming a storage host the block does not declare raises `ExportStorageUndeclared` before any
/// file is written.
// spec: run.export-iceberg.egress@24ccf4f3
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
    assert!(remote.catalog.metadata(NS, TABLE).is_none(), "the table the export created for it is dropped");
}

/// A token reference no adapter resolves refuses at preflight, before any row is read or any catalog request leaves.
// spec: run.export-iceberg.token-preflight@bc91b669
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
// spec: run.export-iceberg.read@f96b87c1
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

/// A column the credential's mask rewrites publishes as the text the read face answers, never as its stored type.
// spec: run.export-iceberg.masked@93752243
#[test]
fn a_masked_column_publishes_as_the_text_the_read_face_answers() {
    let remote = Remote::start();
    let manifest = format!("[[pipeline.tables]]\nname = \"spans\"\n[pipeline.tables.policy.columns]\nduration_ms = {{ strategy = \"hash\" }}\n\n{}", remote.block("spans-lake", "svc://mirror", 2));
    let (dir, public, token) = project(&manifest);
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 2));
    ok(&export(p, "spans-lake", &public, &token));
    let digests = strings(&remote.scan(), "duration_ms");
    assert_eq!(digests.len(), 2);
    for digest in &digests {
        assert_eq!(digest.len(), 32, "{digest}");
        assert!(digest.bytes().all(|b| b.is_ascii_hexdigit()), "{digest}");
    }
    assert_eq!(strings(&remote.scan(), "span_id"), ids("a", 2));
}

/// A queue wake runs every export of the woken table under one credential: an Iceberg export publishing for another
/// credential is skipped, not failed, so per-audience exports of one table each publish under their own wake.
// spec: run.export-iceberg.wake-audience@3eb0e63e
#[test]
fn a_wake_skips_an_iceberg_export_publishing_for_another_credential() {
    let remote = Remote::start();
    let manifest = format!("{}{}", remote.block_in("spans-mirror", "svc://mirror", "mirror", 2), remote.block_in("spans-other", "svc://other", "other", 2));
    let (dir, public, token) = project(&manifest);
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 2));
    let env = [("CONTEXTFUL_TOKEN", token.as_str()), ("CATALOG_TOKEN", TOKEN), ("CONTEXTFUL_SECRETS_ALLOW_ENV_TEMPLATES", "1")];
    let out = cf(p, &["queue", "drain", "--project", "research", "--public-key", &public, "--audience", AUD], &env);
    let said = format!("{}{}", ok(&out), String::from_utf8_lossy(&out.stderr));
    assert!(said.contains("spans-other") && said.contains("svc://other"), "{said}");
    assert_eq!(strings(&remote.catalog.scan(&remote.s3, "mirror", TABLE), "span_id"), ids("a", 2));
    assert!(remote.catalog.metadata("other", TABLE).is_none(), "the other audience's export waits for its own credential");
}

/// A commit whose outcome is unknown may still be in flight: while `main` stays at its parent the next run keeps its
/// files, and the held commit landing first turns the next commit into a retry over it.
// spec: run.export-iceberg.commit-unknown@2c6ded8c
#[test]
fn an_in_flight_commit_keeps_its_files_until_main_moves_past_its_parent() {
    let remote = Remote::start();
    let (dir, public, token) = project(&remote.block("spans-lake", "svc://mirror", 2));
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 1));
    ok(&export(p, "spans-lake", &public, &token));
    land(p, "load-2", "2030-01-01T00:01:00Z", &ids("b", 1));
    remote.catalog.script(&[Script::Deferred(504)]);
    assert!(err(&export(p, "spans-lake", &public, &token)).contains("outcome is unknown"));
    land(p, "load-3", "2030-01-01T00:02:00Z", &ids("c", 1));
    ok(&export(p, "spans-lake", &public, &token));
    assert_eq!(strings(&remote.scan(), "span_id"), ["a000", "b000", "c000"]);
    let metadata = remote.catalog.metadata(NS, TABLE).unwrap();
    assert_eq!(metadata.snapshots().count(), 3, "the held commit landed, and the next export overwrote it");
}

/// Any 2xx takes the commit; a status that leaves the outcome open, such as a proxy's 408, deletes no file.
// spec: run.export-iceberg.commit-status@217d3293
#[test]
fn a_2xx_takes_the_commit_and_an_open_status_keeps_the_files() {
    let remote = Remote::start();
    let (dir, public, token) = project(&remote.block("spans-lake", "svc://mirror", 2));
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 1));
    remote.catalog.script(&[Script::AppliedThenAnswer(204)]);
    assert!(ok(&export(p, "spans-lake", &public, &token)).contains("committed 1 rows"));
    assert!(ok(&export(p, "spans-lake", &public, &token)).contains("unchanged"));
    land(p, "load-2", "2030-01-01T00:01:00Z", &ids("b", 1));
    remote.catalog.script(&[Script::AppliedThenAnswer(408)]);
    assert!(err(&export(p, "spans-lake", &public, &token)).contains("outcome is unknown"));
    assert_eq!(strings(&remote.scan(), "span_id"), ["a000", "b000"], "the applied commit's files remain");
    assert!(ok(&export(p, "spans-lake", &public, &token)).contains("unchanged"));
}

/// A commit requires the table uuid, `main`, the current schema id and the default spec id it was built against; a
/// table recreated or evolved between load and commit refuses as `ExportCommitConflict` and the export's files go.
// spec: run.export-iceberg.overwrite@428b32d8
// spec: run.export-iceberg.commit-retry@1c61cfa9
#[test]
fn a_table_recreated_or_evolved_under_the_export_refuses_its_commit() {
    for script in [Script::Recreate, Script::EvolveSchema] {
        let remote = Remote::start();
        let (dir, public, token) = project(&remote.block("spans-lake", "svc://mirror", 2));
        let p = dir.path();
        land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 1));
        ok(&export(p, "spans-lake", &public, &token));
        land(p, "load-2", "2030-01-01T00:01:00Z", &ids("b", 1));
        let before = remote.s3.keys();
        remote.catalog.script(&[script]);
        let refused = err(&export(p, "spans-lake", &public, &token));
        assert!(refused.contains("ExportCommitConflict"), "{script:?}: {refused}");
        assert_eq!(remote.s3.keys(), before, "{script:?}: the export's files go");
    }
}

/// An export writes unpartitioned tables: a table another engine partitioned refuses before any file is written.
// spec: run.export-iceberg.table-shape@afad5bc4
#[test]
fn a_partitioned_table_refuses_before_any_file_is_written() {
    let remote = Remote::start();
    let (dir, public, token) = project(&remote.block("spans-lake", "svc://mirror", 2));
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 1));
    ok(&export(p, "spans-lake", &public, &token));
    remote.catalog.partition_by(NS, TABLE, "span_id");
    land(p, "load-2", "2030-01-01T00:01:00Z", &ids("b", 1));
    let before = remote.s3.keys();
    let refused = err(&export(p, "spans-lake", &public, &token));
    assert!(refused.contains("partition"), "{refused}");
    assert_eq!(remote.s3.keys(), before);
}

/// An unknown commit against a table dropped since keeps its record until a table stands again, whose other uuid
/// proves the commit never took; its files then go.
// spec: run.export-iceberg.commit-unknown@2c6ded8c
#[test]
fn an_unknown_commit_against_a_dropped_table_deletes_its_files_once_a_table_stands() {
    let remote = Remote::start();
    let (dir, public, token) = project(&remote.block("spans-lake", "svc://mirror", 2));
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 1));
    ok(&export(p, "spans-lake", &public, &token));
    let before = remote.s3.keys();
    land(p, "load-2", "2030-01-01T00:01:00Z", &ids("b", 1));
    remote.catalog.script(&[Script::NotAppliedThenAnswer(503)]);
    err(&export(p, "spans-lake", &public, &token));
    let untaken: Vec<String> = remote.s3.keys().into_iter().filter(|k| !before.contains(k)).collect();
    assert!(!untaken.is_empty());
    remote.catalog.drop_table(NS, TABLE);
    ok(&export(p, "spans-lake", &public, &token));
    let after = remote.s3.keys();
    assert!(untaken.iter().all(|k| !after.contains(k)), "{untaken:?}");
    assert_eq!(strings(&remote.scan(), "span_id"), ["a000", "b000"]);
}

/// A table standing at another format version than the block declares refuses; the export commits at the declared one.
// spec: run.export-iceberg.table-shape@afad5bc4
#[test]
fn a_table_at_another_format_version_refuses() {
    let remote = Remote::start();
    let (dir, public, token) = project(&remote.block("spans-lake", "svc://mirror", 2));
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 1));
    ok(&export(p, "spans-lake", &public, &token));
    let manifest = std::fs::read_to_string(p.join("contextful.toml")).unwrap().replace("format_version = 2", "format_version = 3");
    std::fs::write(p.join("contextful.toml"), manifest).unwrap();
    let refused = err(&export(p, "spans-lake", &public, &token));
    assert!(refused.contains("format version 2") && refused.contains("declares 3"), "{refused}");
    assert_eq!(remote.catalog.commits(), 1);
}

/// Characters a JSON string spells as themselves: printable ASCII less `"` and `\`.
const PRINTABLE: &[u8] = b" !#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[]^_`abcdefghijklmnopqrstuvwxyz{|}~";

/// `len` characters from a seeded SplitMix64 stream, which zstd cannot shrink below their entropy.
fn noise(state: &mut u64, len: usize) -> String {
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        let take = (len - out.len()).min(8);
        out.extend(z.to_le_bytes()[..take].iter().map(|b| PRINTABLE[*b as usize % PRINTABLE.len()]));
    }
    String::from_utf8(out).unwrap()
}

/// Land `(span_id, payload)` rows into `spans` as run `run`.
fn land_payloads(dir: &Path, run: &str, now: &str, rows: impl Iterator<Item = (String, String)>) {
    let file = format!("{run}.jsonl");
    let mut out = std::io::BufWriter::new(std::fs::File::create(dir.join(&file)).unwrap());
    for (id, payload) in rows {
        for part in [format!("{{\"span_id\":\"{id}\",\"payload\":\""), payload, "\"}\n".to_string()] {
            std::io::Write::write_all(&mut out, part.as_bytes()).unwrap();
        }
    }
    drop(out);
    ok(&cf(dir, &["context", "land", "spans", "--project", "research", "--rows", &file, "--run-id", run, "--site-id", "site", "--now", now], &[]));
    std::fs::remove_file(dir.join(&file)).unwrap();
}

/// A Parquet object's footer.
fn footer(bytes: &[u8]) -> parquet::file::metadata::ParquetMetaData {
    use parquet::file::metadata::{FooterTail, ParquetMetaDataReader};
    let tail: &[u8; 8] = bytes[bytes.len() - 8..].try_into().unwrap();
    let length = FooterTail::try_new(tail).unwrap().metadata_length();
    ParquetMetaDataReader::decode_metadata(&bytes[bytes.len() - 8 - length..bytes.len() - 8]).unwrap()
}

/// A Parquet object's row count, read from its footer.
fn parquet_rows(bytes: &[u8]) -> i64 {
    footer(bytes).file_metadata().num_rows()
}

/// Every data object of `remote` in write order, as its size in bytes and its row count.
fn data_files(remote: &Remote) -> Vec<(u64, i64)> {
    let mut keys = remote.data_objects();
    keys.sort();
    keys.iter().map(|k| remote.s3.object(k).unwrap()).map(|b| (b.len() as u64, parquet_rows(&b))).collect()
}

/// Rows whose encoded bytes pass the data file ceiling fill a file toward it and open the next before crossing it;
/// one row larger than the ceiling fills a file alone, apart from the rows on either side of it.
// spec: run.export-iceberg.file-size@0e2de590
#[test]
fn data_files_stay_under_the_ceiling_and_an_oversized_row_fills_one_alone() {
    // 11,000 rows of 16 KiB noise encode to about 141 MiB; eight runs bound each landing.
    const ROWS: usize = 11_000;
    const PAYLOAD: usize = 16 * 1024;
    const RUNS: usize = 8;
    let ceiling = contextful_core::export::iceberg::ICEBERG_DATA_FILE_BYTES;
    let mut seed = 0x5eed_u64;
    {
        let remote = Remote::start();
        let (dir, public, token) = project(&remote.block("spans-lake", "svc://mirror", 2));
        let p = dir.path();
        for run in 0..RUNS {
            let ids = run * ROWS / RUNS..(run + 1) * ROWS / RUNS;
            land_payloads(p, &format!("load-{run}"), &format!("2030-01-01T00:00:{run:02}Z"), ids.map(|i| (format!("r{i:05}"), noise(&mut seed, PAYLOAD))));
        }
        let out = ok(&export(p, "spans-lake", &public, &token));
        assert!(out.contains(&format!("committed {ROWS} rows in 2 files")), "{out}");
        let files = data_files(&remote);
        let [first, second] = files[..] else { panic!("two data files: {files:?}") };
        assert_eq!(first.1 + second.1, ROWS as i64, "{files:?}");
        assert!(first.0 + second.0 > ceiling, "the rows encode past the ceiling, so no one file holds them: {files:?}");
        assert!(first.0 <= ceiling && second.0 <= ceiling, "no file passes the ceiling: {files:?}");
        assert!(first.0 > ceiling / 2, "the first file fills toward the ceiling before the next opens: {files:?}");
    }
    let remote = Remote::start();
    let (dir, public, token) = project(&remote.block("spans-lake", "svc://mirror", 2));
    let p = dir.path();
    land_payloads(p, "load-0", "2030-01-01T00:00:00Z", std::iter::once(("s0".to_string(), noise(&mut seed, 64))));
    land_payloads(p, "load-1", "2030-01-01T00:01:00Z", std::iter::once(("big".to_string(), "x".repeat(ceiling as usize + 1024))));
    land_payloads(p, "load-2", "2030-01-01T00:02:00Z", std::iter::once(("s1".to_string(), noise(&mut seed, 64))));
    let out = ok(&export(p, "spans-lake", &public, &token));
    assert!(out.contains("committed 3 rows in 3 files"), "{out}");
    let files = data_files(&remote);
    assert_eq!(files.iter().map(|f| f.1).collect::<Vec<_>>(), [1, 1, 1], "the oversized row fills the middle file alone: {files:?}");
}

/// The size of each multipart upload part but the last.
const PART: u64 = 16 * 1024 * 1024;

/// Land 3,000 rows of 16 KiB noise, which encode to one data file of about 38 MiB.
fn land_three_parts(dir: &Path) {
    let mut seed = 0x0b1e_u64;
    for run in 0..2 {
        let ids = run * 1500..(run + 1) * 1500;
        land_payloads(dir, &format!("load-{run}"), &format!("2030-01-01T00:00:{run:02}Z"), ids.map(|i| (format!("r{i:05}"), noise(&mut seed, 16 * 1024))));
    }
}

/// A data file past one part streams as a multipart upload: every part but the last holds 16 MiB, the parts together
/// are the file, and the upload completes when the file closes; a file under one part uploads in one request.
// spec: run.export-iceberg.multipart@ee07f8c2
#[test]
fn a_data_file_past_one_part_streams_as_a_multipart_upload() {
    let remote = Remote::start();
    let (dir, public, token) = project(&remote.block("spans-lake", "svc://mirror", 2));
    let p = dir.path();
    land_three_parts(p);
    let out = ok(&export(p, "spans-lake", &public, &token));
    assert!(out.contains("committed 3000 rows in 1 files"), "{out}");
    let files = data_files(&remote);
    let [(size, rows)] = files[..] else { panic!("one data file: {files:?}") };
    assert_eq!(rows, 3000);
    let parts = remote.s3.parts();
    let Some((last, whole)) = parts.split_last() else { panic!("the file uploads in parts") };
    assert!(whole.len() >= 2, "{parts:?}");
    assert!(whole.iter().all(|p| *p == PART) && *last <= PART, "every part but the last holds 16 MiB: {parts:?}");
    assert_eq!(parts.iter().sum::<u64>(), size, "the parts together are the file");
    assert!(remote.s3.uploads().is_empty(), "the upload completes");
    assert_eq!(strings(&remote.scan(), "span_id").len(), 3000);

    let small = Remote::start();
    let (dir, public, token) = project(&small.block("spans-lake", "svc://mirror", 2));
    land(dir.path(), "load-1", "2030-01-01T00:00:00Z", &ids("a", 2));
    ok(&export(dir.path(), "spans-lake", &public, &token));
    assert_eq!(small.data_objects().len(), 1);
    assert!(small.s3.parts().is_empty(), "a file under one part uploads in one request");
}

/// The writer closes each row group once its encoded size reaches 16 MiB, so a data file past one part holds several,
/// each within a row of that size, and the open one bounds what the writer holds besides the spooled part.
// spec: run.export-iceberg.memory@c221d4e4
#[test]
fn row_groups_close_at_their_encoded_size() {
    const ROW_GROUP: i64 = 16 * 1024 * 1024;
    const SLACK: i64 = 64 * 1024;
    let remote = Remote::start();
    let (dir, public, token) = project(&remote.block("spans-lake", "svc://mirror", 2));
    let p = dir.path();
    land_three_parts(p);
    ok(&export(p, "spans-lake", &public, &token));
    let keys = remote.data_objects();
    let [key] = &keys[..] else { panic!("one data file: {keys:?}") };
    let groups: Vec<i64> = footer(&remote.s3.object(key).unwrap()).row_groups().iter().map(|g| g.compressed_size()).collect();
    let Some((last, closed)) = groups.split_last() else { panic!("the file holds a row group") };
    assert!(closed.len() >= 2, "{groups:?}");
    assert!(closed.iter().all(|g| (ROW_GROUP - SLACK..=ROW_GROUP + SLACK).contains(g)), "each closed row group reaches 16 MiB and stops there: {groups:?}");
    assert!(*last <= ROW_GROUP + SLACK, "{groups:?}");
}

/// An export failing while a data file's multipart upload is open aborts the upload, so neither its parts nor a data
/// file stay behind; the next export writes the file afresh.
// spec: run.export-iceberg.orphans@6fa7ecf4
#[test]
fn a_failed_export_aborts_its_open_multipart_upload() {
    let remote = Remote::start();
    let (dir, public, token) = project(&remote.block("spans-lake", "svc://mirror", 2));
    let p = dir.path();
    land_three_parts(p);
    remote.s3.fail_parts(1, &["InternalError"]);
    err(&export(p, "spans-lake", &public, &token));
    assert_eq!(remote.s3.parts(), [PART], "the first part uploaded before the second failed");
    assert!(remote.s3.uploads().is_empty(), "the open upload is aborted");
    assert!(remote.data_objects().is_empty());
    ok(&export(p, "spans-lake", &public, &token));
    assert_eq!(data_files(&remote).len(), 1);
    assert!(remote.s3.uploads().is_empty());
}
