//! `contextful pipeline` through the built binary, against a loopback vendor.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

/// A loopback vendor answering `handler(target)` with `(status, body)`, recording every target.
struct Vendor {
    port: u16,
    targets: Arc<Mutex<Vec<String>>>,
}

impl Vendor {
    fn start(handler: impl Fn(&str) -> (u16, String) + Send + Sync + 'static) -> Vendor {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let targets: Arc<Mutex<Vec<String>>> = Arc::default();
        let (seen, handler) = (targets.clone(), Arc::new(handler));
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let target = line.split_whitespace().nth(1).unwrap_or_default().to_string();
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap() == 0 || h.trim().is_empty() {
                        break;
                    }
                }
                let (status, body) = handler(&target);
                seen.lock().unwrap().push(target);
                let _ = write!(stream, "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        Vendor { port, targets }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn targets(&self) -> Vec<String> {
        self.targets.lock().unwrap().clone()
    }
}

fn project(manifest: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(".contextful/context/research");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    std::fs::write(dir.path().join("contextful.toml"), manifest).unwrap();
    dir
}

fn cf(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(args)
        .current_dir(dir)
        .env_remove("CONTEXTFUL_NODE_ID")
        .env_remove("CONTEXTFUL_SECRETS_BACKEND")
        .output()
        .unwrap()
}

fn fire(dir: &Path, id: &str, run: &str, now: &str) -> Output {
    cf(dir, &["pipeline", "run", id, "--project", "research", "--run-id", run, "--site-id", "site-a", "--now", now])
}

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn pipeline(id: &str, endpoint: &str, extra: &str, tables: &str) -> String {
    format!("[[pipeline]]\nid = \"{id}\"\n{extra}\n{tables}\n[pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"{endpoint}\" }}\n")
}

/// Startup reads `contextful.toml` for project config and inline `[[pipeline]]` blocks, then `pipelines/*.toml`
/// and `pipelines/*.json`; specifications are collected by `id`.
// spec: run.declare.manifest-file@4779cc3b
#[test]
fn specifications_come_from_the_project_manifest_then_the_pipelines_directory() {
    let dir = project(&format!(
        "# project configuration\n\n{}",
        pipeline("inline", "https://api.vendor.example/v1", "", "tables = [\"a\"]")
    ));
    std::fs::create_dir_all(dir.path().join("pipelines")).unwrap();
    std::fs::write(
        dir.path().join("pipelines/from-toml.toml"),
        "id = \"from-toml\"\ntables = [\"b\"]\n[source]\nname = \"http\"\nconfig = { endpoint = \"https://api.vendor.example/v2\" }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("pipelines/from-json.json"),
        r#"{"id": "from-json", "tables": ["c"], "source": {"name": "http", "config": {"endpoint": "https://api.vendor.example/v3"}}}"#,
    )
    .unwrap();
    std::fs::write(dir.path().join("pipelines/readme.md"), "not a manifest").unwrap();
    let listed = ok(&cf(dir.path(), &["pipeline", "validate"]));
    let ids: Vec<&str> = listed.lines().map(|l| l.split(':').next().unwrap()).collect();
    assert_eq!(ids, ["inline", "from-json", "from-toml"]);
}

/// One `id` declared twice raises `PipelineDuplicateId`, naming each file and the line its declaration starts on.
// spec: run.declare.duplicate-id@73a5d632
#[test]
fn one_id_declared_twice_names_both_declarations() {
    let dir = project(&format!("# project\n\n{}", pipeline("orders", "https://api.vendor.example/v1", "", "tables = [\"a\"]")));
    std::fs::create_dir_all(dir.path().join("pipelines")).unwrap();
    std::fs::write(dir.path().join("pipelines/orders.toml"), format!("# again\n\n{}", pipeline("orders", "https://api.vendor.example/v1", "", "tables = [\"b\"]"))).unwrap();
    let out = cf(dir.path(), &["pipeline", "validate"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("PipelineDuplicateId"), "{err}");
    assert!(err.contains("contextful.toml:3") && err.contains("pipelines/orders.toml:3"), "{err}");
}

/// The local context store is the only destination, and synthesized artifacts write back through it; any other
/// `destination` raises `PipelineUnknownDestination` at assembly, before any row moves.
// spec: run.land.unknown-destination@76b028c6
#[test]
fn another_destination_is_refused_before_any_request() {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"a\"}]".into()));
    let dir = project(&pipeline("orders", &vendor.url("/v1"), "destination = { name = \"warehouse\" }", "tables = [\"a\"]"));
    let out = fire(dir.path(), "orders", "r1", "2030-01-01T00:00:00Z");
    assert!(!out.status.success());
    assert!(stderr(&out).contains("PipelineUnknownDestination"), "{}", stderr(&out));
    assert!(vendor.targets().is_empty());
    assert!(!dir.path().join(".contextful/context/research/tables").exists());
    // The store, named or defaulted, lands.
    let dir = project(&pipeline("orders", &vendor.url("/v1"), "destination = { name = \"store\" }", "tables = [\"a\"]"));
    ok(&fire(dir.path(), "orders", "r1", "2030-01-01T00:00:00Z"));
}

