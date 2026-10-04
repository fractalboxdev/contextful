//! `contextful pipeline` through the built binary, against a loopback vendor.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

/// A loopback vendor answering `handler(target)` with `(status, body)`, recording every target.
pub(crate) struct Vendor {
    port: u16,
    targets: Arc<Mutex<Vec<String>>>,
}

impl Vendor {
    pub(crate) fn start(handler: impl Fn(&str) -> (u16, String) + Send + Sync + 'static) -> Vendor {
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

    pub(crate) fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    pub(crate) fn targets(&self) -> Vec<String> {
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
    answer(&cf(dir, &["pipeline", "serve", "--cycle", "--project", "research", "--now", now]))
}

/// A cycle's printed answer, whatever its exit status.
fn answer(out: &Output) -> serde_json::Value {
    let text = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("{e}: {text}\n{}", stderr(out)))
}

fn history(dir: &Path) -> Vec<serde_json::Value> {
    let out = ok(&cf(dir, &["run", "history", "--project", "research", "--export"]));
    out.lines().skip(1).map(|l| serde_json::from_str(l).unwrap()).collect()
}

/// `plan` diffs against the applied snapshot, `apply` converges every pipeline or one, and `serve --cycle` fires
/// the applied specification.
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

    ok(&cf(dir.path(), &["pipeline", "import", "--project", "research"]));
    let plan = json(&cf(dir.path(), &["pipeline", "plan", "--json", "--project", "research"]));
    assert_eq!((plan["applied"].clone(), plan["pipelines"][0]["action"].clone()), (serde_json::json!(1), serde_json::json!("unchanged")));

    // `apply <id>` converges that pipeline alone.
    std::fs::write(
        dir.path().join("contextful.toml"),
        format!("authoring_posture = \"per_request\"\nsite_id = \"site-a\"\n\n{}\n{}", scheduled("orders", &vendor.url("/v2/orders"), "every 1h"), scheduled("filings", &vendor.url("/v1/filings"), "every 1d")),
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
    let first = ok(&cf(dir.path(), &["pipeline", "import", "--project", "research"]));
    assert!(first.contains("imported v1"), "{first}");
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
    let broken = pipeline("broken", "https://api.vendor.example/v1", "destination = { name = \"warehouse\" }", "tables = [\"a\"]");
    let dir = project(&format!("site_id = \"site-a\"\n\n{}\n{broken}", scheduled("orders", "https://api.vendor.example/v1", "every 1h")));
    // The import validates every declared pipeline and claims nothing past a refusal.
    let out = cf(dir.path(), &["pipeline", "import", "--project", "research"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("ApplyValidationRefused") && err.contains("broken") && err.contains("PipelineUnknownDestination"), "{err}");
    assert!(!dir.path().join(CONTROL).exists(), "no version claimed");
    std::fs::write(dir.path().join("contextful.toml"), format!("site_id = \"site-a\"\n\n{}", scheduled("orders", "https://api.vendor.example/v1", "every 1h"))).unwrap();
    ok(&cf(dir.path(), &["pipeline", "import", "--project", "research"]));
    std::fs::write(
        dir.path().join("contextful.toml"),
        format!("site_id = \"site-a\"\n\n{}\n{broken}", scheduled("orders", "https://api.vendor.example/v1", "every 2h")),
    )
    .unwrap();
    let out = cf(dir.path(), &["pipeline", "apply", "--project", "research"]);
    let err = stderr(&out);
    assert!(!out.status.success() && err.contains("ApplyValidationRefused") && err.contains("broken"), "{err}");
    assert!(!dir.path().join(CONTROL).join("manifest@v2.toml").exists(), "no version claimed");
    // Applying the valid pipeline alone validates only the one it converges.
    ok(&cf(dir.path(), &["pipeline", "apply", "orders", "--project", "research"]));
    assert!(dir.path().join(CONTROL).join("manifest@v2.toml").exists());
}

/// A local control plane validates and claims `manifest@v<N>.toml` in its snapshot directory,
/// `.contextful/control/<project>/` unless `[control] snapshot_dir` names one, on its own; `contextful pipeline
/// apply` is that apply, and no hosted plane sits on its path.
// spec: surface.apply.local-claim@f283cd8c
#[test]
fn apply_claims_a_version_in_the_local_snapshot_directory() {
    let dir = project(&format!("site_id = \"site-a\"\n\n{}", scheduled("orders", "https://api.vendor.example/v1", "0 3 * * *")));
    assert!(ok(&cf(dir.path(), &["pipeline", "import", "--project", "research"])).contains("imported v1"));
    let claimed = std::fs::read_to_string(dir.path().join(CONTROL).join("manifest@v1.toml")).unwrap();
    assert!(claimed.contains("[[pipeline]]") && claimed.contains("id = \"orders\"") && claimed.contains("0 3 * * *"), "{claimed}");
    // `[control] snapshot_dir` moves the directory.
    let dir = project(&format!(
        "site_id = \"site-a\"\n\n[control]\nsnapshot_dir = \"ops/control\"\n\n{}",
        scheduled("orders", "https://api.vendor.example/v1", "0 3 * * *")
    ));
    assert!(ok(&cf(dir.path(), &["pipeline", "import", "--project", "research"])).contains("imported v1"));
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
    ok(&cf(dir.path(), &["pipeline", "import", "--project", "research"]));
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
    ok(&cf(dir.path(), &["pipeline", "import", "--project", "research"]));
    let out = cf(dir.path(), &["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T00:00:00Z"]);
    let answer: serde_json::Value = serde_json::from_str(&ok(&out)).unwrap();
    assert_eq!(answer["fired"], serde_json::json!(["orders"]));
    assert_eq!(answer["armed"], 1);
    let err = stderr(&out);
    assert!(err.contains("ScheduleUnreadable") && err.contains("odd"), "{err}");
}

/// An applied pipeline declaring no schedule, or one whose schedule is unreadable, stays unarmed, and serve
/// names each with its reason.
// spec: surface.arm.unarmed-named@2311f6c3
#[test]
fn serve_names_each_pipeline_it_does_not_arm() {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"a\"}]".into()));
    let dir = project(&format!(
        "site_id = \"site-a\"\n\n{}\n{}\n{}",
        scheduled("orders", &vendor.url("/v1/orders"), "every 1h"),
        pipeline("manual", &vendor.url("/v1/manual"), "", "tables = [\"items\"]"),
        scheduled("odd", &vendor.url("/v1/odd"), "0 0 L * *"),
    ));
    ok(&cf(dir.path(), &["pipeline", "import", "--project", "research"]));
    ok(&cf(dir.path(), &["pipeline", "apply", "--project", "research"]));
    let out = cf(dir.path(), &["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T00:00:00Z"]);
    let a = json(&out);
    assert_eq!((a["fired"].clone(), a["armed"].clone()), (serde_json::json!(["orders"]), serde_json::json!(1)));
    let unarmed = a["unarmed"].as_array().unwrap_or_else(|| panic!("no `unarmed` list: {a}"));
    let ids: Vec<&str> = unarmed.iter().map(|u| u["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["manual", "odd"]);
    assert!(unarmed[0]["reason"].as_str().unwrap().contains("no `schedule`"), "{a}");
    assert!(unarmed[1]["reason"].as_str().unwrap().contains("ScheduleUnreadable"), "{a}");
    let err = stderr(&out);
    assert!(err.contains("pipeline `manual` stays unarmed") && err.contains("pipeline `odd` stays unarmed"), "{err}");
    assert_eq!(vendor.targets(), ["/v1/orders"], "an unarmed pipeline fires only through `pipeline run`");
}

/// `serve --cycle` exits non-zero when any unit it dispatched fails, after printing its answer.
// spec: surface.fire.cycle-exit@7f2e29d2
#[test]
fn a_cycle_with_a_failed_fire_exits_non_zero() {
    let vendor = Vendor::start(|t| if t.starts_with("/v1/bad") { (404, "{}".into()) } else { (200, "[{\"id\":\"a\"}]".into()) });
    let dir = project(&format!(
        "site_id = \"site-a\"\n\n{}\n{}",
        scheduled("orders", &vendor.url("/v1/orders"), "every 1h"),
        scheduled("bad", &vendor.url("/v1/bad"), "every 1h"),
    ));
    ok(&cf(dir.path(), &["pipeline", "import", "--project", "research"]));
    ok(&cf(dir.path(), &["pipeline", "apply", "--project", "research"]));
    let out = cf(dir.path(), &["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T00:00:00Z"]);
    assert!(!out.status.success(), "a failed fire exits non-zero");
    let a = answer(&out);
    assert_eq!((a["fired"].clone(), a["failed"].clone()), (serde_json::json!(["orders"]), serde_json::json!(["bad"])));
    assert!(stderr(&out).contains("`bad`"), "{}", stderr(&out));
    // A cycle with nothing due fires nothing and exits zero.
    let quiet = json(&cf(dir.path(), &["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T00:00:00Z"]));
    assert_eq!(quiet["failed"], serde_json::json!([]));
}

/// A malformed pointer refuses the cycle rather than arming a version nobody applied.
#[test]
fn a_malformed_pointer_refuses_the_cycle() {
    let dir = project(&format!("site_id = \"site-a\"\n\n{}", scheduled("orders", "https://api.vendor.example/v1", "every 1h")));
    ok(&cf(dir.path(), &["pipeline", "import", "--project", "research"]));
    std::fs::write(dir.path().join(CONTROL).join("manifest@current"), "1; drop").unwrap();
    let out = cf(dir.path(), &["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T00:00:00Z"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("ControlPointerMalformed"), "{}", stderr(&out));
}

/// A running `pipeline serve`, its stderr collected line by line.
struct Daemon {
    child: std::process::Child,
    lines: Arc<Mutex<Vec<String>>>,
}

impl Daemon {
    fn start(dir: &Path) -> Daemon {
        Daemon::start_with(dir, &[])
    }

    fn start_with(dir: &Path, extra: &[&str]) -> Daemon {
        let mut child = Command::new(env!("CARGO_BIN_EXE_contextful"))
            .args(["pipeline", "serve", "--project", "research"])
            .args(extra)
            .current_dir(dir)
            .env_remove("CONTEXTFUL_NODE_ID")
            .env_remove("CONTEXTFUL_SECRETS_BACKEND")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let lines: Arc<Mutex<Vec<String>>> = Arc::default();
        let (sink, err) = (lines.clone(), child.stderr.take().unwrap());
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                sink.lock().unwrap().push(line);
            }
        });
        Daemon { child, lines }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }

