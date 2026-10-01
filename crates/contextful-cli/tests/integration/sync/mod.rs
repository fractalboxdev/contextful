//! `contextful sync` through the built binary: the endpoint a `[sync]` block names, its
//! credentials, and the pull a run, a pipeline or the tool server takes before it reads.

mod s3;
mod without_s3;

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const STORE: &str = ".contextful/context/research";
const AUD: &str = "contextful://acme-research";
const LANDED: &str = "tables/filings/data/runs/run-1/ingest-a/part-00000.parquet";

/// A project whose store declares `sync`, beside a plan whose connector lands no rows.
fn project(id: &str, sync: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(STORE);
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), format!("[node]\nid = \"{id}\"\n\n[sync]\n{sync}")).unwrap();
    std::fs::write(dir.path().join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{}", "[[pipeline.tables]]\nname = \"filings\"\n")).unwrap();
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
    Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(args)
        .current_dir(dir)
        .env_remove("CONTEXTFUL_NODE_ID")
        .env_remove("CONTEXTFUL_SECRETS_BACKEND")
        .env_remove("CONTEXTFUL_SECRETS_ALLOW_ENV_TEMPLATES")
        .envs(env.iter().copied())
        .output()
        .unwrap()
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

/// Node `ingest-a` lands one run of `filings` and pushes it through `sync`.
fn seed(sync: &str, env: &[(&str, &str)]) -> tempfile::TempDir {
    let a = project("ingest-a", sync);
    std::fs::write(a.path().join("batch.jsonl"), "{\"id\":1}\n").unwrap();
    ok(&cf(
        a.path(),
        &["context", "land", "filings", "--project", "research", "--rows", "batch.jsonl", "--run-id", "run-1", "--site-id", "site", "--now", "2030-01-01T00:00:00Z"],
        env,
    ));
    ok(&cf(a.path(), &["sync", "push", "--project", "research"], env));
    assert_eq!(files(a.path()), [LANDED]);
    a
}

/// A loopback vendor answering every request with an empty page.
fn empty_vendor() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                    break;
                }
            }
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]");
        }
    });
    url
}

/// Start `contextful mcp` on `dir` with a credential reading `filings`, and send it one query.
fn mcp_query(dir: &Path) -> Output {
    let p = dir;
    std::fs::create_dir_all(p.join(".contextful")).unwrap();
    std::fs::write(p.join(".contextful/issuance.toml"), format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n")).unwrap();
    let public = ok(&cf(p, &["token", "keygen", "--out", ".contextful/issuer.seed"], &[]));
    let token = ok(&cf(
        p,
        &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--table", "filings", "--ttl", "600"],
        &[],
    ));
    let mut child = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["mcp", "--project", "research", "--public-key", &public, "--audience", AUD])
        .current_dir(p)
        .env_remove("CONTEXTFUL_NODE_ID")
        .env("CONTEXTFUL_TOKEN", token)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        for m in [
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "context.query", "arguments": { "sql": "SELECT id FROM filings" } } }),
        ] {
            let _ = writeln!(stdin, "{m}");
        }
    }
    child.wait_with_output().unwrap()
}

/// With `[sync] pull_before_run = true`, `run start`, `pipeline run` and `mcp` pull every table of the bucket into
/// the store before their first read, and a failed pull stops the command.
// spec: store.pull.before-run@bc38232e
#[test]
fn a_cold_node_pulls_the_bucket_before_its_run_reads() {
    let bucket = tempfile::tempdir().unwrap();
    let _a = seed(&file_sync(bucket.path(), ""), &[]);

    // A cold node declaring `pull_before_run` starts its run on the bucket's runs.
    let cold = project("ingest-c", &file_sync(bucket.path(), "pull_before_run = true\n"));
    let out = start(cold.path(), "c1");
    ok(&out);
    assert!(String::from_utf8_lossy(&out.stderr).contains("pull_before_run"), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(files(cold.path()), [LANDED]);

    // A cold node firing a declared pipeline pulls before the pipeline reads.
    let fired = project("ingest-f", &file_sync(bucket.path(), "pull_before_run = true\n"));
    std::fs::create_dir_all(fired.path().join("pipelines")).unwrap();
    std::fs::write(
        fired.path().join("pipelines/feed.toml"),
        format!("id = \"feed\"\ntables = [\"filings\"]\n[source]\nname = \"http\"\nconfig = {{ endpoint = \"{}\" }}\n", empty_vendor()),
    )
    .unwrap();
    let out = cf(fired.path(), &["pipeline", "run", "feed", "--project", "research", "--run-id", "f1", "--site-id", "site", "--now", "2030-01-01T01:00:00Z"], &[]);
    ok(&out);
    assert!(String::from_utf8_lossy(&out.stderr).contains("pull_before_run"), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(files(fired.path()).iter().any(|f| f == LANDED), "{:?}", files(fired.path()));

    // A cold tool server answers its first query from the bucket's rows.
    let served = project("ingest-m", &file_sync(bucket.path(), "pull_before_run = true\n"));
    let out = mcp_query(served.path());
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let lines: Vec<Value> = String::from_utf8_lossy(&out.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[1]["result"]["structuredContent"]["rows"], json!([["1"]]), "{:?}", lines[1]);

    // A node that does not declare it reads its own disk alone.
    let plain = project("ingest-d", &file_sync(bucket.path(), ""));
    ok(&start(plain.path(), "d1"));
    refused(&cf(plain.path(), &["context", "files", "filings", "--project", "research"], &[]), "StoreUnknownTable");

    // A pull that fails stops the run before it reads.
    let broken = project("ingest-e", "endpoint = \"ftp://objects.example.org\"\nbucket = \"context-team\"\nprefix = \"team\"\npull_before_run = true\n");
    refused(&start(broken.path(), "e1"), "SyncEndpointUnsupported");
    refused(&cf(broken.path(), &["pipeline", "run", "feed", "--project", "research", "--site-id", "site"], &[]), "SyncEndpointUnsupported");
    let out = mcp_query(broken.path());
    refused(&out, "SyncEndpointUnsupported");
    assert!(out.stdout.is_empty(), "a server whose pull fails writes no framing");
}