/// `validate` builds the seed source as it builds the live one: its connector name, config keys and header
/// templates are refused before any seeding run reaches them.
#[test]
fn validate_builds_the_seed_source_like_the_live_one() {
    let seeded = |seed_source: &str| {
        let tables = "[[pipeline.tables]]\nname = \"items\"\nprimary_key = [\"id\"]\norder_by = \"updated_at\"\n";
        let seed = format!("[pipeline.seed]\nbelow = \"2030-01-01T00:00:00Z\"\n[pipeline.seed.source]\n{seed_source}\n");
        project(&format!("{}{seed}", pipeline("orders", "https://api.vendor.example/v1", "", tables)))
    };
    ok(&cf(seeded("name = \"http\"\nconfig = { endpoint = \"https://exports.vendor.example/v1\" }").path(), &["pipeline", "validate"]));
    for (source, refusal) in [
        ("name = \"file\"\nconfig = { path = \"exports/items.jsonl\" }", "`file`"),
        ("name = \"http\"\nconfig = { endpoint = \"https://exports.vendor.example/v1\", bogus = 1 }", "PipelineUnknownConfigKey"),
        (
            "name = \"http\"\nconfig = { endpoint = \"https://exports.vendor.example/v1\", headers = { Authorization = \"Bearer sk_live_0123456789abcdef\" } }",
            "SecretMaterialInDeclaration",
        ),
    ] {
        let out = cf(seeded(source).path(), &["pipeline", "validate"]);
        assert!(!out.status.success(), "{source}");
        let err = stderr(&out);
        assert!(err.contains(refusal) && err.contains("seed"), "{source}: {err}");
        assert!(!err.contains("sk_live_0123456789abcdef"), "{err}");
    }
}

fn two_tables(vendor: &Vendor, on_error: &str) -> tempfile::TempDir {
    project(&pipeline("shop", &vendor.url("/v1/{table}"), &format!("on_table_error = \"{on_error}\""), "tables = [\"bad\", \"good\"]"))
}

/// A table whose pull fails raises `PipelineTableFailed` carrying the table, the error kind and the run id; the
/// fire then follows `on_table_error`.
// spec: run.land.table-failed@fde056f6
#[test]
fn a_failing_table_is_named_with_its_kind_and_run() {
    let vendor = Vendor::start(|t| if t.starts_with("/v1/bad") { (404, "{}".into()) } else { (200, "[{\"id\":\"g\"}]".into()) });
    let dir = two_tables(&vendor, "abort");
    let out = fire(dir.path(), "shop", "r1", "2030-01-01T00:00:00Z");
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("PipelineTableFailed: table `shop_bad` failed as Permanent in run `r1.shop_bad`"), "{err}");
    assert!(vendor.targets().iter().all(|t| t.starts_with("/v1/bad")), "abort halts the fire: {:?}", vendor.targets());
}

