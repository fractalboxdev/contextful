//! `contextful sync` through the built binary: the endpoint a `[sync]` block names, its
//! credentials, and the pull a run, a pipeline or either tool server takes before it reads.

mod run_state;
mod node_id;
mod s3;
mod without_s3;

#[test]
fn push_reads_the_declared_control_snapshot_directory() {
    use contextful_core::issue::SignatureAlgorithm;
    use contextful_policy::control_receipt::ControlReceipt;
    use contextful_policy::issue::SeedSigner;

    let bucket = tempfile::tempdir().unwrap();
    let site = project("ingest-a", &file_sync(bucket.path(), ""));
    std::fs::write(site.path().join("contextful.toml"), "authoring_posture = 'per_request'\n[control]\nsnapshot_dir = 'ops/control'\n").unwrap();
    let control = site.path().join("ops/control");
    std::fs::create_dir_all(&control).unwrap();
    let snapshot = b"authoring_posture = 'per_request'\n";
    let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let receipt = ControlReceipt::sign("research", 1, None, snapshot, &signer).unwrap();
    std::fs::write(control.join("manifest@v1.toml"), snapshot).unwrap();
    std::fs::write(control.join("receipt@v1.json"), serde_json::to_vec(&receipt).unwrap()).unwrap();
    std::fs::write(control.join("manifest@current"), "1\n").unwrap();
    ok(&cf(site.path(), &["sync", "push", "--project", "research"], &[]));
    let manifest: Value = serde_json::from_slice(&std::fs::read(bucket.path().join("context-team/team/manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["control_heads"]["research"]["receipt_sha256"], receipt.digest());
    assert!(manifest["entries"]["research/control/receipt@v1.json"].is_object());
}

#[test]
fn a_cold_node_pulls_and_adopts_the_admin_attested_control_version() {
    let bucket = tempfile::tempdir().unwrap();
    let sync = file_sync(bucket.path(), "");
    let writer = project("ingest-a", &sync);
    let document = "authoring_posture = 'per_request'\n[[pipeline]]\nid = 'orders'\ntables = ['orders']\n[pipeline.source]\nname = 'http'\nconfig = { endpoint = 'https://api.vendor.example/v1' }\n";
    std::fs::write(writer.path().join("contextful.toml"), document).unwrap();
    std::fs::write(writer.path().join(".contextful/issuance.toml"),
        "default_audience = 'contextful://research'\nmax_lifetime_secs = 3600\n").unwrap();
    let public = ok(&cf(writer.path(), &["token", "keygen", "--out", ".contextful/issuer.seed"], &[]));
    let admin = ok(&cf(writer.path(), &["token", "mint", "--issuer-key", ".contextful/issuer.seed",
        "--on-behalf-of", "user://dana@example.test", "--ttl", "600", "--action", "admin", "--table", "*"], &[]));
    ok(&cf(writer.path(), &["pipeline", "import", "--project", "research", "--issuer-key", ".contextful/issuer.seed",
        "--public-key", &public, "--audience", "contextful://research"], &[("CONTEXTFUL_TOKEN", &admin)]));
    ok(&cf(writer.path(), &["sync", "push", "--project", "research"], &[]));

    let cold = project("ingest-b", &sync);
    std::fs::write(cold.path().join("contextful.toml"), document).unwrap();
    ok(&cf(cold.path(), &["sync", "pull", "--project", "research"], &[]));
    let pointer = cold.path().join(".contextful/control/research/manifest@current");
    assert!(!pointer.exists());
    let out = cf(cold.path(), &["pipeline", "serve", "--cycle", "--project", "research", "--public-key", &public], &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read_to_string(pointer).unwrap(), "1\n");
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(answer["unarmed"][0]["id"], "orders");
}

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

/// With `[sync] pull_before_run = true`, `run start`, `pipeline run`, `mcp` and `serve` pull every table of the
/// bucket into the store before their first read, and a failed pull stops the command.
// spec: store.pull.before-run@e77004a1
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

    a_cold_http_server_pulls_before_it_listens(bucket.path());
}

/// Start `contextful serve --http` on `dir`; its address once it listens, or its exit
/// output when it stops before binding.
fn serve(dir: &Path, public: &str) -> Result<(std::process::Child, String), Output> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["serve", "--http", "127.0.0.1:0", "--project", "research", "--public-key", public, "--audience", AUD, "--max-in-flight", "4"])
        .current_dir(dir)
        .env_remove("CONTEXTFUL_NODE_ID")
        .env_remove("CONTEXTFUL_ISSUER_PUBKEY")
        .env_remove("CONTEXTFUL_AUDIENCE")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let mut seen = String::new();
    let mut line = String::new();
    while err.read_line(&mut line).unwrap() > 0 {
        if let Some(addr) = line.trim().strip_prefix("listening on http://").and_then(|a| a.strip_suffix("/mcp")) {
            return Ok((child, addr.to_string()));
        }
        seen.push_str(&line);
        line.clear();
    }
    let mut out = child.wait_with_output().unwrap();
    out.stderr = seen.into_bytes();
    Err(out)
}