    /// Wait up to 60 s for a line after index `from` holding `needle`, answering its index.
    fn wait_for(&self, needle: &str, from: usize) -> usize {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            if let Some(i) = self.lines().iter().enumerate().skip(from).find(|(_, l)| l.contains(needle)).map(|(i, _)| i) {
                return i;
            }
            assert!(std::time::Instant::now() < deadline, "no `{needle}` after line {from}:\n{}", self.lines().join("\n"));
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    /// Send `SIGTERM` and wait up to 60 s for the exit.
    fn terminate(&mut self) -> std::process::ExitStatus {
        signal(self.pid(), "TERM");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(std::time::Instant::now() < deadline, "serve outlived SIGTERM:\n{}", self.lines().join("\n"));
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `plan` diffs desired state against the store with no side effect, `--json` emitting a structured diff; `apply`
/// converges every pipeline or one; `run` fires one pipeline once; `serve` reconciles continuously.
// spec: run.declare.lifecycle-verbs@c2262175
#[test]
fn serve_reconciles_continuously_and_rearms_each_applied_version() {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"a\"}]".into()));
    let manifest = |path: &str| format!("site_id = \"site-a\"\n\n[control]\npoll = \"every 1s\"\n\n{}", scheduled("orders", &vendor.url(path), "every 2s"));
    let dir = project(&manifest("/v1/orders"));
    let plan = json(&cf(dir.path(), &["pipeline", "plan", "--json", "--project", "research"]));
    assert_eq!((plan["applied"].clone(), plan["pipelines"][0]["action"].clone()), (serde_json::Value::Null, serde_json::json!("add")));
    assert!(!dir.path().join(".contextful/control").exists(), "plan writes nothing");
    assert!(ok(&cf(dir.path(), &["pipeline", "import", "--project", "research"])).contains("imported v1"));
    assert!(vendor.targets().is_empty(), "import fires nothing");
    let daemon = Daemon::start(dir.path());
    let armed = daemon.wait_for("armed v1: 1 scheduled pipeline(s)", 0);
    // No run history: the entry fires on boot, then again one interval on.
    let first = daemon.wait_for("fire orders: done", armed);
    daemon.wait_for("fire orders: done", first + 1);
    assert!(vendor.targets().iter().all(|t| t == "/v1/orders"), "{:?}", vendor.targets());

    // An apply while serve runs re-arms the new version on the next poll.
    std::fs::write(dir.path().join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{}", manifest("/v2/orders"))).unwrap();
    assert!(ok(&cf(dir.path(), &["pipeline", "apply", "--project", "research"])).contains("applied v2"));
    let rearmed = daemon.wait_for("armed v2: 1 scheduled pipeline(s)", armed);
    daemon.wait_for("fire orders: done", rearmed);
    assert!(vendor.targets().iter().any(|t| t == "/v2/orders"), "{:?}", vendor.targets());

    // A malformed pointer leaves the armed set running.
    std::fs::write(dir.path().join(CONTROL).join("manifest@current"), "2; drop").unwrap();
    let malformed = daemon.wait_for("ControlPointerMalformed", rearmed);
    assert!(daemon.lines()[malformed].contains("the armed set stays in place"), "{}", daemon.lines()[malformed]);
    let before = vendor.targets().len();
    daemon.wait_for("fire orders: done", malformed);
    assert!(vendor.targets().len() > before);
    assert!(vendor.targets()[before..].iter().all(|t| t == "/v2/orders"), "{:?}", vendor.targets());
}

fn serve_cycle_live(dir: &Path) -> serde_json::Value {
    json(&cf(dir, &["pipeline", "serve", "--cycle", "--project", "research"]))
}

/// A serve process dispatches only while it holds its deployment's cadence lease; a process finding the lease
/// held arms nothing and, under `--cycle`, exits naming the holder.
// spec: surface.dispatch.lease-gated@f1237082
#[test]
fn a_cycle_under_a_running_daemon_arms_nothing_and_names_the_holder() {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"a\"}]".into()));
    let dir = project(&format!("site_id = \"site-a\"\n\n{}", scheduled("orders", &vendor.url("/v1/orders"), "every 1h")));
    ok(&cf(dir.path(), &["pipeline", "import", "--project", "research"]));
    let mut daemon = Daemon::start(dir.path());
    daemon.wait_for("fire orders: done", 0);

