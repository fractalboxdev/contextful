//! `contextful derive test-engine` through the built binary, and host derive tasks through
//! an embedding binary.

use std::process::{Command, Output};

#[test]
fn a_link_preview_records_each_vendor_request_before_landing() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut request = [0; 4096];
        let n = socket.read(&mut request).unwrap();
        assert!(String::from_utf8_lossy(&request[..n]).starts_with("GET /article HTTP/1.1"));
        let body = b"<head><title>Article</title></head>";
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
        socket.write_all(body).unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".contextful/context/research");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    std::fs::write(
        dir.path().join("contextful.toml"),
        "authoring_posture = \"per_request\"\n[[pipeline]]\nid = \"cards\"\ntables = [{ name = \"cards\", primary_key = [\"unit_ref\", \"derivation_key\", \"cue_seq\"] }]\n[pipeline.source]\nname = \"derive\"\nconfig = { task = \"link_preview\", engine = \"reader\", source_table = \"documents\", media_column = \"url\", parent_id_column = \"doc_id\" }\n[derive.reader]\ndriver = \"fetch\"\nallow_hosts = [\"localhost\"]\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("documents.jsonl"), format!("{{\"doc_id\":\"d1\",\"url\":\"http://localhost:{port}/article\"}}\n")).unwrap();
    ok(&cf(dir.path(), &["context", "land", "documents", "--project", "research", "--rows", "documents.jsonl", "--run-id", "load-1", "--site-id", "site", "--now", "2030-01-01T00:00:00Z"]));
    ok(&cf(dir.path(), &["pipeline", "run", "cards", "--project", "research", "--run-id", "cards-1", "--site-id", "site", "--now", "2030-01-01T01:00:00Z"]));
    server.join().unwrap();
    let store = contextful_context::Store::open(dir.path(), "research").unwrap();
    let files = contextful_context::ledger::files(&store, "cards_cards").unwrap();
    assert_eq!(files.len(), 1);
    let calls = contextful_context::ledger::read(&files[0]).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "cards-1");
    assert_eq!((calls[0].1.connector.as_str(), calls[0].1.method.as_str(), calls[0].1.url_host.as_str(), calls[0].1.status_code, calls[0].1.batch_seq), ("derive", "GET", "localhost", Some(200), Some(0)));
}

#[test]
fn a_link_preview_reserves_document_and_image_requests_from_one_shared_quota() {
    let vendor = super::pipeline::Vendor::start(|target| match target {
        "/article" => (200, "<head><title>Article</title><meta property=\"og:image\" content=\"/cover.jpg\"></head>".into()),
        "/cover.jpg" => (200, "image".into()),
        _ => (404, String::new()),
    });
    let limiter = super::pipeline::Vendor::start(|target| match target {
        "/quota/acquire" => (200, "{\"decision\":\"granted\",\"permits\":1,\"ttl_secs\":60}".into()),
        _ => (204, String::new()),
    });
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".contextful/context/research");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    let vendor_url = vendor.url("/article").replace("127.0.0.1", "localhost");
    std::fs::write(
        dir.path().join("contextful.toml"),
        format!("authoring_posture = \"per_request\"\n[limiters.preview]\nendpoint = \"{}\"\ntoken = \"secret://limiter-token\"\npermits = 1\n[[pipeline]]\nid = \"cards\"\ntables = [{{ name = \"cards\", primary_key = [\"unit_ref\", \"derivation_key\", \"cue_seq\"] }}]\n[pipeline.source]\nname = \"derive\"\nconfig = {{ task = \"link_preview\", engine = \"reader\", source_table = \"documents\", media_column = \"url\", parent_id_column = \"doc_id\", grant = {{ quota = \"preview\", class = \"batch-read\" }} }}\n[derive.reader]\ndriver = \"fetch\"\nallow_hosts = [\"localhost\"]\nallow_image_hosts = [\"localhost\"]\n", limiter.url("/quota")),
    )
    .unwrap();
    std::fs::write(dir.path().join("documents.jsonl"), format!("{{\"doc_id\":\"d1\",\"url\":\"{vendor_url}\"}}\n")).unwrap();
    ok(&cf(dir.path(), &["context", "land", "documents", "--project", "research", "--rows", "documents.jsonl", "--run-id", "load-1", "--site-id", "site", "--now", "2030-01-01T00:00:00Z"]));
    let out = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["pipeline", "run", "cards", "--project", "research", "--run-id", "cards-2", "--site-id", "site", "--now", "2030-01-01T01:00:00Z"])
        .current_dir(dir.path())
        .env("LIMITER_TOKEN", "lim-1")
        .env("CONTEXTFUL_SECRETS_ALLOW_ENV_TEMPLATES", "1")
        .output()
        .unwrap();
    ok(&out);
    assert_eq!(vendor.targets(), ["/article", "/cover.jpg"]);
    assert_eq!(limiter.targets().iter().filter(|target| target.as_str() == "/quota/acquire").count(), 2);
    let store = contextful_context::Store::open(dir.path(), "research").unwrap();
    let files = contextful_context::ledger::files(&store, "cards_cards").unwrap();
    assert_eq!(files.len(), 1);
    let calls = contextful_context::ledger::read(&files[0]).unwrap();
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|(run, call)| run == "cards-2" && call.method == "GET" && call.url_host == "localhost" && call.status_code == Some(200) && call.batch_seq == Some(0)));
}

