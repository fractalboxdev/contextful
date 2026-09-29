//! `contextful sync` through the built binary: the endpoint a `[sync]` block names, its
//! credentials, and the pull a run or the tool server takes before it reads.

use std::path::Path;
use std::process::{Command, Output};

const STORE: &str = ".contextful/context/research";

/// A project whose store declares `sync`, beside a plan whose connector lands no rows.
fn project(id: &str, sync: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(STORE);
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), format!("[node]\nid = \"{id}\"\n\n[sync]\n{sync}")).unwrap();
    std::fs::write(dir.path().join("contextful.toml"), "[[pipeline.tables]]\nname = \"filings\"\n").unwrap();
    std::fs::write(dir.path().join("empty.sh"), "printf '{\"rows\":[],\"more\":false}'\n").unwrap();
    std::fs::write(
        dir.path().join("feed.toml"),
        "pipeline = \"feed\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1\"\ncommand = [\"sh\", \"empty.sh\"]\n",
    )
    .unwrap();
    dir
}

fn file_sync(bucket: &Path, extra: &str) -> String {
    format!("endpoint = \"file://{}\"\nbucket = \"context-team\"\nprefix = \"team\"\ncoordination = \"single-writer\"\n{extra}", bucket.display())
}

fn cf(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful")).args(args).current_dir(dir).env_remove("CONTEXTFUL_NODE_ID").envs(env.iter().copied()).output().unwrap()
}

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn refused(out: &Output, error: &str) {
    assert!(!out.status.success(), "expected {error}, got success: {}", String::from_utf8_lossy(&out.stdout));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(error), "expected {error}, got: {stderr}");
}

fn start(dir: &Path, run: &str) -> Output {
    cf(dir, &["run", "start", "--plan", "feed.toml", "--project", "research", "--run-id", run, "--site-id", "site", "--now", "2030-01-01T01:00:00Z"], &[])
}

fn files(dir: &Path) -> Vec<String> {
    ok(&cf(dir, &["context", "files", "filings", "--project", "research"], &[])).lines().map(str::to_string).collect()
}

/// With `[sync] pull_before_run = true`, `run start`, `pipeline run` and `mcp` pull every table of the bucket into
/// the store before their first read, and a failed pull stops the command.
// spec: store.pull.before-run@bc38232e
#[test]
fn a_cold_node_pulls_the_bucket_before_its_run_reads() {
    let bucket = tempfile::tempdir().unwrap();
    let a = project("ingest-a", &file_sync(bucket.path(), ""));
    std::fs::write(a.path().join("batch.jsonl"), "{\"id\":1}\n").unwrap();
    ok(&cf(
        a.path(),
        &["context", "land", "filings", "--project", "research", "--rows", "batch.jsonl", "--run-id", "run-1", "--site-id", "site", "--now", "2030-01-01T00:00:00Z"],
        &[],
    ));
    ok(&cf(a.path(), &["sync", "push", "--project", "research"], &[]));
    let landed = files(a.path());
    assert_eq!(landed, ["tables/filings/data/runs/run-1/ingest-a/part-00000.parquet"]);

    // A cold node declaring `pull_before_run` starts its run on the bucket's runs.
    let cold = project("ingest-c", &file_sync(bucket.path(), "pull_before_run = true\n"));
    let out = start(cold.path(), "c1");
    ok(&out);
    assert!(String::from_utf8_lossy(&out.stderr).contains("pull_before_run"), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(files(cold.path()), landed);

    // A node that does not declare it reads its own disk alone.
    let plain = project("ingest-d", &file_sync(bucket.path(), ""));
    ok(&start(plain.path(), "d1"));
    refused(&cf(plain.path(), &["context", "files", "filings", "--project", "research"], &[]), "StoreUnknownTable");

    // A pull that fails stops the run before it reads.
    let broken = project("ingest-e", "endpoint = \"ftp://objects.example.org\"\nbucket = \"context-team\"\nprefix = \"team\"\npull_before_run = true\n");
    refused(&start(broken.path(), "e1"), "SyncEndpointUnsupported");
    refused(&cf(broken.path(), &["pipeline", "run", "feed", "--project", "research", "--site-id", "site"], &[]), "SyncEndpointUnsupported");
}

/// An S3 endpoint refuses before a request leaves: an unset credential variable, a literal key, and plaintext
/// off loopback.
#[cfg(feature = "s3-sync")]
#[test]
fn an_s3_endpoint_refuses_unbound_credentials_and_plaintext() {
    let s3 = |endpoint: &str, keys: &str| format!("endpoint = \"{endpoint}\"\nbucket = \"context-team\"\nprefix = \"team\"\n{keys}");
    let unset = project("ingest-a", &s3("http://127.0.0.1:9", "access_key_id = \"env://CONTEXTFUL_TEST_UNSET_KEY\"\nsecret_access_key = \"env://CONTEXTFUL_TEST_UNSET_SECRET\"\n"));
    refused(&cf(unset.path(), &["sync", "probe", "--project", "research"], &[]), "SyncCredentialUnbound");
    let literal = project("ingest-a", &s3("r2://account", "access_key_id = \"AKIAIOSFODNN7EXAMPLE\"\nsecret_access_key = \"env://S\"\n"));
    let out = cf(literal.path(), &["sync", "push", "--project", "research"], &[]);
    refused(&out, "SyncCredentialUnbound");
    assert!(!String::from_utf8_lossy(&out.stderr).contains("AKIAIOSFODNN7EXAMPLE"), "the refusal never repeats the literal");
    let plaintext = project("ingest-a", &s3("http://objects.example.org", "access_key_id = \"env://A\"\nsecret_access_key = \"env://S\"\n"));
    refused(&cf(plaintext.path(), &["sync", "pull", "--project", "research"], &[]), "SyncEndpointInsecure");
    // A `secret://` reference no assembled adapter answers refuses by name.
    let unresolved = project("ingest-a", &s3("http://127.0.0.1:9", "access_key_id = \"secret://sync-key-id\"\nsecret_access_key = \"secret://sync-secret\"\n"));
    refused(&cf(unresolved.path(), &["sync", "probe", "--project", "research"], &[]), "SecretUnresolvedReference");
}

/// A build without the S3 adapter refuses an S3 or R2 endpoint by name.
#[cfg(not(feature = "s3-sync"))]
#[test]
fn a_build_without_the_s3_adapter_refuses_an_s3_endpoint() {
    let p = project("ingest-a", "endpoint = \"r2://account\"\nbucket = \"context-team\"\nprefix = \"team\"\n");
    refused(&cf(p.path(), &["sync", "probe", "--project", "research"], &[]), "SyncEndpointUnsupported");
}
