//! `contextful eval run` through the built binary: a case file lands its corpus into a
//! scratch store, reads every case through the ranked call under one admitted credential,
//! and holds the report to the floors and a baseline.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const AUD: &str = "contextful://eval";
const READER: &str = "agent://eval-reader";

/// The workspace root, where the native golden set lives.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run(dir: &Path, args: &[&str], token: Option<&str>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_contextful"));
    cmd.args(args).current_dir(dir).env_remove("CONTEXTFUL_TOKEN");
    if let Some(t) = token {
        cmd.env("CONTEXTFUL_TOKEN", t);
    }
    cmd.output().unwrap()
}

fn stdout(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// A working directory holding an issuer: its public key, and a mint for credentials.
struct Issuer {
    dir: tempfile::TempDir,
    public: String,
}

impl Issuer {
    fn new() -> Issuer {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".contextful")).unwrap();
        std::fs::write(dir.path().join(".contextful/issuance.toml"), format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n")).unwrap();
        let public = stdout(&run(dir.path(), &["token", "keygen", "--out", ".contextful/issuer.seed"], None));
        Issuer { dir, public }
    }

    /// A credential for `agent`, signing `zone`, reading every table of the native corpus.
    fn mint(&self, agent: &str, zone: &str) -> String {
        let mut args = vec!["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://ops@example.org"];
        args.extend(["--agent", agent, "--zone", zone, "--ttl", "600"]);
        for t in ["kb/*", "news/*", "archive/*", "crm/*", "hr/*", "lab/*"] {
            args.extend(["--table", t]);
        }
        stdout(&run(self.dir.path(), &args, None))
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// `eval run` over `goldens` at k = 5, the report written to `report.json` under the
    /// issuer's directory.
    fn eval(&self, goldens: &Path, token: &str, extra: &[&str]) -> (Output, Value) {
        let report = self.path().join("report.json");
        let _ = std::fs::remove_file(&report);
        let goldens = goldens.to_str().unwrap();
        let mut args = vec!["eval", "run", "--goldens", goldens, "--k", "5", "--public-key", &self.public, "--audience", AUD];
        args.extend(["--report", report.to_str().unwrap()]);
        args.extend(extra);
        let out = run(self.path(), &args, Some(token));
        let text = std::fs::read_to_string(&report).unwrap_or_else(|_| "null".into());
        (out, serde_json::from_str(&text).unwrap())
    }
}

/// Write a corpus and a case file under `dir`: `manifest` as the corpus's declaration,
/// each table's rows under `rows/`, and `cases` pointing at the corpus.
fn corpus(dir: &Path, manifest: &str, tables: &[(&str, Value)], cases: &[Value]) -> PathBuf {
    let c = dir.join("corpus");
    std::fs::create_dir_all(&c).unwrap();
    std::fs::write(c.join("contextful.toml"), manifest).unwrap();
    for (table, rows) in tables {
        let p = c.join("rows").join(format!("{table}.jsonl"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let lines: Vec<String> = rows.as_array().unwrap().iter().map(Value::to_string).collect();
        std::fs::write(p, lines.join("\n") + "\n").unwrap();
    }
    let goldens = dir.join("cases.jsonl");
    let lines: Vec<String> = cases.iter().map(|c| c.to_string()).collect();
    std::fs::write(&goldens, lines.join("\n") + "\n").unwrap();
    goldens
}

fn mean(report: &Value, pointer: &str) -> f64 {
    report.pointer(pointer).and_then(Value::as_f64).unwrap_or_else(|| panic!("{pointer} in {report}"))
}

/// A corpus of one labelled table, `lab/notes`, whose rows carry 3-dimensional embeddings.
const LAB: &str = r#"
[[pipeline.tables]]
name = "lab/notes"
primary_key = ["note_id"]

[pipeline.tables.policy.rows]
predicate = "owner = subject.agent"

[[pipeline.tables]]
name = "lab/plain"
primary_key = ["shelf", "item"]
"#;

fn lab_rows() -> Vec<(&'static str, Value)> {
    vec![
        (
            "lab/notes",
            json!([
                { "note_id": "n1", "title": "solar battery storage", "owner": READER, "embedding": [0.0, 0.0, 1.0] },
                { "note_id": "n2", "title": "wind turbine blades", "owner": READER, "embedding": [1.0, 0.0, 0.0] },
                { "note_id": "n3", "title": "hydro dam survey", "owner": READER, "embedding": [0.0, 1.0, 0.0] },
            ]),
        ),
        (
            "lab/plain",
            json!([
                { "shelf": "a", "item": 1, "title": "pressure valve manual" },
                { "shelf": "a", "item": 2, "title": "conveyor belt tension" },
                { "shelf": "b", "item": 1, "title": "forklift battery charging" },
            ]),
        ),
    ]
}

/// A corpus is a directory holding `contextful.toml`, its table declarations and policy labels, and one `rows/<table>.jsonl` per table.
// spec: assurance.evaluate.corpus-layout@e4dfc247
/// Each case calls {{read.retrieve.ranked-call}} three times at limit k: the lexical leg with the question alone, the vector leg with an embedding and an empty query, the hybrid leg with both.
// spec: assurance.evaluate.legs@e21bd68b
#[test]
fn each_leg_calls_the_ranked_read_with_the_options_a_caller_passes() {
    let issuer = Issuer::new();
    let goldens = corpus(
        issuer.path(),
        LAB,
        &lab_rows(),
        &[json!({ "id": "solar", "corpus": "corpus", "question": "solar battery storage", "prefix": "lab/notes",
                  "query_embedding": [1.0, 0.0, 0.0], "expected": { "artifacts": ["lab/notes#n1"] } })],
    );
    let (out, report) = issuer.eval(&goldens, &issuer.mint(READER, "on-prem:hq"), &[]);
    // The embedding points away from the truth, so two legs miss it: the report is written,
    // then the precision floor reds the run.
    assert!(!out.status.success());
    assert!(stderr(&out).contains("floor retrieval.vector.r_precision.mean"), "{}", stderr(&out));
    let legs = &report["cases"][0]["legs"];
    // The question alone: only the row whose text matches clears the relevance floor.
    assert_eq!(legs["lexical"], json!(["lab/notes#n1"]));
    // The embedding alone ranks by cosine and passes no query text.
    assert_eq!(legs["vector"][0], json!("lab/notes#n2"));
    assert_eq!(legs["vector"].as_array().unwrap().len(), 3);
    // Both: the fused score puts the cosine match first and keeps the lexical match.
    assert_eq!(legs["hybrid"], json!(["lab/notes#n2", "lab/notes#n1"]));
    assert_eq!(mean(&report, "/retrieval/lexical/reciprocal_rank/mean"), 1.0);
    assert_eq!(mean(&report, "/retrieval/hybrid/reciprocal_rank/mean"), 0.5);
}

/// Two cases over the lab corpus, each with one relevant row its question names.
fn two_cases() -> [Value; 2] {
    [
        json!({ "id": "valve", "corpus": "corpus", "question": "pressure valve", "prefix": "lab/plain", "expected": { "artifacts": ["lab/plain#a,1"] } }),
        json!({ "id": "forklift", "corpus": "corpus", "question": "forklift battery", "prefix": "lab/plain", "expected": { "artifacts": ["lab/plain#b,1"] } }),
    ]
}

#[test]
fn a_killed_run_resumes_from_its_checkpoint_and_reruns_only_the_in_flight_case() {
    let issuer = Issuer::new();
    let goldens = corpus(issuer.path(), LAB, &lab_rows(), &two_cases());
    let token = issuer.mint(READER, "on-prem:hq");
    let checkpoint = issuer.path().join("run.jsonl");
    let cp = checkpoint.to_str().unwrap();
    let (out, full) = issuer.eval(&goldens, &token, &["--checkpoint", cp]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = std::fs::read_to_string(&checkpoint).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "a header and one result per case: {text}");

    // The kill lands while the second case's line is half written. The first case's line is
    // doctored so a re-run of it would show: a resumed run must report it as recorded.
    let mut first: Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(first["id"], json!("valve"));
    first["legs"]["lexical"] = json!([{ "row": { "table": "lab/plain", "key": "a,2" }, "in_window": true }]);
    let torn = &lines[2][..lines[2].len() / 2];
    std::fs::write(&checkpoint, format!("{}\n{first}\n{torn}", lines[0])).unwrap();
    let (out, resumed) = issuer.eval(&goldens, &token, &["--checkpoint", cp]);
    assert!(stderr(&out).contains("resuming 1 finished case(s)"), "{}", stderr(&out));
    // The doctored miss scores as recorded and reds the lexical precision floor.
    assert!(!out.status.success());
    assert!(stderr(&out).contains("floor retrieval.lexical.r_precision.mean"), "{}", stderr(&out));
    assert_eq!(resumed["cases"][0]["legs"]["lexical"], json!(["lab/plain#a,2"]), "the finished case did not run again");
    assert_eq!(resumed["cases"][1]["legs"], full["cases"][1]["legs"], "the in-flight case ran again");
    assert_eq!(std::fs::read_to_string(&checkpoint).unwrap().lines().count(), 3);

    // A checkpoint written under another seed resumes nothing.
    let (out, report) = issuer.eval(&goldens, &token, &["--checkpoint", cp, "--seed", "7"]);
    assert!(stderr(&out).contains("was written under"), "{}", stderr(&out));
    assert_eq!(report, Value::Null);
}

/// The gate is a local command comparing the report against in-tree baselines and floors, reporting both verdicts, with or without a trace store reachable.
// spec: assurance.baseline.offline@cb4d0757
#[test]
fn both_verdicts_come_from_in_tree_files_with_no_trace_store() {
    let issuer = Issuer::new();
    let goldens = root().join("evals/cases/native.jsonl");
    let baseline = root().join("evals/baselines/native.json");
    let token = issuer.mint(READER, "on-prem:hq");
    let args = ["--baseline", baseline.to_str().unwrap()];
    let (out, report) = issuer.eval(&goldens, &token, &args);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("eval: floor verdict: held"), "{}", stderr(&out));
    assert!(stderr(&out).contains("eval: baseline verdict: held against"), "{}", stderr(&out));

    // A trace endpoint nobody answers changes neither verdict nor figure.
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_contextful"));
    let report_path = issuer.path().join("unreachable.json");
    cmd.args(["eval", "run", "--goldens", goldens.to_str().unwrap(), "--k", "5", "--public-key", &issuer.public, "--audience", AUD])
        .args(["--report", report_path.to_str().unwrap()])
        .args(args)
        .current_dir(issuer.path())
        .env("CONTEXTFUL_TOKEN", &token)
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:9")
        .env("HTTPS_PROXY", "http://127.0.0.1:9");
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let again: Value = serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
    assert_eq!(again["retrieval"], report["retrieval"]);
    assert!(stderr(&out).contains("eval: baseline verdict: held against"), "{}", stderr(&out));

    // A red floor and a held baseline report apart.
    let wrong = issuer.path().join("wrong.jsonl");
    let text = std::fs::read_to_string(&goldens).unwrap().replace("\"corpus\": \"../corpora/native\"", &format!("\"corpus\": {}", json!(root().join("evals/corpora/native"))));
    let text = text.replace("kb/articles#art-0", "kb/articles#art-2").replace("kb/articles#cat-0", "kb/articles#cat-2");
    let text = text.replace("kb/articles#log-0", "kb/articles#log-2").replace("kb/articles#car-0", "kb/articles#car-2");
    let text = text.replace("kb/articles#pen-0", "kb/articles#pen-2").replace("kb/articles#sea-0", "kb/articles#sea-2");
    let text = text.replace("kb/articles#ice-0", "kb/articles#ice-2").replace("kb/articles#ant-0", "kb/articles#ant-2");
    std::fs::write(&wrong, text.replace("kb/cjk#ja-", "kb/cjk#jx-").replace("news/wire#", "news/wirex#")).unwrap();
    let (out, _) = issuer.eval(&wrong, &token, &[]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("eval: floor verdict: red"), "{}", stderr(&out));
    assert!(stderr(&out).contains("eval: baseline verdict: none"), "{}", stderr(&out));
}

/// A loopback collector answering one POST with 200, handing back the body it received.
fn collector() -> (String, std::thread::JoinHandle<String>) {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}/v1/runs", listener.local_addr().unwrap().port());
    let handle = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = v.trim().parse().unwrap();
            }
            if line == "\r\n" || line.is_empty() {
                break;
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let mut stream = stream;
        stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").unwrap();
        String::from_utf8(body).unwrap()
    });
    (url, handle)
}