    let out = cf(dir.path(), &["pipeline", "serve", "--cycle", "--project", "research"]);
    let answer = json(&out);
    let holder = answer["held_by"].as_str().unwrap_or_else(|| panic!("{answer}"));
    assert!(holder.ends_with(&format!(":{}", daemon.pid())), "{holder}");
    assert_eq!((answer["fired"].clone(), answer.get("armed")), (serde_json::json!([]), None), "{answer}");
    assert!(!stderr(&out).contains("armed v"), "{}", stderr(&out));
    assert_eq!(vendor.targets(), ["/v1/orders"], "the held cycle dispatched nothing");

    // SIGTERM stops the daemon, which releases the lease on its way out.
    assert!(daemon.terminate().success(), "{}", daemon.lines().join("\n"));
    daemon.wait_for("stopped", 0);
    let answer = serve_cycle_live(dir.path());
    assert_eq!((answer.get("held_by"), answer["armed"].clone()), (None, serde_json::json!(1)), "{answer}");
}

/// A control URL whose host is not a loopback address raises `ControlSourceNotLoopback` and arms nothing; a poll
/// follows no redirect and routes through no proxy.
// spec: surface.reconcile.loopback-only@3c8e166e
#[test]
fn a_control_url_outside_loopback_arms_nothing() {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"a\"}]".into()));
    let control = Vendor::start(|t| match t {
        "/moved/manifest@current" => (302, String::new()),
        _ => (503, String::new()),
    });
    // A redirect or a `5xx` reads as unreadable, never as a version.
    for (path, status) in [("/moved", "302"), ("/down", "503")] {
        let dir = project(&format!("site_id = \"site-a\"\n\n[control]\nurl = \"{}\"\n", control.url(path)));
        let out = cf(dir.path(), &["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T00:00:00Z"]);
        let err = stderr(&out);
        assert!(!out.status.success() && err.contains("ControlSnapshotUnreadable") && err.contains(status), "{err}");
    }
    // A host outside loopback arms nothing and reaches nothing.
    let dir = project(&format!(
        "site_id = \"site-a\"\n\n[control]\nurl = \"http://10.255.255.1:8787/control\"\n\n{}",
        scheduled("orders", &vendor.url("/v1/orders"), "every 1h")
    ));
    let out = cf(dir.path(), &["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T00:00:00Z"]);
    assert!(!out.status.success() && stderr(&out).contains("ControlSourceNotLoopback"), "{}", stderr(&out));
    assert!(vendor.targets().is_empty());
}

/// A control URL serves `manifest@current` and each `manifest@v<N>.toml` directly beneath its path; a pointer
/// answered `404` reads as no applied version, and any other status besides `200` is unreadable.
// spec: surface.reconcile.url-layout@5ce53a55
#[test]
fn a_loopback_control_url_serves_the_applied_snapshot() {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"a\"}]".into()));
    // A plane's snapshot directory, claimed by an apply and served over loopback HTTP.
    let plane = project(&format!("site_id = \"site-a\"\n\n{}", scheduled("orders", &vendor.url("/v1/orders"), "every 1h")));
    let root = plane.path().join(CONTROL);
    let control = Vendor::start(move |t| match t.strip_prefix("/control/") {
        Some(file) => std::fs::read_to_string(root.join(file)).map(|b| (200, b)).unwrap_or((404, String::new())),
        None => (404, String::new()),
    });
    let dir = project(&format!(
        "site_id = \"site-a\"\n\n[control]\nurl = \"{}\"\n\n{}",
        control.url("/control"),
        scheduled("orders", "https://changed.example/v1", "every 1h")
    ));
    // Before the plane's first apply the pointer answers `404`: no applied version.
    let plan = json(&cf(dir.path(), &["pipeline", "plan", "--json", "--project", "research"]));
    assert_eq!(plan["applied"], serde_json::Value::Null);
    let out = cf(dir.path(), &["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T00:00:00Z"]);
    assert!(!out.status.success() && stderr(&out).contains("CycleControlSourceUnresolved"), "{}", stderr(&out));

    ok(&cf(plane.path(), &["pipeline", "import", "--project", "research"]));
    let plan = json(&cf(dir.path(), &["pipeline", "plan", "--json", "--project", "research"]));
    assert_eq!((plan["applied"].clone(), plan["pipelines"][0]["action"].clone()), (serde_json::json!(1), serde_json::json!("change")));
    let answer = serve_cycle(dir.path(), "2030-01-01T00:00:00Z");
    assert_eq!(answer["fired"], serde_json::json!(["orders"]), "{answer}");
    assert_eq!(vendor.targets(), ["/v1/orders"], "orders fires as the plane applied it");
    assert!(control.targets().iter().all(|t| t.starts_with("/control/manifest@")), "{:?}", control.targets());
    // An apply claims through the plane at the URL; no local writer substitutes for it.
    let out = cf(dir.path(), &["pipeline", "apply", "--project", "research"]);
    assert!(!out.status.success() && stderr(&out).contains("ConfigOwnerUnconfigured"), "{}", stderr(&out));
}