/// The issuer key a project serves under and a bearer reading `filings` minted by it.
fn issue(dir: &Path) -> (String, String) {
    std::fs::create_dir_all(dir.join(".contextful")).unwrap();
    std::fs::write(dir.join(".contextful/issuance.toml"), format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n")).unwrap();
    let public = ok(&cf(dir, &["token", "keygen", "--out", ".contextful/issuer.seed"], &[]));
    let token = ok(&cf(
        dir,
        &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--table", "filings", "--ttl", "600"],
        &[],
    ));
    (public, token)
}

/// `POST /mcp` carrying one `context.query` over `filings` under a bare bearer.
fn http_query(addr: &str, token: &str) -> Value {
    use std::io::Read;
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "context.query", "arguments": { "sql": "SELECT id FROM filings" } } }).to_string();
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    write!(s, "POST /mcp HTTP/1.1\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    serde_json::from_str(raw.split_once("\r\n\r\n").unwrap().1).unwrap()
}

/// A cold `serve --http` declaring `pull_before_run` answers from the bucket's rows, and
/// one whose pull fails binds no listener.
fn a_cold_http_server_pulls_before_it_listens(bucket: &Path) {
    // A cold network face answers its first query from the bucket's rows.
    let cold = project("ingest-h", &file_sync(bucket, "pull_before_run = true\n"));
    let (public, token) = issue(cold.path());
    let (mut child, addr) = serve(cold.path(), &public).unwrap_or_else(|out| panic!("{}", String::from_utf8_lossy(&out.stderr)));
    let answer = http_query(&addr, &token);
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(answer["result"]["structuredContent"]["rows"], json!([["1"]]), "{answer:?}");
    assert_eq!(files(cold.path()), [LANDED]);

    // A pull that fails refuses the start before the listener binds.
    let broken = project("ingest-x", "endpoint = \"ftp://objects.example.org\"\nbucket = \"context-team\"\nprefix = \"team\"\npull_before_run = true\n");
    let (public, _) = issue(broken.path());
    match serve(broken.path(), &public) {
        Ok((mut child, addr)) => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("a server whose pull fails listened on {addr}");
        }
        Err(out) => refused(&out, "SyncEndpointUnsupported"),
    }
}

/// A replica's `sync pull` reads `replicate = false` from a `pipelines/` file as from the
/// declaration: a refresh naming the table refuses, and an unscoped one leaves it behind.
// spec: store.declare.declaration-set@14e776d1
#[test]
fn a_pipeline_file_keeps_a_replicate_off_table_off_the_pull() {
    let bucket = tempfile::tempdir().unwrap();
    let _a = seed(&file_sync(bucket.path(), ""), &[]);
    let replica = project("replica-p", &file_sync(bucket.path(), "\n[replica]\nof = \"team/research\"\n"));
    std::fs::write(replica.path().join("contextful.toml"), "authoring_posture = \"per_request\"\n").unwrap();
    std::fs::create_dir_all(replica.path().join("pipelines")).unwrap();
    std::fs::write(replica.path().join("pipelines/store.toml"), "[pipeline]\n[[pipeline.tables]]\nname = \"filings\"\nreplicate = false\n").unwrap();
    refused(&cf(replica.path(), &["sync", "pull", "--project", "research", "--table", "filings"], &[]), "ReplicaSensitiveTable");
    ok(&cf(replica.path(), &["sync", "pull", "--project", "research"], &[]));
    refused(&cf(replica.path(), &["context", "files", "filings", "--project", "research"], &[]), "StoreUnknownTable");
}

/// A push records the pushing site's residency allow-set in the bucket manifest; a push finding a set another site
/// recorded that differs from its own raises `ResidencySitesDiverge` and commits nothing.
// spec: surface.reside.site-regions@64018045
#[test]
fn two_sites_declaring_different_residency_diverge_at_push() {
    let bucket = tempfile::tempdir().unwrap();
    let site = |node: &str, site: &str, residency: &str| {
        let dir = project(node, &file_sync(bucket.path(), ""));
        std::fs::write(dir.path().join("contextful.toml"), format!("site_id = \"{site}\"\n{residency}\n[[pipeline.tables]]\nname = \"filings\"\n")).unwrap();
        dir
    };
    let eu = "\n[residency]\nregions = [\"eu-west-1\"]\n";
    let a = site("ingest-a", "site-a", eu);
    ok(&cf(a.path(), &["sync", "push", "--project", "research"], &[]));
    let manifest = || -> Value { serde_json::from_str(&std::fs::read_to_string(bucket.path().join("context-team/team/manifest.json")).unwrap()).unwrap() };
    assert_eq!(manifest()["residency"], json!({ "site_id": "site-a", "regions": ["eu-west-1"] }));

    // The same set from another site pushes.
    ok(&cf(site("ingest-b", "site-b", eu).path(), &["sync", "push", "--project", "research"], &[]));
    // A differing set, or none, refuses and leaves the record as it stands.
    for (node, residency) in [("ingest-c", "\n[residency]\nregions = [\"us-east-1\"]\n"), ("ingest-d", "")] {
        let out = cf(site(node, "site-c", residency).path(), &["sync", "push", "--project", "research"], &[]);
        refused(&out, "ResidencySitesDiverge");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("site-c") && err.contains("eu-west-1"), "{err}");
    }
    assert_eq!(manifest()["residency"]["regions"], json!(["eu-west-1"]));

    // The site holding the record changes its own set; the next push records it.
    let widened = "\n[residency]\nregions = [\"eu-central-1\", \"eu-west-1\"]\n";
    ok(&cf(site("ingest-b2", "site-b", widened).path(), &["sync", "push", "--project", "research"], &[]));
    assert_eq!(manifest()["residency"], json!({ "site_id": "site-b", "regions": ["eu-central-1", "eu-west-1"] }));
    refused(&cf(a.path(), &["sync", "push", "--project", "research"], &[]), "ResidencySitesDiverge");
}