/// `on_table_error` is abort, the default, halting the fire at the first failing table, or continue, landing
/// every other table and reporting the failed run ids through {{run.declare.table-error-exit}}.
// spec: run.declare.table-error@86b78a63
#[test]
fn abort_halts_at_the_first_failure_and_continue_runs_the_rest() {
    let serve = |t: &str| if t.starts_with("/v1/bad") { (404, "{}".to_string()) } else { (200, "[{\"id\":\"g\"}]".to_string()) };
    // Abort is the default.
    let vendor = Vendor::start(serve);
    let dir = project(&pipeline("shop", &vendor.url("/v1/{table}"), "", "tables = [\"bad\", \"good\"]"));
    assert!(!fire(dir.path(), "shop", "r1", "2030-01-01T00:00:00Z").status.success());
    assert_eq!(vendor.targets(), ["/v1/bad"]);

    let vendor = Vendor::start(serve);
    let dir = two_tables(&vendor, "continue");
    let out = fire(dir.path(), "shop", "r1", "2030-01-01T00:00:00Z");
    assert!(String::from_utf8_lossy(&out.stdout).contains("shop_good: r1.shop_good success"), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(stderr(&out).contains("1 table(s) failed: r1.shop_bad"), "{}", stderr(&out));
    assert_eq!(vendor.targets(), ["/v1/bad", "/v1/good"]);
    assert!(!out.status.success(), "a continued fire with a failed table exits non-zero");
    let listed = ok(&cf(dir.path(), &["context", "files", "shop_good", "--project", "research"]));
    assert_eq!(listed.lines().count(), 1, "the continuing table landed");

    // A table the engine refuses to open is a failed table too: under continue, the fire reaches the next one.
    let again = fire(dir.path(), "shop", "r1", "2030-01-01T00:01:00Z");
    assert!(!again.status.success());
    let err = stderr(&again);
    assert_eq!(err.matches("PipelineTableFailed").count(), 2, "{err}");
    assert!(err.contains("2 table(s) failed: r1.shop_bad, r1.shop_good"), "{err}");
}

/// A table pattern binds table-name segments into the request URL, percent-encoded, and each table holds its own
/// position.
// spec: connector.source.table-pattern@2cf8da67
#[test]
fn each_table_binds_its_segment_and_keeps_its_own_position() {
    let vendor = Vendor::start(|t| match t.split('?').next().unwrap_or_default() {
        "/v1/sales%20orders" => (200, "[{\"id\":\"s1\",\"at\":5},{\"id\":\"s2\",\"at\":9}]".into()),
        "/v1/returns" => (200, "[{\"id\":\"r1\",\"at\":2}]".into()),
        _ => (404, "{}".into()),
    });
    let dir = project(&format!(
        "[[pipeline]]\nid = \"shop\"\nincremental = \"at\"\ntables = [\"sales orders\", \"returns\"]\n[pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"{}\", since_param = \"since\" }}\n",
        vendor.url("/v1/{table}")
    ));
    ok(&fire(dir.path(), "shop", "f1", "2030-01-01T00:00:00Z"));
    ok(&fire(dir.path(), "shop", "f2", "2030-01-01T00:01:00Z"));
    let targets = vendor.targets();
    assert_eq!(&targets[..2], ["/v1/sales%20orders", "/v1/returns"]);
    assert_eq!(&targets[2..], ["/v1/sales%20orders?since=9", "/v1/returns?since=2"]);

    // A declared pattern binds each segment by name into its own place in the path.
    let vendor = Vendor::start(|t| match t.split('?').next().unwrap_or_default() {
        "/repos/acme/wid%20gets/issues" => (200, "[{\"id\":\"i1\",\"at\":4}]".into()),
        "/repos/acme/tools/pulls" => (200, "[{\"id\":\"p1\",\"at\":7},{\"id\":\"p2\",\"at\":8}]".into()),
        _ => (404, "{}".into()),
    });
    let dir = project(&format!(
        "[[pipeline]]\nid = \"gh\"\nincremental = \"at\"\ntables = [\"issues/acme/wid gets\", \"pulls/acme/tools\"]\n[pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"{}\", table_pattern = \"{{stream}}/{{owner}}/{{repo}}\", since_param = \"since\" }}\n",
        vendor.url("/repos/{owner}/{repo}/{stream}")
    ));
    ok(&fire(dir.path(), "gh", "g1", "2030-01-01T00:00:00Z"));
    ok(&fire(dir.path(), "gh", "g2", "2030-01-01T00:01:00Z"));
    let targets = vendor.targets();
    assert_eq!(&targets[..2], ["/repos/acme/wid%20gets/issues", "/repos/acme/tools/pulls"]);
    assert_eq!(&targets[2..], ["/repos/acme/wid%20gets/issues?since=4", "/repos/acme/tools/pulls?since=8"]);
}

#[test]
fn validate_holds_store_tables_to_their_visibility_block() {
    let dir = project("[[pipeline.tables]]\nname = \"wiki/pages\"\n\n[pipeline.tables.visibility]\nsource = \"wiki\"\nresource_key = \"page_id\"\nfidelity = \"mirrored\"\nfamily = \"item-exception\"\nmax_acl_staleness = \"1h30m\"\n");
    let out = cf(dir.path(), &["pipeline", "validate"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("VisibilityBudgetMalformed") && err.contains("wiki/pages"), "{err}");
}

#[test]
fn an_incremental_workbook_pipeline_is_refused_at_validation() {
    let dir = project(
        "[[pipeline]]\nid = \"book\"\nincremental = \"id\"\ntables = [\"a\"]\n[pipeline.source]\nname = \"http\"\nconfig = { endpoint = \"https://files.vendor.example/{table}.xlsx\", format = \"xlsx\" }\n",
    );
    let out = cf(dir.path(), &["pipeline", "validate"]);
    assert!(!out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let err = stderr(&out);
    assert!(err.contains("ConnectorIncrementalUnsupported") && err.contains("incremental"), "{err}");
}

/// A fire takes its site id from the manifest's `site_id`; `--site-id` replaces it for one fire.
#[test]
fn a_fire_takes_its_site_id_from_the_manifest_unless_the_flag_names_one() {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"a\"}]".into()));
    let dir = project(&format!("site_id = \"site-m\"\n\n{}", pipeline("shop", &vendor.url("/v1/{table}"), "", "tables = [\"orders\"]")));
    let site = |run: &str| -> serde_json::Value {
        let shown: serde_json::Value = serde_json::from_str(&ok(&cf(dir.path(), &["run", "show", run, "--project", "research"]))).unwrap();
        shown["site_id"].clone()
    };
    ok(&cf(dir.path(), &["pipeline", "run", "shop", "--project", "research", "--run-id", "m1", "--now", "2030-01-01T00:00:00Z"]));
    assert_eq!(site("m1"), "site-m");
    ok(&fire(dir.path(), "shop", "f1", "2030-01-01T00:01:00Z"));
    assert_eq!(site("f1"), "site-a");
}

fn object_pipeline(config: &str) -> tempfile::TempDir {
    project(&format!("[[pipeline]]\nid = \"feed\"\ntables = [\"orders\"]\n[pipeline.source]\nname = \"s3\"\nconfig = {{ bucket = \"lake\", format = \"jsonl\"{config} }}\n"))
}

/// `pipeline validate` builds an `s3` source: a key beside a prefix and a literal credential
/// each refuse before any request, and a well-formed source validates.
#[test]
fn validate_builds_an_object_source_and_refuses_its_malformed_declarations() {
    ok(&cf(
        object_pipeline(", prefix = \"orders/\", suffix = \".jsonl.gz\", skip_unchanged = true, access_key_id = \"secret://lake-key-id\", secret_access_key = \"secret://lake-secret\"").path(),
        &["pipeline", "validate"],
    ));
    for (config, error) in [
        (", key = \"orders.jsonl\", prefix = \"orders/\"", "ConnectorObjectAddressRejected"),
        (", key = \"orders.jsonl\", access_key_id = \"AKIAIOSFODNN7EXAMPLE\", secret_access_key = \"secret://lake-secret\"", "SecretMaterialInDeclaration"),
    ] {
        let out = cf(object_pipeline(config).path(), &["pipeline", "validate"]);
        assert!(!out.status.success(), "{config}: {}", String::from_utf8_lossy(&out.stdout));
        assert!(stderr(&out).contains(error), "{config}: {}", stderr(&out));
    }
}

/// `incremental` beside `skip_unchanged` on an `s3` source refuses at validation: the
/// position records each object's ETag.
// spec: connector.source.object-position-owned@4ed72fb9
#[test]
fn an_incremental_field_beside_an_etag_position_is_refused_at_validation() {
    let manifest = |skip: bool| {
        format!("[[pipeline]]\nid = \"feed\"\nincremental = \"at\"\ntables = [\"orders\"]\n[pipeline.source]\nname = \"s3\"\nconfig = {{ bucket = \"lake\", key = \"orders.jsonl\", skip_unchanged = {skip} }}\n")
    };
    ok(&cf(project(&manifest(false)).path(), &["pipeline", "validate"]));
    let out = cf(project(&manifest(true)).path(), &["pipeline", "validate"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("ConnectorPositionOwned") && stderr(&out).contains("ETag"), "{}", stderr(&out));
}

/// A loopback S3 endpoint over a mutable key map: object reads answer with an `ETag` of
/// the body's length and first byte, and every request line is recorded.
struct Bucket {
    port: u16,
    objects: Arc<Mutex<Vec<(String, String)>>>,
    lines: Arc<Mutex<Vec<String>>>,
}

impl Bucket {
    fn start(objects: &[(&str, &str)]) -> Bucket {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let objects: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(objects.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()));
        let lines: Arc<Mutex<Vec<String>>> = Arc::default();
        let (held, seen) = (objects.clone(), lines.clone());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap() == 0 || h.trim().is_empty() {
                        break;
                    }
                }
                let mut parts = line.split_whitespace();
                let (method, target) = (parts.next().unwrap_or_default().to_string(), parts.next().unwrap_or_default().to_string());
                seen.lock().unwrap().push(format!("{method} {}", target.split('?').next().unwrap_or_default()));
                let path = target.split('?').next().unwrap_or_default();
                let etag = |v: &str| format!("\"{}-{}\"", v.len(), v.bytes().next().unwrap_or(0));
                let (status, headers, body) = match path.strip_prefix("/lake/") {
                    Some(key) => match held.lock().unwrap().iter().find(|(k, _)| k == key) {
                        Some((_, v)) => (200, format!("ETag: {}\r\n", etag(v)), if method == "HEAD" { String::new() } else { v.clone() }),
                        None => (404, String::new(), String::new()),
                    },
                    None => (404, String::new(), String::new()),
                };
                let _ = write!(stream, "HTTP/1.1 {status} X\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n{body}", body.len());
            }
        });
        Bucket { port, objects, lines }
    }

    fn put(&self, key: &str, body: &str) {
        let mut objects = self.objects.lock().unwrap();
        objects.retain(|(k, _)| k != key);
        objects.push((key.to_string(), body.to_string()));
    }
}