/// The GitHub recipe, its host swapped for `vendor` and its credential header dropped.
fn github_recipe(vendor: &Vendor) -> tempfile::TempDir {
    let recipe = include_str!("../../../../recipes/github.toml").replace("https://api.github.com", &vendor.url(""));
    project(&recipe.lines().filter(|l| !l.starts_with("Authorization")).collect::<Vec<_>>().join("\n"))
}

/// Two recorded pages as one response body.
fn recorded(pages: [&str; 2]) -> String {
    let rows: Vec<serde_json::Value> = pages.iter().flat_map(|p| serde_json::from_str::<Vec<serde_json::Value>>(p).unwrap()).collect();
    serde_json::to_string(&rows).unwrap()
}

/// `recipes/github.toml` keys issues on `id` and commits on `sha`, so the boundary row each poll re-serves lands as
/// one row.
// spec: connector.source.github-recipe-keys@f99fe817
#[test]
fn the_github_recipe_lands_one_row_per_key_across_polls() {
    let issues = recorded([
        include_str!("../../../contextful-connectors/tests/fixtures/github/issues-page-1.json"),
        include_str!("../../../contextful-connectors/tests/fixtures/github/issues-page-2.json"),
    ]);
    let commits = recorded([
        include_str!("../../../contextful-connectors/tests/fixtures/github/commits-page-1.json"),
        include_str!("../../../contextful-connectors/tests/fixtures/github/commits-page-2.json"),
    ]);
    let vendor = Vendor::start(move |t| match t.split('?').next().unwrap_or_default() {
        "/repos/octocat/Hello-World/issues" => (200, issues.clone()),
        "/repos/octocat/Hello-World/commits" => (200, commits.clone()),
        _ => (404, "{}".into()),
    });
    let dir = github_recipe(&vendor);
    for (i, id) in ["github_issues", "github_commits"].iter().enumerate() {
        for n in 0..3 {
            ok(&fire(dir.path(), id, &format!("f{i}{n}"), &format!("2030-01-01T00:0{i}:{n}0Z")));
        }
    }
    let counts = |sql: &str| -> serde_json::Value {
        let out: serde_json::Value = serde_json::from_str(&ok(&cf(dir.path(), &["query", "--json", "--project", "research", sql]))).unwrap();
        out["rows"].clone()
    };
    let per_sha = counts("SELECT substr(sha, 1, 7), count(*) FROM \"github_commits_octocat_hello_world\" GROUP BY sha ORDER BY sha");
    assert_eq!(per_sha, serde_json::json!([["553c207", "1"], ["7629413", "1"], ["7fd1a60", "1"]]), "one row per sha");
    let per_issue = counts("SELECT number, count(*) FROM \"github_issues_octocat_hello_world\" GROUP BY number ORDER BY number");
    assert_eq!(per_issue, serde_json::json!([["7", "1"], ["12", "1"]]), "one row per issue");
}

/// Send `SIG<name>` to process `pid`.
fn signal(pid: u32, name: &str) {
    let sent = Command::new("kill").args([&format!("-{name}"), &pid.to_string()]).status().unwrap();
    assert!(sent.success(), "kill -{name} {pid}");
}

/// Whether process `pid` exists, by `kill -0`.
fn alive(pid: u32) -> bool {
    Command::new("kill").args(["-0", &pid.to_string()]).stderr(std::process::Stdio::null()).status().unwrap().success()
}

/// The pids whose parent is `parent`, by the process table.
fn children_of(parent: u32) -> Vec<u32> {
    let out = Command::new("ps").args(["-A", "-o", "pid=", "-o", "ppid="]).output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace().map(|n| n.parse::<u32>().ok());
            match (f.next().flatten(), f.next().flatten()) {
                (Some(pid), Some(ppid)) if ppid == parent => Some(pid),
                _ => None,
            }
        })
        .collect()
}

/// A loopback vendor that accepts every connection and never answers, so a fire against it blocks.
struct Silent {
    port: u16,
    accepted: Arc<Mutex<usize>>,
}

impl Silent {
    fn start() -> Silent {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted: Arc<Mutex<usize>> = Arc::default();
        let count = accepted.clone();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for stream in listener.incoming().flatten() {
                held.push(stream);
                *count.lock().unwrap() += 1;
            }
        });
        Silent { port, accepted }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    /// The children of `parent` once one of them holds a connection open to this vendor; waits up to 60 s.
    fn blocked_children(&self, parent: u32) -> Vec<u32> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let children = children_of(parent);
            if *self.accepted.lock().unwrap() > 0 && !children.is_empty() {
                return children;
            }
            assert!(std::time::Instant::now() < deadline, "no blocked child of {parent}");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

/// Children a failing assertion leaves behind, killed as the test unwinds.
struct Strays(Vec<u32>);

impl Drop for Strays {
    fn drop(&mut self) {
        if std::thread::panicking() {
            for pid in self.0.iter().filter(|p| alive(**p)) {
                let _ = Command::new("kill").args(["-KILL", &pid.to_string()]).status();
            }
        }
    }
}

/// Wait up to 20 s, past the 10 s child grace, for `child` to exit.
fn exits_within_grace(child: &mut std::process::Child) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(std::time::Instant::now() < deadline, "serve outlived its stop signal by 20 s");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// A project whose one scheduled pipeline fires against `vendor`, applied.
fn blocking_project(vendor: &Silent) -> tempfile::TempDir {
    let dir = project(&format!("site_id = \"site-a\"\n\n{}", scheduled("orders", &vendor.url("/v1/orders"), "every 1h")));
    ok(&cf(dir.path(), &["pipeline", "import", "--project", "research"]));
    ok(&cf(dir.path(), &["pipeline", "apply", "--project", "research"]));
    dir
}

fn serve_ends_its_blocked_children_on(name: &str) {
    let vendor = Silent::start();
    let dir = blocking_project(&vendor);
    let mut daemon = Daemon::start(dir.path());
    daemon.wait_for("fire orders: started", 0);
    let children = Strays(vendor.blocked_children(daemon.pid()));
    signal(daemon.pid(), name);
    let status = exits_within_grace(&mut daemon.child);
    assert!(status.success(), "{}", daemon.lines().join("\n"));
    let survivors: Vec<u32> = children.0.iter().copied().filter(|p| alive(*p)).collect();
    assert!(survivors.is_empty(), "children {survivors:?} outlived serve:\n{}", daemon.lines().join("\n"));
    daemon.wait_for("fire orders: failed", 0);
    daemon.wait_for("stopped", 0);
}