/// A trace store records run history and curation staging, and decides no verdict.
// spec: assurance.baseline.trace-store@b327df67
#[test]
fn the_trace_store_records_history_and_staging_and_decides_nothing() {
    let issuer = Issuer::new();
    let mut cases = two_cases().to_vec();
    cases.push(json!({ "id": "absent", "corpus": "corpus", "question": "pressure valve", "prefix": "lab/plain", "expected": { "must_abstain": true } }));
    let goldens = corpus(issuer.path(), LAB, &lab_rows(), &cases);
    let token = issuer.mint(READER, "on-prem:hq");
    let store = issuer.path().join("traces");
    let s = store.to_str().unwrap();
    let (out, plain) = issuer.eval(&goldens, &token, &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let (out, traced) = issuer.eval(&goldens, &token, &["--trace-store", s]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(traced["retrieval"], plain["retrieval"], "the store moves no figure");

    // A red run records its verdict as it stands.
    let mut wrong = cases.clone();
    wrong[0]["expected"]["artifacts"] = json!(["lab/plain#a,2"]);
    let red = corpus(issuer.path(), LAB, &lab_rows(), &wrong);
    let (out, _) = issuer.eval(&red, &token, &["--trace-store", s]);
    assert!(!out.status.success());
    let history: Vec<Value> = std::fs::read_to_string(store.join("runs.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0]["floors"], json!("held"));
    assert_eq!(history[0]["baseline"], Value::Null);
    assert_eq!(history[0]["n_cases"], json!(3));
    assert_eq!(history[1]["floors"], json!("red"));
    // The case set file now carries the red truth; restore the original for the runs below.
    let goldens = corpus(issuer.path(), LAB, &lab_rows(), &cases);
    // The must-abstain case returned rows, so a curator reviews it.
    let staging = std::fs::read_to_string(store.join("staging.jsonl")).unwrap();
    assert!(staging.lines().any(|l| l.contains("\"absent\"") && l.contains("must-abstain")), "{staging}");

    // A store that cannot record leaves the outcome as it was.
    let blocked = issuer.path().join("blocked");
    std::fs::write(&blocked, "a file, not a directory").unwrap();
    let (out, report) = issuer.eval(&goldens, &token, &["--trace-store", blocked.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("recorded nothing"), "{}", stderr(&out));
    assert_eq!(report["retrieval"], plain["retrieval"]);

    // A self-hosted collector receives the same record.
    let (url, received) = collector();
    let (out, _) = issuer.eval(&goldens, &token, &["--trace-endpoint", &url]);
    assert!(out.status.success(), "{}", stderr(&out));
    let body: Value = serde_json::from_str(&received.join().unwrap()).unwrap();
    assert_eq!(body["floors"], json!("held"));
    assert_eq!(body["seed"], plain["seed"]);
}

/// A run touching a deployed store with a hosted trace endpoint configured raises `TraceExportOutOfPerimeter`; hosted collection serves runs over public or synthetic fixtures alone.
// spec: assurance.baseline.trace-export@7c0d7ccd
#[test]
fn a_hosted_trace_endpoint_serves_fixtures_and_refuses_a_deployed_store() {
    let issuer = Issuer::new();
    let goldens = corpus(issuer.path(), LAB, &lab_rows(), &two_cases());
    let token = issuer.mint(READER, "on-prem:hq");
    // Fixtures alone: a hosted endpoint is admitted, and one nobody answers decides nothing.
    let hosted = "https://collector.example.invalid/v1/runs";
    let (out, report) = issuer.eval(&goldens, &token, &["--trace-endpoint", hosted]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("received nothing"), "{}", stderr(&out));
    assert!(report["retrieval"].is_object());

    // The same corpus holding a deployed project's store root.
    std::fs::create_dir_all(issuer.path().join("corpus/.contextful/context/ops")).unwrap();
    let (out, report) = issuer.eval(&goldens, &token, &["--trace-endpoint", hosted]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("TraceExportOutOfPerimeter"), "{}", stderr(&out));
    assert!(stderr(&out).contains(".contextful/context"), "{}", stderr(&out));
    assert_eq!(report, Value::Null, "the refusal lands no row and writes no report");

    // A collector inside the perimeter serves the deployed store's run.
    let (url, received) = collector();
    let (out, _) = issuer.eval(&goldens, &token, &["--trace-endpoint", &url]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(received.join().unwrap().contains("\"floors\""));
}

/// Every generated fixture and randomized schedule derives from the record's seed, and replaying that seed reproduces a count-valued entry's value.
// spec: assurance.measure.seeded@2f5ac2a3
#[test]
fn replaying_the_ledger_seed_reproduces_the_native_gate_figures() {
    let ledger: toml::Value = toml::from_str(&std::fs::read_to_string(root().join("evals/ledger.toml")).unwrap()).unwrap();
    let entry = &ledger["entry"]["eval-through-enforcement"];
    let seed = entry["seed"].as_integer().unwrap().to_string();
    let issuer = Issuer::new();
    let goldens = root().join("evals/cases/native.jsonl");
    let token = issuer.mint(READER, "on-prem:hq");
    let (out, first) = issuer.eval(&goldens, &token, &["--seed", &seed]);
    assert!(out.status.success(), "{}", stderr(&out));
    let (out, replay) = issuer.eval(&goldens, &token, &["--seed", &seed]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(first["seed"].as_i64().map(|s| s.to_string()), Some(seed));
    let metric = entry["metric"].as_str().unwrap().replace('.', "/");
    assert_eq!(first.pointer(&format!("/{metric}")), replay.pointer(&format!("/{metric}")));
    assert_eq!(first["retrieval"], replay["retrieval"], "every figure replays");
    assert_eq!(first["cases"], replay["cases"], "every ranking replays");
}

#[test]
fn each_case_reports_its_tokens_latency_and_corpus_bucket() {
    let issuer = Issuer::new();
    let goldens = corpus(issuer.path(), LAB, &lab_rows(), &two_cases());
    let token = issuer.mint(READER, "on-prem:hq");
    let (out, report) = issuer.eval(&goldens, &token, &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(report["latency_ms"]["n"], json!(2));
    assert!(mean(&report, "/systems/tokens_per_query/min") >= 4.0, "a question and the rows it was handed: {}", report["systems"]);
    assert_eq!(mean(&report, "/systems/cost/max"), 0.0);
    assert_eq!(report["systems"]["buckets"]["le_1x"]["n_cases"], json!(2));
    // A window smaller than the corpus moves both cases to a larger bucket.
    let (out, small) = issuer.eval(&goldens, &token, &["--context-window", "4"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let buckets = small["systems"]["buckets"].as_object().unwrap();
    assert_eq!(buckets.len(), 1);
    assert!(buckets.keys().all(|b| b != "le_1x"), "{buckets:?}");
}

/// The deterministic tier embeds every row and question with a seeded feature-hashing stub embedder; a corpus row or a case carrying its own embedding keeps it.
// spec: assurance.evaluate.stub-embedder@57f9091d
#[test]
fn rows_and_questions_without_an_embedding_take_the_seeded_stub() {
    let issuer = Issuer::new();
    let goldens = corpus(
        issuer.path(),
        LAB,
        &lab_rows(),
        &[json!({ "id": "forklift", "corpus": "corpus", "question": "forklift battery", "prefix": "lab/plain",
                  "expected": { "artifacts": ["lab/plain#b,1"] } })],
    );
    let token = issuer.mint(READER, "on-prem:hq");
    let (out, report) = issuer.eval(&goldens, &token, &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    // A composite key joins its values with commas.
    assert_eq!(report["cases"][0]["legs"]["vector"][0], json!("lab/plain#b,1"));
    assert_eq!(report["seed"], json!(0x5eed_0078u64));
    // Another seed embeds both sides alike, so the ranking holds and the report names it.
    let (again, other) = issuer.eval(&goldens, &token, &["--seed", "7"]);
    assert!(again.status.success(), "{}", stderr(&again));
    assert_eq!(other["seed"], json!(7));
    assert_eq!(other["cases"][0]["legs"]["vector"][0], json!("lab/plain#b,1"));
}

/// A run over a corpus whose tables declare no zone or row policy, with no `local:` zone passed as `--zone`, raises `EvalCorpusUnlabeled` before any row lands.
// spec: assurance.evaluate.unlabeled-corpus@3e99725b
#[test]
fn an_unlabeled_corpus_refuses_without_a_local_zone() {
    let issuer = Issuer::new();
    let manifest = "[[pipeline.tables]]\nname = \"lab/plain\"\nprimary_key = [\"shelf\", \"item\"]\n";
    let goldens = corpus(
        issuer.path(),
        manifest,
        &lab_rows()[1..],
        &[json!({ "id": "valve", "corpus": "corpus", "question": "pressure valve", "expected": { "artifacts": ["lab/plain#a,1"] } })],
    );
    // The refusal needs no credential: it lands nothing and admits nothing.
    let (out, report) = issuer.eval(&goldens, "unused", &[]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("EvalCorpusUnlabeled"), "{}", stderr(&out));
    assert_eq!(report, Value::Null, "a refused run writes no report");
    let (out, _) = issuer.eval(&goldens, "unused", &["--zone", "on-prem:hq"]);
    assert!(stderr(&out).contains("EvalCorpusUnlabeled"), "only a local zone admits an unlabeled corpus");

    let (out, report) = issuer.eval(&goldens, &issuer.mint(READER, "local:device"), &["--zone", "local:device"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(report["cases"][0]["legs"]["lexical"], json!(["lab/plain#a,1"]));
}

/// An evaluation corpus carries zone and row-policy labels in its table declarations, and a run reads it through the access-control path under one admitted credential.
// spec: assurance.evaluate.policy-labels@e45316b9
#[test]
fn a_row_the_credential_may_not_read_never_scores() {
    let issuer = Issuer::new();
    let goldens = root().join("evals/cases/native.jsonl");
    let (out, report) = issuer.eval(&goldens, &issuer.mint(READER, "on-prem:hq"), &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    for leg in ["lexical", "vector", "hybrid"] {
        assert_eq!(report["retrieval"][leg]["forbidden_row_rate"]["n"], json!(6), "{leg}");
        assert_eq!(mean(&report, &format!("/retrieval/{leg}/forbidden_row_rate/max")), 0.0, "{leg}");
    }
    // The same cases under the other team's credential: its row policy admits the rows the
    // reader's withholds, so the zero above is a fact of enforcement, not of ranking.
    let (out, other) = issuer.eval(&goldens, &issuer.mint("agent://other-team", "on-prem:hq"), &[]);
    assert!(!out.status.success(), "the other team's run reads the reader's must-not rows");
    assert!(mean(&other, "/retrieval/hybrid/forbidden_row_rate/max") > 0.0);
    assert!(stderr(&out).contains("retrieval.hybrid.forbidden_row_rate.max"), "{}", stderr(&out));
    // A zone the payroll table admits reads its rows; the reader's zone reads none of them.
    let (_, cloud) = issuer.eval(&goldens, &issuer.mint(READER, "public-cloud:eu"), &[]);
    let payroll = |r: &Value| r["cases"].as_array().unwrap().iter().flat_map(|c| c["legs"]["hybrid"].as_array().unwrap().clone()).filter(|row| row.as_str().unwrap().starts_with("hr/payroll#")).count();
    assert!(payroll(&cloud) > 0);
    assert_eq!(payroll(&report), 0);
}

/// A case over `lab/plain`, whose rows carry no publication column and so date by their
/// landing instant, asked at `anchor` with the day before it as its recency bound.
fn dated_case(id: &str, anchor: &str, since: &str) -> Value {
    json!({ "id": id, "corpus": "corpus", "question": "pressure valve", "prefix": "lab/plain",
            "expected": { "artifacts": ["lab/plain#a,1"], "time_anchor": anchor, "since": since } })
}

/// `contextful eval run --goldens <file>` loads the case file, lands and folds each referenced corpus into a scratch store at `--landed-at`, the Unix epoch by default, reads every case, and writes the run report.
// spec: assurance.evaluate.run-command@5f634816
#[test]
fn each_corpus_lands_at_the_epoch_unless_landed_at_names_an_instant() {
    let issuer = Issuer::new();
    let token = issuer.mint(READER, "on-prem:hq");
    let epoch = corpus(issuer.path(), LAB, &lab_rows(), &[dated_case("epoch", "1970-01-01T12:00:00Z", "1970-01-01T00:00:00Z")]);
    // Undated rows landed at the epoch sit inside a window on the epoch's first day, and a
    // landing at the wall clock would put them decades past it.
    let (out, report) = issuer.eval(&epoch, &token, &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(mean(&report, "/retrieval/hybrid/in_window_rate/min"), 1.0);
    assert_eq!(report["cases"][0]["legs"]["lexical"], json!(["lab/plain#a,1"]));
    let (again, replay) = issuer.eval(&epoch, &token, &[]);
    assert!(again.status.success(), "{}", stderr(&again));
    assert_eq!(replay["retrieval"], report["retrieval"]);

    // `--landed-at` moves every row: the epoch window loses them and the in-window floor reds.
    let (out, moved) = issuer.eval(&epoch, &token, &["--landed-at", "2030-01-01T00:00:00Z"]);
    assert!(!out.status.success());
    assert_eq!(mean(&moved, "/retrieval/hybrid/in_window_rate/min"), 0.0);
    assert!(stderr(&out).contains("floor retrieval.hybrid.in_window_rate.min"), "{}", stderr(&out));

    // A window on the named day holds only when the run lands there.
    let later = corpus(issuer.path(), LAB, &lab_rows(), &[dated_case("later", "2030-01-01T12:00:00Z", "2030-01-01T00:00:00Z")]);
    let (out, _) = issuer.eval(&later, &token, &["--landed-at", "2030-01-01T00:00:00Z"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let (out, _) = issuer.eval(&later, &token, &[]);
    assert!(!out.status.success(), "the default landing predates a 2030 window");

    let (out, report) = issuer.eval(&later, &token, &["--landed-at", "new year 2030"]);
    assert!(stderr(&out).contains("`--landed-at` is not an RFC 3339 instant"), "{}", stderr(&out));
    assert_eq!(report, Value::Null);
}

/// The native case file with every recency bound removed, its corpus path made absolute:
/// the ranked call reads no window, as a read path ignoring the bound does.
fn unbounded_native(dir: &Path) -> PathBuf {
    let corpus = root().join("evals/corpora/native");
    let lines: Vec<String> = std::fs::read_to_string(root().join("evals/cases/native.jsonl"))
        .unwrap()
        .lines()
        .map(|l| {
            let mut case: Value = serde_json::from_str(l).unwrap();
            case["corpus"] = json!(corpus);
            case["expected"].as_object_mut().unwrap().remove("since");
            case.to_string()
        })
        .collect();
    let path = dir.join("unbounded.jsonl");
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    path
}

#[test]
fn a_read_that_drops_the_recency_bound_goes_red_against_the_baseline() {
    let issuer = Issuer::new();
    let baseline = root().join("evals/baselines/native.json");
    let (out, report) = issuer.eval(&unbounded_native(issuer.path()), &issuer.mint(READER, "on-prem:hq"), &["--baseline", baseline.to_str().unwrap()]);
    // Each temporal topic's archived stories outscore its recent ones, so only the window
    // keeps the recent stories on top.
    assert!(mean(&report, "/slices/temporal/retrieval/hybrid/recall_at_k/mean") <= 0.5, "{}", report["slices"]["temporal"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("baseline retrieval.hybrid.recall_at_k"), "{}", stderr(&out));
}

#[test]
fn the_native_golden_set_holds_its_floors_and_baseline() {
    let issuer = Issuer::new();
    let goldens = root().join("evals/cases/native.jsonl");
    let baseline = root().join("evals/baselines/native.json");
    let token = issuer.mint(READER, "on-prem:hq");
    let (out, report) = issuer.eval(&goldens, &token, &["--baseline", baseline.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(report["n_cases"].as_u64().unwrap() >= 30);
    for tag in ["substring-trap", "cjk", "temporal", "deep-recall", "regression"] {
        assert!(report["slices"][tag]["n_cases"].as_u64().unwrap() > 0, "no `{tag}` case");
    }
    assert_eq!(report["tally"]["dropped"], json!(0), "every native case carries artifact truth");
    let forbidden = mean(&report, "/retrieval/hybrid/forbidden_row_rate/max");
    let precision = mean(&report, "/retrieval/hybrid/r_precision/mean");
    let n = report["retrieval"]["hybrid"]["r_precision"]["n"].as_u64().unwrap();
    let seed = report["seed"].as_u64().unwrap();
    contextful_eval::record::emit("eval-through-enforcement", forbidden, report["retrieval"]["hybrid"]["forbidden_row_rate"]["n"].as_u64().unwrap(), seed);
    contextful_eval::record::emit("native-golden-floor", precision, n, seed);
}

/// The corpus loads through the real store, and the retriever under test calls {{read.retrieve.ranked-call}} with the options a caller passes.
// spec: assurance.evaluate.through-the-store@aaa02c86
#[test]
fn a_ranking_that_loses_its_sidecars_goes_red_against_the_baseline() {
    let issuer = Issuer::new();
    // The native set with its archive table's sidecars undeclared: only the recency window
    // reads that table, and the oldest rows fall outside it.
    let copy = issuer.path().join("evals");
    for dir in ["cases", "baselines", "corpora/native/rows/archive", "corpora/native/rows/crm", "corpora/native/rows/hr", "corpora/native/rows/kb", "corpora/native/rows/news"] {
        std::fs::create_dir_all(copy.join(dir)).unwrap();
    }
    for f in [
        "cases/native.jsonl",
        "baselines/native.json",
        "corpora/native/rows/archive/incidents.jsonl",
        "corpora/native/rows/crm/accounts.jsonl",
        "corpora/native/rows/hr/payroll.jsonl",
        "corpora/native/rows/kb/articles.jsonl",
        "corpora/native/rows/kb/cjk.jsonl",
        "corpora/native/rows/news/wire.jsonl",
    ] {
        std::fs::copy(root().join("evals").join(f), copy.join(f)).unwrap();
    }
    let manifest = std::fs::read_to_string(root().join("evals/corpora/native/contextful.toml")).unwrap();
    let start = manifest.find("[[pipeline.tables.indexes]]").unwrap();
    let end = manifest.find("[[pipeline.tables]]\nname = \"crm/accounts\"").unwrap();
    std::fs::write(copy.join("corpora/native/contextful.toml"), format!("{}{}", &manifest[..start], &manifest[end..])).unwrap();

    let baseline = copy.join("baselines/native.json");
    let (out, report) = issuer.eval(&copy.join("cases/native.jsonl"), &issuer.mint(READER, "on-prem:hq"), &["--baseline", baseline.to_str().unwrap()]);
    assert_eq!(mean(&report, "/slices/deep-recall/retrieval/hybrid/recall_at_k/mean"), 0.0);
    assert!(!out.status.success(), "a recall slide under every floor still reds the baseline");
    assert!(stderr(&out).contains("baseline retrieval.hybrid.recall_at_k"), "{}", stderr(&out));
}

/// A run holds its report to the floors and, given `--baseline`, to that file; either red verdict exits non-zero, and `--update-baseline` applies {{assurance.baseline.raise-only}} on green alone.
// spec: assurance.evaluate.run-verdict@f4bc8e69
#[test]
fn a_red_run_exits_non_zero_and_only_a_green_one_raises_its_baseline() {
    let issuer = Issuer::new();
    let goldens = root().join("evals/cases/native.jsonl");
    let token = issuer.mint(READER, "on-prem:hq");
    let committed: Value = serde_json::from_str(&std::fs::read_to_string(root().join("evals/baselines/native.json")).unwrap()).unwrap();

    // A baseline below the run: green, and the update raises the entry to the measured value.
    let low = issuer.path().join("low.json");
    let mut file = committed.clone();
    file["retrieval.hybrid.ndcg_at_k"] = json!(0.5);
    std::fs::write(&low, file.to_string()).unwrap();
    let (out, report) = issuer.eval(&goldens, &token, &["--baseline", low.to_str().unwrap(), "--update-baseline"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let raised: Value = serde_json::from_str(&std::fs::read_to_string(&low).unwrap()).unwrap();
    assert_eq!(raised["retrieval.hybrid.ndcg_at_k"], report["retrieval"]["hybrid"]["ndcg_at_k"]["mean"]);

    // A baseline above the run: red, and the file stays as written.
    let high = issuer.path().join("high.json");
    let mut file = committed.clone();
    file["retrieval.vector.r_precision"] = json!(1.0);
    file["retrieval.hybrid.ndcg_at_k"] = json!(0.5);
    let written = file.to_string();
    std::fs::write(&high, &written).unwrap();
    let (out, _) = issuer.eval(&goldens, &token, &["--baseline", high.to_str().unwrap(), "--update-baseline"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("baseline retrieval.vector.r_precision"), "{}", stderr(&out));
    assert_eq!(std::fs::read_to_string(&high).unwrap(), written, "a red run raises nothing");

    // A baseline recorded at another k refuses before any corpus lands.
    let other_k = issuer.path().join("k10.json");
    let mut file = committed;
    file["_run"]["k"] = json!(10);
    std::fs::write(&other_k, file.to_string()).unwrap();
    let (out, report) = issuer.eval(&goldens, &token, &["--baseline", other_k.to_str().unwrap()]);
    assert!(stderr(&out).contains("BaselineRunStampMismatch"), "{}", stderr(&out));
    assert_eq!(report, Value::Null);

    // With no baseline the floors still gate: a case set whose truth the ranking misses reds.
    let wrong = issuer.path().join("wrong.jsonl");
    let text = std::fs::read_to_string(&goldens).unwrap().replace("\"corpus\": \"../corpora/native\"", &format!("\"corpus\": {}", json!(root().join("evals/corpora/native"))));
    let text = text.replace("kb/articles#art-0", "kb/articles#art-2").replace("kb/articles#cat-0", "kb/articles#cat-2");
    let text = text.replace("kb/articles#log-0", "kb/articles#log-2").replace("kb/articles#car-0", "kb/articles#car-2");
    let text = text.replace("kb/articles#pen-0", "kb/articles#pen-2").replace("kb/articles#sea-0", "kb/articles#sea-2");
    let text = text.replace("kb/articles#ice-0", "kb/articles#ice-2").replace("kb/articles#ant-0", "kb/articles#ant-2");
    let text = text.replace("kb/cjk#ja-", "kb/cjk#jx-").replace("news/wire#", "news/wirex#");
    std::fs::write(&wrong, text).unwrap();
    let (out, _) = issuer.eval(&wrong, &token, &[]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("floor retrieval.hybrid.r_precision.mean"), "{}", stderr(&out));
}