#[test]
fn a_link_preview_refuses_to_land_when_its_request_ledger_cannot_settle() {
    let vendor = super::pipeline::Vendor::start(|_| (200, "<head><title>Article</title></head>".into()));
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".contextful/context/research");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    std::fs::write(
        dir.path().join("contextful.toml"),
        "authoring_posture = \"per_request\"\n[[pipeline]]\nid = \"cards\"\ntables = [{ name = \"cards\", primary_key = [\"unit_ref\", \"derivation_key\", \"cue_seq\"] }]\n[pipeline.source]\nname = \"derive\"\nconfig = { task = \"link_preview\", engine = \"reader\", source_table = \"documents\", media_column = \"url\", parent_id_column = \"doc_id\" }\n[derive.reader]\ndriver = \"fetch\"\nallow_hosts = [\"localhost\"]\n",
    )
    .unwrap();
    let address = vendor.url("/article").replace("127.0.0.1", "localhost");
    std::fs::write(dir.path().join("documents.jsonl"), format!("{{\"doc_id\":\"d1\",\"url\":\"{address}\"}}\n")).unwrap();
    ok(&cf(dir.path(), &["context", "land", "documents", "--project", "research", "--rows", "documents.jsonl", "--run-id", "load-1", "--site-id", "site", "--now", "2030-01-01T00:00:00Z"]));
    let blocked = root.join("tables/cards_cards/requests");
    std::fs::create_dir_all(blocked.parent().unwrap()).unwrap();
    std::fs::write(&blocked, "a file blocks the request ledger directory").unwrap();
    let out = cf(dir.path(), &["pipeline", "run", "cards", "--project", "research", "--run-id", "cards-3", "--site-id", "site", "--now", "2030-01-01T01:00:00Z"]);
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && error.contains("request ledger"), "{error}");
    assert!(root.join("tables/cards_cards/data/runs/cards-3").read_dir().is_err(), "no derived batch commits without its ledger");
    assert_eq!(vendor.targets(), ["/article"]);
}

#[test]
fn a_rate_limited_link_request_has_no_batch_ordinal_in_its_ledger() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut request = [0; 4096];
        let mut received = 0;
        while !request[..received].windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            assert!(received < request.len(), "request header exceeds the fixture buffer");
            let count = socket.read(&mut request[received..]).unwrap();
            assert!(count > 0, "connection closes before the request header");
            received += count;
        }
        socket.write_all(b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 301\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".contextful/context/research");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    std::fs::write(
        dir.path().join("contextful.toml"),
        "authoring_posture = \"per_request\"\n[[pipeline]]\nid = \"cards\"\ntables = [{ name = \"cards\", primary_key = [\"unit_ref\", \"derivation_key\", \"cue_seq\"] }]\n[pipeline.source]\nname = \"derive\"\nconfig = { task = \"link_preview\", engine = \"reader\", source_table = \"documents\", media_column = \"url\", parent_id_column = \"doc_id\" }\n[derive.reader]\ndriver = \"fetch\"\nallow_hosts = [\"localhost\"]\n",
    )
    .unwrap();
    let address = format!("http://localhost:{port}/article");
    std::fs::write(dir.path().join("documents.jsonl"), format!("{{\"doc_id\":\"d1\",\"url\":\"{address}\"}}\n")).unwrap();
    ok(&cf(dir.path(), &["context", "land", "documents", "--project", "research", "--rows", "documents.jsonl", "--run-id", "load-1", "--site-id", "site", "--now", "2030-01-01T00:00:00Z"]));
    let out = cf(dir.path(), &["pipeline", "run", "cards", "--project", "research", "--run-id", "cards-4", "--site-id", "site", "--now", "2030-01-01T01:00:00Z"]);
    assert!(!out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    server.join().unwrap();
    let store = contextful_context::Store::open(dir.path(), "research").unwrap();
    let files = contextful_context::ledger::files(&store, "cards_cards").unwrap();
    assert_eq!(files.len(), 1);
    let calls = contextful_context::ledger::read(&files[0]).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!((calls[0].1.status_code, calls[0].1.batch_seq), (Some(429), None));
}