/// A serve process starts each child run in its own process group; on `SIGTERM`, `SIGINT`, a `--cycle` exit or an
/// unwind it signals each live group `SIGTERM`, `SIGKILL`s the remainder after 10 s, and exits once every child has.
// spec: surface.dispatch.children-reaped@9af41d1b
#[test]
fn serve_ends_every_child_it_dispatched() {
    serve_ends_its_blocked_children_on("TERM");
    serve_ends_its_blocked_children_on("INT");
    a_signalled_cycle_returns_only_after_its_children_exit();
    an_external_wake_ends_its_blocked_child_on_stop();
}

fn an_external_wake_ends_its_blocked_child_on_stop() {
    let vendor = Silent::start();
    let dir = project(&format!(
        "site_id = \"site-a\"\n\n[control]\ntrigger = \"external\"\n\n{}",
        scheduled("orders", &vendor.url("/v1/orders"), "every 1h")
    ));
    ok(&cf(dir.path(), &["pipeline", "import", "--project", "research"]));
    ok(&cf(dir.path(), &["pipeline", "apply", "--project", "research"]));
    let (mut daemon, url) = external(dir.path());
    let request = std::thread::spawn(move || post(&url));
    let children = Strays(vendor.blocked_children(daemon.pid()));
    signal(daemon.pid(), "TERM");
    let status = exits_within_grace(&mut daemon.child);
    assert!(status.success(), "{}", daemon.lines().join("\n"));
    assert!(children.0.iter().all(|pid| !alive(*pid)), "a child outlived the HTTP serve");
    let _ = request.join();
}

fn a_signalled_cycle_returns_only_after_its_children_exit() {
    let vendor = Silent::start();
    let dir = blocking_project(&vendor);
    let mut cycle = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["pipeline", "serve", "--cycle", "--project", "research"])
        .current_dir(dir.path())
        .env_remove("CONTEXTFUL_NODE_ID")
        .env_remove("CONTEXTFUL_SECRETS_BACKEND")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let children = Strays(vendor.blocked_children(cycle.id()));
    signal(cycle.id(), "TERM");
    exits_within_grace(&mut cycle);
    let survivors: Vec<u32> = children.0.iter().copied().filter(|p| alive(*p)).collect();
    assert!(survivors.is_empty(), "children {survivors:?} outlived the cycle");
    let out = cycle.wait_with_output().unwrap();
    let answer: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", stderr(&out)));
    assert_eq!(answer["failed"], serde_json::json!(["orders"]), "{answer}");
}

/// `POST` to `url` on a loopback address, answering the status and the JSON body.
fn post(url: &str) -> (u16, serde_json::Value) {
    let rest = url.strip_prefix("http://").unwrap();
    let (addr, path) = rest.split_once('/').unwrap();
    let mut stream = std::net::TcpStream::connect(addr).unwrap();
    write!(stream, "POST /{path} HTTP/1.1\r\nHost: {addr}\r\nContent-Length: 0\r\n\r\n").unwrap();
    let mut text = String::new();
    std::io::Read::read_to_string(&mut stream, &mut text).unwrap();
    let status = text.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = text.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or_default();
    (status, serde_json::from_str(body).unwrap_or(serde_json::Value::Null))
}

/// A daemon under the external trigger, and the wake URL it printed.
fn external(dir: &Path) -> (Daemon, String) {
    let daemon = Daemon::start_with(dir, &["--http", "127.0.0.1:0"]);
    let at = daemon.wait_for("wake on ", 0);
    let url = daemon.lines()[at].split("wake on ").nth(1).unwrap().trim().to_string();
    (daemon, url)
}

/// An unrecognized trigger value raises `TriggerAdapterUnknown` at startup and downgrades onto no adapter.
// spec: surface.arm.unknown-trigger@2601faf6
#[test]
fn an_unknown_trigger_arms_nothing() {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"a\"}]".into()));
    let dir = project(&format!("site_id = \"site-a\"\n\n[control]\ntrigger = \"externl\"\n\n{}", scheduled("orders", &vendor.url("/v1/orders"), "every 1h")));
    for args in [&["pipeline", "serve", "--project", "research"][..], &["pipeline", "serve", "--project", "research", "--http", "127.0.0.1:0"][..]] {
        let out = cf(dir.path(), args);
        let err = stderr(&out);
        assert!(!out.status.success() && err.contains("TriggerAdapterUnknown") && err.contains("externl"), "{err}");
        assert!(!err.contains("armed v") && !err.contains("wake on"), "{err}");
    }
    assert!(vendor.targets().is_empty(), "no adapter fired anything");
}

/// `external` selected on a deployment serving no HTTP face raises `TriggerFaceMissing` at startup.
// spec: surface.arm.trigger-face-missing@d3a5252e
#[test]
fn the_external_trigger_without_an_http_face_refuses_to_start() {
    let dir = project(&format!("site_id = \"site-a\"\n\n[control]\ntrigger = \"external\"\n\n{}", scheduled("orders", "https://api.vendor.example/v1", "every 1h")));
    ok(&cf(dir.path(), &["pipeline", "import", "--project", "research"]));
    let out = cf(dir.path(), &["pipeline", "serve", "--project", "research"]);
    let err = stderr(&out);
    assert!(!out.status.success() && err.contains("TriggerFaceMissing") && err.contains("--http"), "{err}");
}

/// `[control] trigger` selects `in-process`, the default, or `external`, under which `pipeline serve --http` answers
/// `POST /wake` and runs no tick of its own.
// spec: surface.arm.trigger-select@31728b75
#[test]
fn the_external_trigger_answers_wakes_and_runs_no_tick() {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"a\"}]".into()));
    let dir = project(&format!("site_id = \"site-a\"\n\n[control]\ntrigger = \"external\"\n\n{}", scheduled("orders", &vendor.url("/v1/orders"), "every 2s")));
    ok(&cf(dir.path(), &["pipeline", "import", "--project", "research"]));
    let (daemon, url) = external(dir.path());
    assert!(url.ends_with("/wake"), "{url}");
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert!(vendor.targets().is_empty(), "no tick fired a due entry between wakes");
    let (status, answer) = post(&url);
    assert_eq!((status, answer["fired"].clone()), (200, serde_json::json!(["orders"])), "{answer}");
    let (status, _) = post(&url.replace("/wake", "/elsewhere"));
    assert_eq!(status, 404);
    daemon.wait_for("armed v1: 1 scheduled pipeline(s)", 0);
    // In-process is the default, and it serves no wake route.
    let plain = project(&format!("site_id = \"site-a\"\n\n{}", scheduled("orders", "https://api.vendor.example/v1", "every 1h")));
    let out = cf(plain.path(), &["pipeline", "serve", "--project", "research", "--http", "127.0.0.1:0"]);
    assert!(!out.status.success() && stderr(&out).contains("trigger = \"external\""), "{}", stderr(&out));
}

