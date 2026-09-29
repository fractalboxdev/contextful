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
                let mut length = 0;
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap() == 0 || h.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        if k.trim().eq_ignore_ascii_case("content-length") {
                            length = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let mut body = vec![0; length];
                let _ = std::io::Read::read_exact(&mut reader, &mut body);
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

pub(crate) fn project(manifest: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(".contextful/context/research");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    std::fs::write(dir.path().join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{manifest}")).unwrap();
    dir
}

pub(crate) fn cf(dir: &Path, args: &[&str]) -> Output {
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

pub(crate) fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

pub(crate) fn stderr(out: &Output) -> String {
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
    assert!(err.contains("contextful.toml:4") && err.contains("pipelines/orders.toml:3"), "{err}");
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
        project(&format!("{}{seed}{FOLD_ITEMS}", pipeline("orders", "https://api.vendor.example/v1", "", tables)))
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

/// A scheduled fold of the seeded `orders_items` table.
const FOLD_ITEMS: &str = "\n[[job]]\nname = \"fold-items\"\nschedule = \"0 3 * * *\"\nkind = \"fold\"\ntarget = \"orders_items\"\n";

const KEYED_ITEMS: &str = "[[pipeline.tables]]\nname = \"items\"\nprimary_key = [\"id\"]\norder_by = \"updated_at\"\n";

/// `pipeline validate` warns on stderr, naming the table, where a table declares `primary_key` and no job
/// meeting {{store.declare.fold-job}} covers it; the warning alone fails nothing.
// spec: store.declare.fold-coverage@99fcbb4e
#[test]
fn a_keyed_table_no_fold_job_covers_warns_and_validates() {
    let keyed = pipeline("orders", "https://api.vendor.example/v1", "", KEYED_ITEMS);
    let warned = |manifest: &str| -> Vec<String> {
        let out = cf(project(manifest).path(), &["pipeline", "validate"]);
        ok(&out);
        stderr(&out).lines().filter(|l| l.contains("warning")).map(str::to_string).collect()
    };
    let uncovered = warned(&keyed);
    assert_eq!(uncovered.len(), 1, "{uncovered:?}");
    assert!(uncovered[0].contains("`orders_items`") && uncovered[0].contains("fold"), "{uncovered:?}");

    assert!(warned(&format!("{keyed}{FOLD_ITEMS}")).is_empty());
    assert!(warned(&format!("{keyed}\n[[job]]\nname = \"all\"\nschedule = \"every 6h\"\nkind = \"fold\"\n")).is_empty());
    assert_eq!(warned(&format!("{keyed}{FOLD_ITEMS}enabled = false\n")).len(), 1, "a disabled job covers nothing");
    assert_eq!(warned(&format!("{keyed}{}", FOLD_ITEMS.replace("orders_items", "orders_other"))).len(), 1);

    // A keyless table reads as a union and needs no fold; a store table keyed under `[pipeline]` does.
    assert!(warned(&pipeline("orders", "https://api.vendor.example/v1", "", "tables = [\"items\"]")).is_empty());
    let store_table = warned("[[pipeline.tables]]\nname = \"filings\"\nprimary_key = [\"id\"]\n");
    assert!(store_table.len() == 1 && store_table[0].contains("`filings`"), "{store_table:?}");

    // A `[[table]]` memory table is keyed on its shape's key.
    let memory = "[[table]]\nname = \"ents\"\nshape = \"memory_entities\"\ncolumns = [\"entity_id\", \"kind\", \"name\", \"aliases\"]\n";
    let uncovered = warned(memory);
    assert!(uncovered.len() == 1 && uncovered[0].contains("`ents`"), "{uncovered:?}");
    assert!(warned(&format!("{memory}{}", FOLD_ITEMS.replace("orders_items", "ents"))).is_empty());
}

/// A seeded table that no job meeting {{store.declare.fold-job}} covers raises `PipelineSeedCompactionMissing`
/// at validate and before a fire, naming the table.
// spec: run.seed.compaction-cadence@1c29dc12
#[test]
fn a_seeded_table_no_fold_job_covers_is_refused() {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"a\",\"updated_at\":\"2029-01-01T00:00:00Z\"}]".into()));
    let seed = format!(
        "[pipeline.seed]\nbelow = \"2030-01-01T00:00:00Z\"\n[pipeline.seed.source]\nname = \"http\"\nconfig = {{ endpoint = \"{}\" }}\n",
        vendor.url("/export")
    );
    let manifest = format!("{}{seed}", pipeline("orders", &vendor.url("/v1"), "", KEYED_ITEMS));
    let dir = project(&manifest);
    let out = cf(dir.path(), &["pipeline", "validate"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("PipelineSeedCompactionMissing") && err.contains("`orders_items`"), "{err}");

    let out = fire(dir.path(), "orders", "r1", "2030-01-01T00:00:00Z");
    assert!(!out.status.success());
    assert!(stderr(&out).contains("PipelineSeedCompactionMissing"), "{}", stderr(&out));
    assert!(vendor.targets().is_empty(), "the fire refuses before any request: {:?}", vendor.targets());

    ok(&cf(project(&format!("{manifest}{FOLD_ITEMS}")).path(), &["pipeline", "validate"]));
    let disabled = cf(project(&format!("{manifest}{FOLD_ITEMS}enabled = false\n")).path(), &["pipeline", "validate"]);
    assert!(stderr(&disabled).contains("PipelineSeedCompactionMissing"), "{}", stderr(&disabled));
}

/// `pipeline validate` reading no manifest file raises `PipelineManifestMissing`, naming the declaration path.
// spec: run.declare.manifest-missing@0f5308ec
#[test]
fn validate_over_no_manifest_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let out = cf(dir.path(), &["pipeline", "validate"]);
    assert!(!out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let err = stderr(&out);
    assert!(err.contains("PipelineManifestMissing") && err.contains("contextful.toml"), "{err}");

    let out = cf(dir.path(), &["pipeline", "validate", "--declaration", "conf/typo.toml"]);
    assert!(stderr(&out).contains("PipelineManifestMissing") && stderr(&out).contains("conf/typo.toml"), "{}", stderr(&out));

    // A directory is no manifest file.
    std::fs::create_dir_all(dir.path().join("conf")).unwrap();
    let out = cf(dir.path(), &["pipeline", "validate", "--declaration", "conf"]);
    assert!(stderr(&out).contains("PipelineManifestMissing") && stderr(&out).contains("`conf`"), "{}", stderr(&out));

    // A pipelines directory alone is a manifest.
    std::fs::create_dir_all(dir.path().join("pipelines")).unwrap();
    std::fs::write(
        dir.path().join("pipelines/a.toml"),
        "id = \"a\"\ntables = [\"t\"]\n[source]\nname = \"http\"\nconfig = { endpoint = \"https://api.vendor.example/v1\" }\n",
    )
    .unwrap();
    ok(&cf(dir.path(), &["pipeline", "validate"]));
}

fn metered_project(vendor: &Vendor, limiters: &str) -> tempfile::TempDir {
    project(&format!(
        "{limiters}\n[[pipeline]]\nid = \"shop\"\ntables = [\"orders\"]\n[pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"{}\", page_param = \"p\", limiter = {{ quota = \"vendor-app\", class = \"batch-read\" }} }}\n",
        vendor.url("/v1/{table}")
    ))
}

fn fire_with(dir: &Path, run: &str, env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_contextful"));
    cmd.args(["pipeline", "run", "shop", "--project", "research", "--run-id", run, "--site-id", "site-a", "--now", "2030-01-01T00:00:00Z"])
        .current_dir(dir)
        .env_remove("CONTEXTFUL_NODE_ID")
        .env_remove("CONTEXTFUL_SECRETS_BACKEND");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

/// The generic HTTP source reads its limiter declaration from a `limiter` config table, and the project manifest holds
/// each limiter binding under `[limiters.<quota>]`.
// spec: connector.source.http-limiter@b6550a91
#[test]
fn a_bound_quota_meters_every_page_of_a_run() {
    let vendor = Vendor::start(|t| match t {
        "/v1/orders?p=1" => (200, "[{\"id\":\"a\"}]".into()),
        "/v1/orders?p=2" => (200, "[{\"id\":\"b\"}]".into()),
        _ => (200, "[]".into()),
    });
    let limiter = Vendor::start(|t| match t {
        "/lim/acquire" => (200, "{\"decision\":\"granted\",\"permits\":1,\"ttl_secs\":60}".into()),
        _ => (204, String::new()),
    });
    let binding = format!("[limiters.vendor-app]\nendpoint = \"{}\"\ntoken = \"secret://limiter-token\"\npermits = 1\n", limiter.url("/lim"));
    let dir = metered_project(&vendor, &binding);
    let out = fire_with(dir.path(), "m1", &[("LIMITER_TOKEN", "lim-1"), ("CONTEXTFUL_SECRETS_ALLOW_ENV_TEMPLATES", "1")]);
    assert!(ok(&out).contains("2 rows in 2 batches"), "{}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(vendor.targets(), ["/v1/orders?p=1", "/v1/orders?p=2", "/v1/orders?p=3"]);
    let calls = limiter.targets();
    assert_eq!(calls.iter().filter(|t| *t == "/lim/acquire").count(), 3, "{calls:?}");
    assert_eq!(calls.last().map(String::as_str), Some("/lim/report"), "{calls:?}");

    // A declared quota the manifest leaves unbound refuses at validation and before any vendor request.
    let vendor = Vendor::start(|_| (200, "[]".into()));
    let dir = metered_project(&vendor, "");
    let err = stderr(&cf(dir.path(), &["pipeline", "validate"]));
    assert!(err.contains("ConnectorQuotaUnbound") && err.contains("vendor-app"), "{err}");
    let out = fire_with(dir.path(), "m2", &[]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("ConnectorQuotaUnbound"), "{}", stderr(&out));
    assert!(vendor.targets().is_empty());
}

const CONTROL: &str = ".contextful/control/research";

fn scheduled(id: &str, endpoint: &str, schedule: &str) -> String {
    pipeline(id, endpoint, &format!("schedule = \"{schedule}\""), "tables = [\"items\"]")
}

fn json(out: &Output) -> serde_json::Value {
    let text = ok(out);
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

fn serve_cycle(dir: &Path, now: &str) -> serde_json::Value {
    json(&cf(dir, &["pipeline", "serve", "--cycle", "--project", "research", "--now", now]))
}

fn history(dir: &Path) -> Vec<serde_json::Value> {
    let out = ok(&cf(dir, &["run", "history", "--project", "research", "--export"]));
    out.lines().skip(1).map(|l| serde_json::from_str(l).unwrap()).collect()
}

/// `plan` diffs desired state against the store with no side effect, `--json` emitting a structured diff; `apply`
/// converges every pipeline or one; `run` fires one pipeline once; `serve` reconciles continuously.
// spec: run.declare.lifecycle-verbs@c2262175
#[test]
fn plan_diffs_apply_converges_and_serve_fires() {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"a\"}]".into()));
    let dir = project(&format!("site_id = \"site-a\"\n\n{}", scheduled("orders", &vendor.url("/v1/orders"), "every 1h")));
    let plan = json(&cf(dir.path(), &["pipeline", "plan", "--json", "--project", "research"]));
    assert_eq!(plan["applied"], serde_json::Value::Null);
    assert_eq!(plan["pipelines"][0]["id"], "orders");
    assert_eq!(plan["pipelines"][0]["action"], "add");
    assert_eq!(plan["pipelines"][0]["schedule"], "every 1h");
    assert!(!dir.path().join(".contextful/control").exists(), "plan writes nothing");
    assert!(ok(&cf(dir.path(), &["pipeline", "plan", "--project", "research"])).contains("+ orders"));

    ok(&cf(dir.path(), &["pipeline", "apply", "--project", "research"]));
    let plan = json(&cf(dir.path(), &["pipeline", "plan", "--json", "--project", "research"]));
    assert_eq!((plan["applied"].clone(), plan["pipelines"][0]["action"].clone()), (serde_json::json!(1), serde_json::json!("unchanged")));

    // `apply <id>` converges that pipeline alone.
    std::fs::write(
        dir.path().join("contextful.toml"),
        format!("site_id = \"site-a\"\n\n{}\n{}", scheduled("orders", &vendor.url("/v2/orders"), "every 1h"), scheduled("filings", &vendor.url("/v1/filings"), "every 1d")),
    )
    .unwrap();
    ok(&cf(dir.path(), &["pipeline", "apply", "filings", "--project", "research"]));
    let plan = json(&cf(dir.path(), &["pipeline", "plan", "--json", "--project", "research"]));
    let actions: Vec<(String, String)> =
        plan["pipelines"].as_array().unwrap().iter().map(|p| (p["id"].as_str().unwrap().into(), p["action"].as_str().unwrap().into())).collect();
    assert_eq!(actions, [("filings".to_string(), "unchanged".to_string()), ("orders".to_string(), "change".to_string())]);

    // `serve --cycle` fires the applied specification of each due pipeline once.
    let answer = serve_cycle(dir.path(), "2030-01-01T00:00:00Z");
    assert_eq!(answer["fired"], serde_json::json!(["filings", "orders"]));
    assert_eq!(answer["armed"], 2);
    let mut targets = vendor.targets();
    targets.sort();
    assert_eq!(targets, ["/v1/filings", "/v1/orders"], "orders fires as applied, not as the unapplied edit declares");
}

/// Applying a manifest fires nothing of itself; a second `apply` over unchanged sources is a no-op modulo
/// elapsed schedules.
// spec: run.declare.apply-fires-nothing@89dd7f54
#[test]
fn apply_fires_nothing_and_a_second_apply_is_a_no_op() {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"a\"}]".into()));
    let dir = project(&format!("site_id = \"site-a\"\n\n{}", scheduled("orders", &vendor.url("/v1/orders"), "every 1h")));
    let first = ok(&cf(dir.path(), &["pipeline", "apply", "--project", "research"]));
    assert!(first.contains("applied v1"), "{first}");
    assert!(dir.path().join(CONTROL).join("manifest@v1.toml").exists());
    assert!(vendor.targets().is_empty(), "apply reached no source");
    assert!(history(dir.path()).is_empty(), "apply journaled no run");
    let second = ok(&cf(dir.path(), &["pipeline", "apply", "--project", "research"]));
    assert!(second.contains("unchanged at v1"), "{second}");
    assert!(!dir.path().join(CONTROL).join("manifest@v2.toml").exists());
    assert_eq!(std::fs::read_to_string(dir.path().join(CONTROL).join("manifest@current")).unwrap(), "1\n");
    // The first fire happens when serve arms it, not before.
    serve_cycle(dir.path(), "2030-01-01T00:00:00Z");
    assert_eq!(vendor.targets(), ["/v1/orders"]);
    assert_eq!(history(dir.path()).len(), 1);
}

/// A document failing engine validation raises `ApplyValidationRefused` and claims no version.
// spec: surface.apply.validation@17970d8f
#[test]
fn an_invalid_document_claims_no_version() {
    let dir = project(&format!(
        "site_id = \"site-a\"\n\n{}\n{}",
        scheduled("orders", "https://api.vendor.example/v1", "every 1h"),
        pipeline("broken", "https://api.vendor.example/v1", "destination = { name = \"warehouse\" }", "tables = [\"a\"]")
    ));
    let out = cf(dir.path(), &["pipeline", "apply", "--project", "research"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("ApplyValidationRefused") && err.contains("broken") && err.contains("PipelineUnknownDestination"), "{err}");
    assert!(!dir.path().join(CONTROL).exists(), "no version claimed");
    // Applying the valid pipeline alone validates only the one it converges.
    ok(&cf(dir.path(), &["pipeline", "apply", "orders", "--project", "research"]));
    assert!(dir.path().join(CONTROL).join("manifest@v1.toml").exists());
}

/// A local control plane validates and claims `manifest@v<N>.toml` in its snapshot directory,
/// `.contextful/control/<project>/` unless `[control] snapshot_dir` names one, on its own; `contextful pipeline
/// apply` is that apply, and no hosted plane sits on its path.
// spec: surface.apply.local-claim@f283cd8c
#[test]
fn apply_claims_a_version_in_the_local_snapshot_directory() {
    let dir = project(&format!("site_id = \"site-a\"\n\n{}", scheduled("orders", "https://api.vendor.example/v1", "0 3 * * *")));
    assert!(ok(&cf(dir.path(), &["pipeline", "apply", "--project", "research"])).contains("applied v1"));
    let claimed = std::fs::read_to_string(dir.path().join(CONTROL).join("manifest@v1.toml")).unwrap();
    assert!(claimed.contains("[[pipeline]]") && claimed.contains("id = \"orders\"") && claimed.contains("0 3 * * *"), "{claimed}");
    // `[control] snapshot_dir` moves the directory.
    let dir = project(&format!(
        "site_id = \"site-a\"\n\n[control]\nsnapshot_dir = \"ops/control\"\n\n{}",
        scheduled("orders", "https://api.vendor.example/v1", "0 3 * * *")
    ));
    assert!(ok(&cf(dir.path(), &["pipeline", "apply", "--project", "research"])).contains("applied v1"));
    assert_eq!(std::fs::read_to_string(dir.path().join("ops/control/manifest@current")).unwrap(), "1\n");
    assert!(!dir.path().join(CONTROL).exists());
}

/// `serve --cycle` arms the applied snapshot, evaluates due-ness once, waits for every unit it dispatched, and
/// prints what fired, what failed, what stays pending, the armed count and the next due instant.
// spec: surface.fire.cycle@fbba08b7
#[test]
fn a_cycle_fires_what_is_due_once_and_reports_the_next_instant() {
    let vendor = Vendor::start(|t| if t.starts_with("/v1/bad") { (404, "{}".into()) } else { (200, "[{\"id\":\"a\"}]".into()) });
    let dir = project(&format!(
        "site_id = \"site-a\"\n\n[control]\npool = 1\n\n{}\n{}\n{}",
        scheduled("orders", &vendor.url("/v1/orders"), "every 1h"),
        scheduled("bad", &vendor.url("/v1/bad"), "every 1h"),
        scheduled("nightly", &vendor.url("/v1/nightly"), "0 3 * * *"),
    ));
    ok(&cf(dir.path(), &["pipeline", "apply", "--project", "research"]));
    // A pool of one: the first due unit by id fires, the others stay pending.
    let a = serve_cycle(dir.path(), "2030-01-01T00:00:00Z");
    assert_eq!(a["fired"], serde_json::json!([]));
    assert_eq!(a["failed"], serde_json::json!(["bad"]));
    assert_eq!(a["pending"], serde_json::json!(["nightly", "orders"]));
    assert_eq!(a["armed"], 3);
    let b = serve_cycle(dir.path(), "2030-01-01T00:00:00Z");
    assert_eq!((b["fired"].clone(), b["pending"].clone()), (serde_json::json!(["nightly"]), serde_json::json!(["orders"])));
    let c = serve_cycle(dir.path(), "2030-01-01T00:00:00Z");
    assert_eq!(c["fired"], serde_json::json!(["orders"]));
    assert_eq!(c["next_due"], "2030-01-01T01:00:00Z");
    // Nothing further is due in the same instant.
    let d = serve_cycle(dir.path(), "2030-01-01T00:00:00Z");
    assert_eq!((d["fired"].clone(), d["failed"].clone(), d["pending"].clone()), (serde_json::json!([]), serde_json::json!([]), serde_json::json!([])));
    // Five hours on, each pipeline fires once, not once per missed interval.
    let mut fired: Vec<String> = Vec::new();
    for _ in 0..4 {
        let x = serve_cycle(dir.path(), "2030-01-01T05:00:00Z");
        fired.extend(x["fired"].as_array().unwrap().iter().chain(x["failed"].as_array().unwrap()).map(|v| v.as_str().unwrap().to_string()));
    }
    assert_eq!(fired, ["bad", "orders", "nightly"], "due order, one per cycle, then nothing");
    assert_eq!(vendor.targets().iter().filter(|t| t.starts_with("/v1/orders")).count(), 2);
}

/// A configured control source that does not resolve under `cycle` raises `CycleControlSourceUnresolved`.
// spec: surface.fire.cycle-control-source@e888c4f1
#[test]
fn a_cycle_with_no_applied_snapshot_is_refused() {
    let dir = project(&format!("site_id = \"site-a\"\n\n{}", scheduled("orders", "https://api.vendor.example/v1", "every 1h")));
    assert!(dir.path().join("contextful.toml").exists(), "a pipeline is declared, only never applied");
    let out = cf(dir.path(), &["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T00:00:00Z"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("CycleControlSourceUnresolved"), "{}", stderr(&out));
}

/// An entry whose schedule the grammar cannot read is held back by name; every other entry arms.
#[test]
fn an_unreadable_schedule_holds_back_its_entry_alone() {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"a\"}]".into()));
    let dir = project(&format!(
        "site_id = \"site-a\"\n\n{}\n{}",
        scheduled("orders", &vendor.url("/v1/orders"), "every 1h"),
        scheduled("odd", &vendor.url("/v1/odd"), "0 0 L * *"),
    ));
    ok(&cf(dir.path(), &["pipeline", "apply", "--project", "research"]));
    let out = cf(dir.path(), &["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T00:00:00Z"]);
    let answer: serde_json::Value = serde_json::from_str(&ok(&out)).unwrap();
    assert_eq!(answer["fired"], serde_json::json!(["orders"]));
    assert_eq!(answer["armed"], 1);
    let err = stderr(&out);
    assert!(err.contains("ScheduleUnreadable") && err.contains("odd"), "{err}");
}

/// A malformed pointer refuses the cycle rather than arming a version nobody applied.
#[test]
fn a_malformed_pointer_refuses_the_cycle() {
    let dir = project(&format!("site_id = \"site-a\"\n\n{}", scheduled("orders", "https://api.vendor.example/v1", "every 1h")));
    ok(&cf(dir.path(), &["pipeline", "apply", "--project", "research"]));
    std::fs::write(dir.path().join(CONTROL).join("manifest@current"), "1; drop").unwrap();
    let out = cf(dir.path(), &["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T00:00:00Z"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("ControlPointerMalformed"), "{}", stderr(&out));
}