fn cf(dir: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful")).args(args).current_dir(dir).output().unwrap()
}

/// A `fetch` engine named to the verb raises `DeriveTestEngineUnsupported`.
// spec: run.test-engine.unsupported-driver@48b970c1
#[test]
fn the_verb_refuses_a_fetch_engine_and_runs_a_transcriber_over_one_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("contextful.toml"),
        "[derive.cards]\ndriver = \"fetch\"\nallow_hosts = [\"example.com\"]\n\n[derive.reader]\ndriver = \"exec\"\n[derive.reader.engine]\ncommand = [\"cat\", \"{input}\"]\noutput_format = \"srt\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("caption.srt"), "1\n00:00:01,000 --> 00:00:02,000\nhello\n").unwrap();
    let out = cf(dir.path(), &["derive", "test-engine", "cards", "--file", "caption.srt"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("DeriveTestEngineUnsupported"));
    let out = cf(dir.path(), &["derive", "test-engine", "reader", "--file", "caption.srt"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let line: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(line, serde_json::json!({"start_ms": 1000, "end_ms": 2000, "text": "hello"}));
}

/// The embedding binary under `examples/host_derive.rs`, built on first request: the command
/// line with the compiled `word-split` task registered.
fn host_binary() -> std::path::PathBuf {
    static BUILT: std::sync::Once = std::sync::Once::new();
    BUILT.call_once(|| {
        let status = Command::new(env!("CARGO")).args(["build", "-q", "-p", "contextful-cli", "--example", "host_derive"]).status().unwrap();
        assert!(status.success(), "building the host_derive example");
    });
    std::path::Path::new(env!("CARGO_BIN_EXE_contextful")).parent().unwrap().join("examples").join("host_derive")
}

fn run_bin(bin: &std::path::Path, dir: &std::path::Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(bin).args(args).envs(env.iter().copied()).current_dir(dir).env_remove("CONTEXTFUL_NODE_ID").output().unwrap()
}

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A `word-split` pipeline over `documents`: `words` retains versions, `stats` does not, and
/// `units` holds the markers.
fn host_manifest(task: &str) -> String {
    format!(
        "[[pipeline]]\nid = \"split\"\ntables = [\n  \
         {{ name = \"words\", primary_key = [\"unit_ref\", \"derivation_key\", \"word_seq\"], retain_versions = true }},\n  \
         {{ name = \"stats\", primary_key = [\"unit_ref\", \"derivation_key\"] }},\n  \
         {{ name = \"units\", primary_key = [\"unit_ref\", \"derivation_key\", \"cue_seq\"] }},\n]\n\
         [pipeline.source]\nname = \"derive\"\n\
         config = {{ task = \"{task}\", source_table = \"documents\", parent_id_column = \"doc_id\" }}\n"
    )
}

fn host_project(manifest: &str) -> tempfile::TempDir {
    host_project_with_rows(
        manifest,
        "{\"doc_id\":\"d1\",\"body\":\"alpha beta\"}\n{\"doc_id\":\"d2\",\"body\":\"gamma\"}\n{\"doc_id\":\"d3\",\"body\":\"   \"}\n",
    )
}

fn host_project_with_rows(manifest: &str, documents: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(".contextful/context/research");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    std::fs::write(dir.path().join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{}", manifest)).unwrap();
    std::fs::write(dir.path().join("documents.jsonl"), documents).unwrap();
    ok(&cf(dir.path(), &["context", "land", "documents", "--project", "research", "--rows", "documents.jsonl", "--run-id", "load-1", "--site-id", "site", "--now", "2030-01-01T00:00:00Z"]));
    dir
}

#[test]
fn an_empty_host_content_marker_carries_the_declared_parent_retention_clock() {
    let host = host_binary();
    let manifest = host_manifest("word-split").replace(
        "retain_versions = true",
        "columns = { base_arrived_at = \"timestamp\" }, retain_rows = { column = \"base_arrived_at\", age = \"30d\" }",
    ).replace(
        "primary_key = [\"unit_ref\", \"derivation_key\", \"word_seq\"]",
        "primary_key = [\"unit_ref\", \"derivation_key\"]",
    );
    let dir = host_project_with_rows(
        &manifest,
        "{\"doc_id\":\"d1\",\"body\":\"   \",\"base_arrived_at\":\"2100-01-01T00:00:00Z\"}\n",
    );
    ok(&fire(&host, dir.path(), "split-1", "2030-01-01T01:00:00Z", &[]));
    assert_eq!(
        select(dir.path(), "SELECT unit_ref, kind, base_arrived_at FROM split_words"),
        [["d1", "marker", "2100-01-01T00:00:00Z"]]
    );
}

/// The rows `sql` answers over the project, each as its cells' text.
fn select(dir: &std::path::Path, sql: &str) -> Vec<Vec<String>> {
    let v: serde_json::Value = serde_json::from_str(&ok(&cf(dir, &["query", "--json", "--project", "research", sql]))).unwrap();
    v["rows"].as_array().unwrap().iter().map(|r| r.as_array().unwrap().iter().map(|c| c.as_str().map(str::to_string).unwrap_or_else(|| c.to_string())).collect()).collect()
}

fn fire(bin: &std::path::Path, dir: &std::path::Path, run: &str, now: &str, env: &[(&str, &str)]) -> Output {
    run_bin(bin, dir, &["pipeline", "run", "split", "--project", "research", "--run-id", run, "--site-id", "site", "--now", now], env)
}

/// A host task records an incomplete parent in each table run's skipped count.
#[test]
fn a_host_task_counts_an_incomplete_parent_on_its_run_records() {
    let host = host_binary();
    let dir = host_project(&host_manifest("word-split"));
    std::fs::write(dir.path().join("missing.jsonl"), "{\"body\":\"orphan\"}\n").unwrap();
    ok(&cf(dir.path(), &["context", "land", "documents", "--project", "research", "--rows", "missing.jsonl", "--run-id", "load-2", "--site-id", "site", "--now", "2030-01-01T00:30:00Z"]));
    ok(&fire(&host, dir.path(), "split-1", "2030-01-01T01:00:00Z", &[]));
    let history = ok(&cf(dir.path(), &["run", "history", "--project", "research", "--export"]));
    let runs: Vec<serde_json::Value> = history.lines().skip(1).map(|line| serde_json::from_str(line).unwrap()).collect();
    for table in ["split_words", "split_stats", "split_units"] {
        let run_id = format!("split-1.{table}");
        assert_eq!(runs.iter().find(|run| run["run_id"] == run_id).map(|run| &run["skipped"]), Some(&serde_json::json!(1)), "{history}");
    }
}

fn chain_manifest() -> String {
    let child = "site_id = \"site\"\n[[pipeline]]\nid = \"echo\"\nschedule = \"every 1h\"\ntables = [\n  { name = \"copies\", primary_key = [\"unit_ref\", \"derivation_key\", \"cue_seq\"] },\n  { name = \"units\", primary_key = [\"unit_ref\", \"derivation_key\", \"cue_seq\"] },\n]\n[pipeline.source]\nname = \"derive\"\nconfig = { task = \"word-copy\", source_table = \"split_words\", parent_id_column = \"word\" }\n";
    format!("{child}\n{}", host_manifest("word-split").replace("id = \"split\"\n", "id = \"split\"\nschedule = \"every 1h\"\n"))
}

/// A single-pipeline apply validates the combined applied graph before claiming a version.
#[test]
fn applying_one_derive_change_refuses_a_cycle_with_the_applied_sibling() {
    let host = host_binary();
    let dir = host_project(&chain_manifest());
    let path = dir.path();
    ok(&run_bin(&host, path, &["pipeline", "import", "--project", "research"], &[]));
    let revised = chain_manifest()
        .replace("source_table = \"split_words\"", "source_table = \"documents\"")
        .replace("source_table = \"documents\", parent_id_column = \"doc_id\"", "source_table = \"echo_copies\", parent_id_column = \"doc_id\"");
    std::fs::write(path.join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{revised}")).unwrap();
    let result = run_bin(&host, path, &["pipeline", "apply", "split", "--project", "research"], &[]);
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(!result.status.success() && error.contains("DeriveCycle") && error.contains("split") && error.contains("echo"), "{error}");
    assert!(!path.join(".contextful/control/research/manifest@v2.toml").exists(), "cyclic snapshot was claimed");
}

// spec: run.select.derive-order@1ff2a000
#[test]
fn a_child_declared_first_derives_its_parents_new_rows_in_one_tick() {
    let host = host_binary();
    let dir = host_project(&chain_manifest());
    let p = dir.path();
    ok(&run_bin(&host, p, &["pipeline", "import", "--project", "research"], &[]));
    let cycle = run_bin(&host, p, &["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T01:00:00Z"], &[]);
    ok(&cycle);
    assert_eq!(select(p, "SELECT copy FROM \"echo_copies\" WHERE kind = 'passage' ORDER BY copy"), [["alpha"], ["beta"], ["gamma"]]);
}

// spec: run.select.derive-failed-parent@219b4497
#[test]
fn a_child_reads_committed_parent_rows_after_its_parent_fails() {
    let host = host_binary();
    let dir = host_project(&chain_manifest());
    let p = dir.path();
    ok(&fire(&host, p, "seed", "2030-01-01T00:00:00Z", &[]));
    ok(&run_bin(&host, p, &["pipeline", "import", "--project", "research"], &[]));
    let cycle = run_bin(&host, p, &["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T01:00:00Z"], &[("WORD_SPLIT_VERSION", "2"), ("WORD_SPLIT_FAIL", "1")]);
    assert!(!cycle.status.success(), "a failed parent makes the cycle red: {}", String::from_utf8_lossy(&cycle.stdout));
    assert_eq!(select(p, "SELECT copy FROM \"echo_copies\" WHERE kind = 'passage' ORDER BY copy"), [["alpha"], ["beta"], ["gamma"]]);
}

#[test]
fn a_failed_parent_runs_its_derive_child_past_an_explicit_after_sibling() {
    let host = host_binary();
    let vendor = crate::pipeline::Vendor::start(|_| (200, "[]".into()));
    let manifest = format!(
        "{}\n[[pipeline]]\nid = \"a-after\"\nafter = \"split\"\ntables = [\"items\"]\n[pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"{}\" }}\n",
        chain_manifest(),
        vendor.url("/after"),
    );
    let dir = host_project(&manifest);
    let p = dir.path();
    ok(&fire(&host, p, "seed", "2030-01-01T00:00:00Z", &[]));
    ok(&run_bin(&host, p, &["pipeline", "import", "--project", "research"], &[]));
    let cycle = run_bin(&host, p, &["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T01:00:00Z"], &[("WORD_SPLIT_VERSION", "2"), ("WORD_SPLIT_FAIL", "1")]);
    assert!(!cycle.status.success());
    assert_eq!(select(p, "SELECT copy FROM \"echo_copies\" WHERE kind = 'passage' ORDER BY copy"), [["alpha"], ["beta"], ["gamma"]]);
    assert!(vendor.targets().is_empty(), "an explicit after sibling ran after the parent failed");
}

// spec: run.select.parent-outcome@418ee978
#[cfg(unix)]
#[test]
fn a_derived_child_stops_when_the_parent_process_cannot_start() {
    let host = host_binary();
    let manifest = chain_manifest().replacen("site_id = \"site\"", "site_id = \"site\"\n[control]\ntrigger = \"external\"", 1);
    let dir = host_project(&manifest);
    ok(&run_bin(&host, dir.path(), &["pipeline", "import", "--project", "research"], &[]));
    let copy = dir.path().join("host-copy");
    std::fs::copy(&host, &copy).unwrap();
    let (daemon, url) = crate::pipeline::external_from(dir.path(), &copy);
    std::fs::remove_file(&copy).unwrap();
    assert_eq!(crate::pipeline::post(&url).0, 200);
    let index = daemon.wait_for("fire split: failed", 0);
    let line = &daemon.lines()[index];
    assert!(line.contains("split: starting"), "{line}");
    assert!(!line.contains("echo: starting"), "a derived child started without a parent outcome: {line}");
}

/// A pipeline naming a registered host task builds; an unregistered name raises `DeriveUnknownTask`, listing the
/// built-in and registered names.
#[test]
fn a_registered_host_task_builds_and_an_unregistered_name_lists_both_sets() {
    let host = host_binary();
    let dir = host_project(&host_manifest("word-split"));
    let valid = ok(&run_bin(&host, dir.path(), &["pipeline", "validate"], &[]));
    assert!(valid.contains("split: valid (3 tables"), "{valid}");

    // The stock binary registers no host task, so the same name is unknown to it.
    let stock = cf(dir.path(), &["pipeline", "validate"]);
    let err = String::from_utf8_lossy(&stock.stderr);
    assert!(!stock.status.success() && err.contains("DeriveUnknownTask") && err.contains("(none)"), "{err}");

    std::fs::write(dir.path().join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{}", host_manifest("word-splat"))).unwrap();
    let out = run_bin(&host, dir.path(), &["pipeline", "validate"], &[]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    for name in ["DeriveUnknownTask", "word-splat", "transcribe, link_preview", "word-split"] {
        assert!(err.contains(name), "{name}: {err}");
    }
}

/// A host-task run commits each content table under its own commit, then the marker table, and a content table failing stops the fire before its marker lands, so the unit re-runs and its rows collapse by key.
// spec: run.emit.marker-last@466e5fca
#[test]
fn a_failure_before_the_marker_re_runs_the_unit_and_its_rows_collapse_by_key() {
    let host = host_binary();
    let dir = host_project(&host_manifest("word-split"));
    let p = dir.path();
    // A file where the marker table's run directory belongs fails that table's landing alone.
    let blocked = p.join(".contextful/context/research/tables/split_units/data/runs/split-1.split_units");
    std::fs::create_dir_all(blocked.parent().unwrap()).unwrap();
    std::fs::write(&blocked, "").unwrap();
    let failed = fire(&host, p, "split-1", "2030-01-01T01:00:00Z", &[]);
    let err = String::from_utf8_lossy(&failed.stderr);
    assert!(!failed.status.success() && err.contains("split-1.split_units"), "the marker table's landing fails: {err}");
    std::fs::remove_file(&blocked).unwrap();
    let count = |t: &str| select(p, &format!("SELECT count(*) AS n FROM \"{t}\""))[0][0].clone();
    // `d3` derives no word: each content table holds its `empty` marker.
    assert_eq!((count("split_words"), count("split_stats")), ("4".into(), "3".into()), "the content tables landed first");
    assert_eq!(count("split_units"), "0", "no marker landed");

    // The next fire derives every unit again; its content rows land under the same keys.
    ok(&fire(&host, p, "split-2", "2030-01-01T02:00:00Z", &[]));
    assert_eq!((count("split_words"), count("split_stats"), count("split_units")), ("4".into(), "3".into(), "3".into()));
    let statuses = select(p, "SELECT unit_ref, unit_status, task_version FROM \"split_units\" ORDER BY unit_ref");
    assert_eq!(statuses, [["d1", "ok", "1"], ["d2", "ok", "1"], ["d3", "empty", "1"]]);
    let words = select(p, "SELECT unit_ref, word, kind FROM \"split_words\" ORDER BY unit_ref, word_seq");
    assert_eq!(words, [["d1", "alpha", "passage"], ["d1", "beta", "passage"], ["d2", "gamma", "passage"], ["d3", "null", "marker"]]);

    // Every unit settled: a third fire derives nothing.
    ok(&fire(&host, p, "split-3", "2030-01-01T03:00:00Z", &[]));
    assert_eq!((count("split_words"), count("split_stats"), count("split_units")), ("4".into(), "3".into(), "3".into()));
}

/// On an output table declaring `retain_versions`, rows under an earlier task version stay current under that version beside the newer version's rows.
// spec: run.emit.version-retained@7759092f
#[test]
fn a_raised_task_version_re_derives_and_a_retaining_table_keeps_both_versions() {
    let host = host_binary();
    let dir = host_project(&host_manifest("word-split"));
    let p = dir.path();
    ok(&fire(&host, p, "split-1", "2030-01-01T01:00:00Z", &[]));
    ok(&fire(&host, p, "split-2", "2030-01-01T02:00:00Z", &[("WORD_SPLIT_VERSION", "2")]));
    // A content table's passages, or the marker table's markers, per task version.
    let by_version = |t: &str| {
        let kind = if t == "split_units" { "marker" } else { "passage" };
        select(p, &format!("SELECT task_version, count(*) AS n FROM \"{t}\" WHERE kind = '{kind}' GROUP BY 1 ORDER BY 1"))
    };
    // `words` retains versions: each version's rows read under their own version.
    assert_eq!(by_version("split_words"), [["1", "3"], ["2", "3"]]);
    // `stats` and the markers supersede: the earlier version's rows stop answering.
    assert_eq!(by_version("split_stats"), [["2", "2"]]);
    assert_eq!(by_version("split_units"), [["2", "3"]]);

    // A fold keeps what a read answers.
    for t in ["split_words", "split_stats"] {
        ok(&cf(p, &["context", "compact", t, "--project", "research", "--now", "2030-01-01T03:00:00Z"]));
    }
    assert_eq!(by_version("split_words"), [["1", "3"], ["2", "3"]]);
    assert_eq!(by_version("split_stats"), [["2", "2"]]);
}

/// A host unit landing no row in a content table lands one `kind` `marker`, `unit_status` `empty` row there under its key, so {{run.emit.stale-supersedes}} holds in every content table.
// spec: run.emit.content-empty@17beeffb
#[test]
fn a_content_table_left_without_rows_stops_answering_the_earlier_key() {
    let host = host_binary();
    let dir = host_project(&host_manifest("word-split").replace(", retain_versions = true", ""));
    let p = dir.path();
    let passages = |t: &str| select(p, &format!("SELECT unit_ref, task_version FROM \"{t}\" WHERE kind = 'passage' ORDER BY unit_ref, task_version"));
    ok(&fire(&host, p, "split-1", "2030-01-01T01:00:00Z", &[]));
    assert_eq!(passages("split_stats"), [["d1", "1"], ["d2", "1"]]);

    // Version 2 returns no `stats` rows: the earlier `stats` rows stop answering.
    ok(&fire(&host, p, "split-2", "2030-01-01T02:00:00Z", &[("WORD_SPLIT_VERSION", "2"), ("WORD_SPLIT_SKIP", "stats")]));
    assert_eq!(passages("split_words"), [["d1", "2"], ["d1", "2"], ["d2", "2"]]);
    assert!(passages("split_stats").is_empty(), "{:?}", passages("split_stats"));

    // Version 3 derives nothing for any unit: every content table answers no passage.
    ok(&fire(&host, p, "split-3", "2030-01-01T03:00:00Z", &[("WORD_SPLIT_VERSION", "3"), ("WORD_SPLIT_SKIP", "words,stats")]));
    let statuses = select(p, "SELECT unit_ref, unit_status, task_version FROM \"split_units\" ORDER BY unit_ref");
    assert_eq!(statuses, [["d1", "empty", "3"], ["d2", "empty", "3"], ["d3", "empty", "3"]]);
    for t in ["split_words", "split_stats"] {
        assert!(passages(t).is_empty(), "{t}: {:?}", passages(t));
        let markers = select(p, &format!("SELECT unit_ref, unit_status, task_version FROM \"{t}\" WHERE kind = 'marker' ORDER BY unit_ref"));
        assert_eq!(markers, [["d1", "empty", "3"], ["d2", "empty", "3"], ["d3", "empty", "3"]], "{t}");
    }

    // A fold keeps what a read answers.
    for t in ["split_words", "split_stats"] {
        ok(&cf(p, &["context", "compact", t, "--project", "research", "--now", "2030-01-01T04:00:00Z"]));
        assert!(passages(t).is_empty(), "{t} after fold: {:?}", passages(t));
    }
}