/// A wake answers within 25 s with what fired, what failed, what stays armed and the next due instant, naming
/// each fire still in flight as pending.
// spec: surface.arm.wake-answer@324d39c8
#[test]
fn a_wake_answers_within_its_bound_naming_what_still_runs() {
    let fast = Vendor::start(|t| if t.starts_with("/v1/bad") { (404, "{}".into()) } else { (200, "[{\"id\":\"a\"}]".into()) });
    let slow = Vendor::start(|_| {
        std::thread::sleep(std::time::Duration::from_secs(35));
        (200, "[{\"id\":\"a\"}]".into())
    });
    let dir = project(&format!(
        "site_id = \"site-a\"\n\n[control]\ntrigger = \"external\"\n\n{}\n{}\n{}",
        scheduled("orders", &fast.url("/v1/orders"), "every 1h"),
        scheduled("bad", &fast.url("/v1/bad"), "every 1h"),
        scheduled("backfill", &slow.url("/v1/backfill"), "every 1h"),
    ));
    ok(&cf(dir.path(), &["pipeline", "import", "--project", "research"]));
    let (_daemon, url) = external(dir.path());
    let started = std::time::Instant::now();
    let (status, answer) = post(&url);
    let took = started.elapsed();
    assert_eq!(status, 200, "{answer}");
    assert!(took >= std::time::Duration::from_secs(24) && took < std::time::Duration::from_secs(30), "{took:?}");
    assert_eq!(answer["fired"], serde_json::json!(["orders"]), "{answer}");
    assert_eq!(answer["failed"], serde_json::json!(["bad"]), "{answer}");
    assert_eq!(answer["pending"], serde_json::json!(["backfill"]), "{answer}");
    assert_eq!(answer["armed"], 3, "{answer}");
    assert!(answer["next_due"].as_str().is_some_and(|t| t.ends_with('Z')), "{answer}");
}

/// An unreadable pointer, an unparseable snapshot or a control plane answering `5xx` raises
/// `ControlSnapshotUnreadable`, logs a diagnostic, and leaves the armed set in place running.
// spec: surface.reconcile.fail-static@59ee4328
#[test]
fn an_unreadable_snapshot_leaves_the_armed_set_running() {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"a\"}]".into()));
    let dir = project(&format!(
        "site_id = \"site-a\"\n\n[control]\ntrigger = \"external\"\n\n{}\n{}",
        scheduled("orders", &vendor.url("/v1/orders"), "every 1s"),
        scheduled("filings", &vendor.url("/v1/filings"), "every 1s"),
    ));
    ok(&cf(dir.path(), &["pipeline", "import", "--project", "research"]));
    let (daemon, url) = external(dir.path());
    let (_, first) = post(&url);
    assert_eq!((first["armed"].clone(), first["fired"].clone()), (serde_json::json!(2), serde_json::json!(["filings", "orders"])), "{first}");
    // The pointer names a version whose document does not parse.
    let control = dir.path().join(CONTROL);
    std::fs::write(control.join("manifest@v2.toml"), "[[pipeline]\nid = ").unwrap();
    std::fs::write(control.join("manifest@current"), "2\n").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let (status, after) = post(&url);
    assert_eq!(status, 200, "{after}");
    assert_eq!(after["armed"], 2, "{after}");
    assert_eq!(after["fired"], serde_json::json!(["filings", "orders"]), "the armed set keeps firing: {after}");
    let diagnostic = after["diagnostics"][0].as_str().unwrap_or_default();
    assert!(diagnostic.contains("ControlSnapshotUnreadable") && diagnostic.contains("manifest@v2.toml"), "{after}");
    let logged = daemon.wait_for("ControlSnapshotUnreadable", 0);
    assert!(daemon.lines()[logged].contains("the armed set stays in place"), "{}", daemon.lines()[logged]);
}

/// A daemon learns of a new snapshot only by reading its control source, on each poll and on each wake; nothing
/// pushes a snapshot to it.
// spec: surface.reconcile.learns-by-reading@3ccae6ce
#[test]
fn a_wake_reads_the_applied_version_from_the_control_source() {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"a\"}]".into()));
    let dir = project(&format!(
        "site_id = \"site-a\"\n\n[control]\ntrigger = \"external\"\npoll = \"every 1d\"\n\n{}",
        scheduled("orders", &vendor.url("/v1/orders"), "every 1s")
    ));
    ok(&cf(dir.path(), &["pipeline", "import", "--project", "research"]));
    let (daemon, url) = external(dir.path());
    post(&url);
    let path = dir.path().join("contextful.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace(&vendor.url("/v1/orders"), &vendor.url("/v2/orders"))).unwrap();
    assert!(ok(&cf(dir.path(), &["pipeline", "apply", "--project", "research"])).contains("applied v2"));
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let (_, answer) = post(&url);
    assert_eq!(answer["fired"], serde_json::json!(["orders"]), "{answer}");
    daemon.wait_for("armed v2", 0);
    assert_eq!(vendor.targets(), ["/v1/orders", "/v2/orders"], "the wake read v2 though the poll is a day away");
}

/// `contextful pipeline import` claims v1 from the declared pipelines while the snapshot directory holds no
/// version; a second import claims nothing.
// spec: surface.apply.guarded-import@a21a37ba
#[test]
fn the_import_claims_the_first_version_once() {
    let dir = project(&format!("site_id = \"site-a\"\n\n{}", scheduled("orders", "https://api.vendor.example/v1", "every 1h")));
    let out = ok(&cf(dir.path(), &["pipeline", "import", "--project", "research"]));
    assert!(out.contains("+ orders") && out.contains("imported v1"), "{out}");
    let again = cf(dir.path(), &["pipeline", "import", "--project", "research"]);
    assert!(!again.status.success() && stderr(&again).contains("v1"), "{}", stderr(&again));
    assert!(!dir.path().join(CONTROL).join("manifest@v2.toml").exists());
    // An empty declaration imports an empty document, and the directory counts as imported.
    let empty = project("site_id = \"site-a\"\n");
    assert!(ok(&cf(empty.path(), &["pipeline", "import", "--project", "research"])).contains("imported v1"));
    assert!(ok(&cf(empty.path(), &["pipeline", "apply", "--project", "research"])).contains("unchanged at v1"));
}