/// A fixed-key JSONL object lands its rows; a second fire over the same ETag lands zero rows,
/// closes a success and keeps the position, and a rewritten object lands again.
#[test]
fn a_fixed_key_object_lands_once_per_etag() {
    let bucket = Bucket::start(&[("feed/orders.jsonl", "{\"id\":\"o1\"}\n{\"id\":\"o2\"}\n")]);
    let dir = project(&format!(
        "[[pipeline]]\nid = \"feed\"\ntables = [\"orders\"]\n[pipeline.source]\nname = \"s3\"\nconfig = {{ bucket = \"lake\", endpoint = \"http://127.0.0.1:{}\", key = \"feed/orders.jsonl\", skip_unchanged = true, access_key_id = \"secret://lake-key-id\", secret_access_key = \"secret://lake-secret\" }}\n",
        bucket.port
    ));
    let fire = |run: &str, now: &str| {
        Command::new(env!("CARGO_BIN_EXE_contextful"))
            .args(["pipeline", "run", "feed", "--project", "research", "--run-id", run, "--site-id", "site-a", "--now", now])
            .current_dir(dir.path())
            .env_remove("CONTEXTFUL_NODE_ID")
            .env_remove("CONTEXTFUL_SECRETS_BACKEND")
            .env("CONTEXTFUL_SECRETS_ALLOW_ENV_TEMPLATES", "1")
            .env("LAKE_KEY_ID", "AKIDLOOPBACKEXAMPLE")
            .env("LAKE_SECRET", "loopback-secret")
            .output()
            .unwrap()
    };
    assert!(ok(&fire("f1", "2030-01-01T00:00:00Z")).contains("2 rows"));
    assert!(ok(&fire("f2", "2030-01-01T00:01:00Z")).contains("f2 success · 0 rows"));
    bucket.put("feed/orders.jsonl", "{\"id\":\"o1\"}\n{\"id\":\"o2\"}\n{\"id\":\"o3\"}\n");
    assert!(ok(&fire("f3", "2030-01-01T00:02:00Z")).contains("3 rows"));
    assert_eq!(*bucket.lines.lock().unwrap(), ["GET /lake/feed/orders.jsonl", "HEAD /lake/feed/orders.jsonl", "HEAD /lake/feed/orders.jsonl", "GET /lake/feed/orders.jsonl"]);
}