/// An edit or apply against a store that has taken no explicit guarded import, an empty store included, raises
/// `StoreNotInitialized`, answered `409`.
// spec: surface.apply.uninitialized-store@d6d5c26a
#[test]
fn an_apply_before_the_import_is_refused() {
    use contextful_core::surface::SurfaceError;
    let dir = project(&format!("site_id = \"site-a\"\n\n{}", scheduled("orders", "https://api.vendor.example/v1", "every 1h")));
    for args in [&["pipeline", "apply", "--project", "research"][..], &["pipeline", "apply", "orders", "--project", "research"][..]] {
        let out = cf(dir.path(), args);
        let err = stderr(&out);
        assert!(!out.status.success() && err.contains("StoreNotInitialized") && err.contains("pipeline import"), "{err}");
    }
    assert!(!dir.path().join(CONTROL).exists(), "no version claimed");
    // An empty directory is no import.
    std::fs::create_dir_all(dir.path().join(CONTROL)).unwrap();
    assert!(stderr(&cf(dir.path(), &["pipeline", "apply", "--project", "research"])).contains("StoreNotInitialized"));
    assert_eq!(SurfaceError::StoreNotInitialized(String::new()).status(), 409);
}

/// An owner with no storage configured or no credential raises `ConfigOwnerUnconfigured`, answered `503`; no local
/// writer substitutes for the store-scoped API.
// spec: surface.apply.owner-unconfigured@8a921fea
#[test]
fn an_owner_with_nothing_behind_it_is_refused() {
    use contextful_core::surface::SurfaceError;
    let dir = project(&format!(
        "site_id = \"site-a\"\n\n[control]\nurl = \"http://127.0.0.1:9/control\"\n\n{}",
        scheduled("orders", "https://api.vendor.example/v1", "every 1h")
    ));
    for verb in ["import", "apply"] {
        let out = cf(dir.path(), &["pipeline", verb, "--project", "research"]);
        let err = stderr(&out);
        assert!(!out.status.success() && err.contains("ConfigOwnerUnconfigured") && err.contains("127.0.0.1:9"), "{verb}: {err}");
    }
    assert!(!dir.path().join(".contextful/control").exists(), "no local writer substituted");
    assert_eq!(SurfaceError::ConfigOwnerUnconfigured(String::new()).status(), 503);
}

/// A credential value typed into a configuration field raises `SecretMaterialInDocument`; the document holds
/// references and the backend holds material.
// spec: surface.edit.secret-in-document@b8b6f23b
#[test]
fn a_credential_in_the_document_is_refused() {
    let with = |value: &str| {
        format!(
            "site_id = \"site-a\"\n\n[[pipeline]]\nid = \"orders\"\ntables = [\"items\"]\n[pipeline.source]\nname = \"http\"\n\
             [pipeline.source.config]\nendpoint = \"https://api.vendor.example/v1\"\n[pipeline.source.config.headers]\nX-Api-Token = \"{value}\"\n"
        )
    };
    let dir = project(&with("tok-9f8e7d6c5b4a"));
    let out = cf(dir.path(), &["pipeline", "import", "--project", "research"]);
    let err = stderr(&out);
    assert!(!out.status.success() && err.contains("SecretMaterialInDocument") && err.contains("X-Api-Token"), "{err}");
    assert!(!err.contains("tok-9f8e7d6c5b4a"), "{err}");
    assert!(!dir.path().join(CONTROL).exists(), "no version claimed");
    let dir = project(&with("${secret://vendor-token}"));
    ok(&cf(dir.path(), &["pipeline", "import", "--project", "research"]));
}

/// An artifact uploaded through the operator surface raises `ConnectorUploadRefused`; the surface references
/// registered connectors by id and version.
// spec: surface.edit.connector-upload@e5761c5c
#[test]
fn an_artifact_in_the_document_is_refused() {
    let dir = project(
        "site_id = \"site-a\"\n\n[[pipeline]]\nid = \"metrics\"\ntables = [\"items\"]\n[pipeline.source]\nname = \"vendor-metrics\"\n\
         [pipeline.source.config]\nartifact = \"data:application/wasm;base64,AGFzbQEAAAA=\"\n",
    );
    let out = cf(dir.path(), &["pipeline", "import", "--project", "research"]);
    let err = stderr(&out);
    assert!(!out.status.success() && err.contains("ConnectorUploadRefused") && err.contains("registered connector"), "{err}");
    assert!(!dir.path().join(CONTROL).exists(), "no version claimed");
}

/// A configured resource resolving to a region the policy omits raises `EnforceRegionMismatch` at startup, and the
/// runtime serves nothing.
// spec: surface.reside.region-mismatch@a11f32b8
#[test]
fn a_resource_outside_the_residency_allow_set_serves_nothing() {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"a\"}]".into()));
    let dir = project(&format!(
        "site_id = \"site-a\"\n\n[residency]\nregions = [\"eu-west-1\"]\n\n{}\n[[pipeline]]\nid = \"objects\"\ntables = [\"objects\"]\n\
         [pipeline.source]\nname = \"s3\"\nconfig = {{ bucket = \"vendor-drop\", region = \"us-east-1\" }}\n",
        scheduled("orders", &vendor.url("/v1/orders"), "every 1h"),
    ));
    std::fs::write(
        dir.path().join(".contextful/context/research/config.toml"),
        "[node]\nid = \"ingest-a\"\n\n[sync]\nendpoint = \"s3://eu-west-1\"\nbucket = \"team\"\nprefix = \"research\"\n",
    )
    .unwrap();
    for args in [&["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T00:00:00Z"][..], &["pipeline", "serve", "--project", "research"][..]] {
        let out = cf(dir.path(), args);
        let err = stderr(&out);
        assert!(!out.status.success() && err.contains("EnforceRegionMismatch") && err.contains("pipeline `objects` source resolves to `us-east-1`"), "{err}");
        assert!(!err.contains("bucket` resolves"), "the bucket resolves inside the set: {err}");
    }
    assert!(vendor.targets().is_empty(), "nothing served");
}

const SPANS: &str = r#"[
  {"id": "s1", "resource": {"service": "api", "pod": {"name": "p-1"}}, "events": [{"name": "start", "attrs": {"k": "v"}}, {"name": "end", "attrs": {}}], "bounds": [1, 2.5], "payload": "plain"},
  {"id": "s2", "resource": {"service": "db"}, "events": [], "bounds": null, "payload": {"k": 1}}
]"#;

/// The SQL types and values of the spans table's nested columns.
fn nested_read(dir: &Path, table: &str) -> serde_json::Value {
    let sql = format!(
        "SELECT typeof(resource) AS r, typeof(events) AS e, typeof(bounds) AS b, typeof(payload) AS p, resource, events, bounds FROM \"{table}\" ORDER BY id"
    );
    serde_json::from_str(&ok(&cf(
        dir,
        &["query", "--json", "--project", "research", &sql],
    )))
    .unwrap()
}

/// On the store sink, `native` lands each undeclared column of objects and arrays as a struct or list column inferred over the batch; a column mixing kinds, or one `schema.json` holds as a scalar, lands as `Json`.
// spec: run.normalize.native-store@6324658d
#[test]
fn a_native_pipeline_lands_nested_json_as_one_table_of_nested_columns() {
    let vendor = Vendor::start(|_| (200, SPANS.to_string()));
    let dir = project(&pipeline(
        "otel",
        &vendor.url("/v1/{table}"),
        "",
        "tables = [\"spans\"]",
    ));
    ok(&fire(dir.path(), "otel", "r1", "2030-01-01T00:00:00Z"));
    // One table holds the nesting: no child table lands beside it.
    let tables: Vec<String> =
        std::fs::read_dir(dir.path().join(".contextful/context/research/tables"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .filter(|n| !n.starts_with('_') && !n.starts_with('.'))
            .collect();
    assert_eq!(tables, ["otel_spans"]);
    let read = nested_read(dir.path(), "otel_spans");
    let row = &read["rows"][0];
    // A struct's fields follow the batch's key order.
    assert_eq!(
        row[0], "STRUCT(pod STRUCT(\"name\" VARCHAR), service VARCHAR)",
        "{read}"
    );
    assert_eq!(
        row[1], "STRUCT(attrs STRUCT(k VARCHAR), \"name\" VARCHAR)[]",
        "{read}"
    );
    assert_eq!(row[2], "DOUBLE[]", "{read}");
    // A column holding text beside an object keeps its JSON text, as an undeclared column always has.
    assert_eq!(row[3], "VARCHAR", "{read}");
    assert_eq!(
        row[4],
        serde_json::json!({"service": "api", "pod": {"name": "p-1"}})
    );
    assert_eq!(
        row[5],
        serde_json::json!([{"name": "start", "attrs": {"k": "v"}}, {"name": "end", "attrs": {"k": null}}])
    );
    assert_eq!(row[6], serde_json::json!([1.0, 2.5]));
    assert_eq!(
        read["rows"][1][4],
        serde_json::json!({"service": "db", "pod": null})
    );

}

/// Relational normalization flattens objects and preserves list order in child tables.
#[test]
fn a_relational_pipeline_shreds_lists_into_indexed_child_rows() {
    let vendor = Vendor::start(|_| (200, SPANS.to_string()));
    let dir = project(&pipeline(
        "otel",
        &vendor.url("/v1/{table}"),
        "normalize = \"relational\"",
        "tables = [\"spans\"]",
    ));
    ok(&fire(dir.path(), "otel", "r1", "2030-01-01T00:00:00Z"));
    let parent: serde_json::Value = serde_json::from_str(&ok(&cf(dir.path(), &[
        "query", "--json", "--project", "research",
        "SELECT id, resource_service, resource_pod_name, _row_id FROM otel_spans ORDER BY id",
    ]))).unwrap();
    assert_eq!(&parent["rows"][0].as_array().unwrap()[..3], serde_json::json!(["s1", "api", "p-1"]).as_array().unwrap());
    assert_eq!(&parent["rows"][1].as_array().unwrap()[..3], serde_json::json!(["s2", "db", null]).as_array().unwrap());
    let parent_id = parent["rows"][0][3].as_str().unwrap();
    let child: serde_json::Value = serde_json::from_str(&ok(&cf(dir.path(), &[
        "query", "--json", "--project", "research",
        "SELECT _parent_id, _list_index, name, attrs_k FROM otel_spans_events ORDER BY _list_index",
    ]))).unwrap();
    assert_eq!(child["rows"].as_array().unwrap().len(), 2);
    assert_eq!(child["rows"][0], serde_json::json!([parent_id, 0, "start", "v"]));
    assert_eq!(child["rows"][1], serde_json::json!([parent_id, 1, "end", null]));
}

/// Recursion stops at the declared `depth`, default 5 levels, landing a deeper subtree as one `Json` value.
// spec: run.normalize.nesting-depth@3392c7de
#[test]
fn native_nesting_stops_at_the_declared_depth() {
    let vendor = Vendor::start(|_| (200, SPANS.to_string()));
    let dir = project(&pipeline(
        "otel",
        &vendor.url("/v1/{table}"),
        "normalize = { mode = \"native\", depth = 1 }",
        "tables = [\"spans\"]",
    ));
    ok(&fire(dir.path(), "otel", "r1", "2030-01-01T00:00:00Z"));
    let read = nested_read(dir.path(), "otel_spans");
    let row = &read["rows"][0];
    assert_eq!(row[0], "STRUCT(pod VARCHAR, service VARCHAR)", "{read}");
    assert_eq!(row[1], "VARCHAR[]", "{read}");
    assert_eq!(
        row[5][0],
        serde_json::json!("{\"attrs\":{\"k\":\"v\"},\"name\":\"start\"}")
    );
    assert_eq!(
        row[4],
        serde_json::json!({"service": "api", "pod": "{\"name\":\"p-1\"}"})
    );
}

/// A mode outside the two raises `PipelineNormalizeModeUnknown`, printing both spellings.
// spec: run.normalize.mode-unknown@d2033711
#[test]
fn an_unknown_normalize_mode_is_refused_at_validation() {
    let dir = project(&pipeline(
        "otel",
        "https://vendor.example/v1/{table}",
        "normalize = \"flat\"",
        "tables = [\"spans\"]",
    ));
    let out = cf(dir.path(), &["pipeline", "validate"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("PipelineNormalizeModeUnknown")
            && err.contains("`flat`")
            && err.contains("`native`")
            && err.contains("`relational`"),
        "{err}"
    );
}
