//! `contextful job` through an embedding binary registering the `score` row body: job
//! blocks against the kind union, and a store-driven fire reading its input through the
//! read face under the job's credential.

use contextful_core::run::drive::row_label;
use contextful_core::run::journal::EntryKey;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const AUD: &str = "contextful://acme-research";

fn cf(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful")).args(args).current_dir(dir).env_remove("CONTEXTFUL_NODE_ID").output().unwrap()
}

/// The embedding binary under `examples/store_driven.rs`, built on first request.
fn host_binary() -> PathBuf {
    static BUILT: std::sync::Once = std::sync::Once::new();
    BUILT.call_once(|| {
        let status = Command::new(env!("CARGO")).args(["build", "-q", "-p", "contextful-cli", "--example", "store_driven"]).status().unwrap();
        assert!(status.success(), "building the store_driven example");
    });
    Path::new(env!("CARGO_BIN_EXE_contextful")).parent().unwrap().join("examples").join("store_driven")
}

fn host(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(host_binary()).args(args).envs(env.iter().copied()).current_dir(dir).env_remove("CONTEXTFUL_NODE_ID").output().unwrap()
}

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn err(out: &Output) -> String {
    assert!(!out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn job(extra: &str) -> String {
    format!(
        "[[job]]\nname = \"score-documents\"\nkind = \"store-driven\"\nbody = \"score\"\n\
         statement = \"SELECT doc_id, body FROM documents ORDER BY doc_id\"\n\
         as_of = \"2030-01-01T00:00:10Z\"\ntables = [\"scores\"]\n{extra}"
    )
}

/// A project with three documents landed at 00:00:00 and a fourth at 00:00:20, an issuer
/// key, and a credential reading `documents`.
fn project(manifest: &str) -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let store = p.join(".contextful/context/research");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    std::fs::write(p.join(".contextful/issuance.toml"), format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n")).unwrap();
    std::fs::write(p.join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{}", manifest)).unwrap();
    std::fs::write(p.join("early.jsonl"), "{\"doc_id\":\"d1\",\"body\":\"alpha\"}\n{\"doc_id\":\"d2\",\"body\":\"be\"}\n{\"doc_id\":\"d3\",\"body\":\"gamma ray\"}\n").unwrap();
    std::fs::write(p.join("late.jsonl"), "{\"doc_id\":\"d4\",\"body\":\"late\"}\n").unwrap();
    for (rows, run, now) in [("early.jsonl", "load-1", "2030-01-01T00:00:00Z"), ("late.jsonl", "load-2", "2030-01-01T00:00:20Z")] {
        ok(&cf(p, &["context", "land", "documents", "--project", "research", "--rows", rows, "--run-id", run, "--site-id", "site", "--now", now]));
    }
    let public = ok(&cf(p, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let token = ok(&cf(
        p,
        &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "svc://scoring", "--zone", "on-prem:hq", "--table", "documents", "--ttl", "600"],
    ));
    (dir, public, token)
}

fn fire(dir: &Path, public: &str, token: &str, run: &str, now: &str, env: &[(&str, &str)]) -> Output {
    let mut env = env.to_vec();
    env.push(("CONTEXTFUL_TOKEN", token));
    host(
        dir,
        &["job", "fire", "score-documents", "--project", "research", "--run-id", run, "--site-id", "site", "--now", now, "--public-key", public, "--audience", AUD],
        &env,
    )
}

/// The rows `sql` answers over the project, each as its cells' text.
fn select(dir: &Path, sql: &str) -> Vec<Vec<String>> {
    let v: serde_json::Value = serde_json::from_str(&ok(&cf(dir, &["query", "--json", "--project", "research", sql]))).unwrap();
    v["rows"].as_array().unwrap().iter().map(|r| r.as_array().unwrap().iter().map(|c| c.as_str().map(str::to_string).unwrap_or_else(|| c.to_string())).collect()).collect()
}

#[test]
fn job_blocks_validate_against_the_union_the_concurrency_rule_and_the_registered_bodies() {
    let (dir, _, _) = project(&job("max_in_flight = 4\n"));
    let p = dir.path();
    let valid = ok(&host(p, &["job", "validate"], &[]));
    assert!(valid.contains("score-documents: valid (store-driven, body score, max_in_flight 4"), "{valid}");

    // The stock binary registers no row body.
    let stock = err(&cf(p, &["job", "validate"]));
    assert!(stock.contains("JobBodyUnregistered") && stock.contains("`score`"), "{stock}");

    std::fs::write(p.join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{}", job(""))).unwrap();
    let unset = err(&host(p, &["job", "validate"], &[]));
    assert!(unset.contains("JobConcurrencyUnset"), "{unset}");

    std::fs::write(p.join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{}", "[[job]]\nname = \"clean\"\nkind = \"shell\"\ncommand = [\"rm\", \"-rf\", \"/\"]\n")).unwrap();
    let unknown = err(&host(p, &["job", "validate"], &[]));
    assert!(unknown.contains("JobKindUnknown"), "{unknown}");
}

/// A store-driven run reads its statement once, through the read face under the job's grant at its `as_of`, and
/// records the resolved snapshot id per table and the ordered input rows as its first step.
// spec: run.journal.store-input@819e3960
#[test]
fn a_fire_reads_its_input_at_the_pinned_as_of_and_lands_its_output_table() {
    let (dir, public, token) = project(&job("max_in_flight = 2\n"));
    let p = dir.path();
    let out = ok(&fire(p, &public, &token, "fire-1", "2030-01-01T00:01:00Z", &[]));
    assert!(out.contains("score-documents: fire-1 success · 3 rows landed from 3 input rows"), "{out}");
    // `d4` landed after the pinned `as_of` and never enters the input set.
    assert_eq!(
        select(p, "SELECT doc_id, score FROM scores ORDER BY doc_id"),
        vec![vec!["d1".to_string(), "5".to_string()], vec!["d2".to_string(), "2".to_string()], vec!["d3".to_string(), "9".to_string()]]
    );
    let row: serde_json::Value = serde_json::from_str(&ok(&cf(p, &["run", "show", "fire-1", "--project", "research"]))).unwrap();
    assert_eq!(row["input"]["as_of"], serde_json::json!("2030-01-01T00:00:10Z"));
    assert_eq!(row["input"]["rows"], serde_json::json!(3));
    assert_eq!(row["input"]["snapshots"].as_object().unwrap().keys().collect::<Vec<_>>(), ["documents"]);
    assert_eq!(row["host_scope"], serde_json::json!("job:score-documents"));

    // A credential reading no `documents` meets the read guard at the input step.
    let other = ok(&cf(p, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "svc://other", "--zone", "on-prem:hq", "--table", "notes", "--ttl", "600"]));
    let refused = err(&fire(p, &public, &other, "fire-2", "2030-01-01T00:02:00Z", &[]));
    assert!(refused.contains("fire-2 failed"), "{refused}");
}

/// The rows every body emits land after the last input row completes and before the owner retires, one run per
/// declared output table through {{run.land.stage-order}}.
// spec: run.journal.row-output@6b72f3ec
#[test]
fn each_declared_output_table_lands_in_its_own_run_and_a_failed_landing_holds_the_owner() {
    let declares = |tables: &str| job("max_in_flight = 2\n").replace("tables = [\"scores\"]", tables);
    let (dir, public, token) = project(&declares("tables = [\"scores\"]"));
    let p = dir.path();
    let ledger = p.join("ledger.txt");
    let ledger_env = ledger.to_str().unwrap();
    let env = [("SCORE_LEDGER", ledger_env), ("SCORE_AUDIT", "1")];
    let paid = || std::fs::read_to_string(&ledger).unwrap().lines().count();

    // The body emits `audits`, which the job does not declare: every row runs, then the landing fails.
    let refused = err(&fire(p, &public, &token, "fire-1", "2030-01-01T00:01:00Z", &env));
    assert!(refused.contains("fire-1 failed") && refused.contains("does not declare in `tables`"), "{refused}");
    assert_eq!(paid(), 3, "every row paid before the landing");

    // Declaring the table, the next fire resumes the held owner: no call pays again, and each
    // declared table lands in its own run, its id the fire's suffixed with the table.
    std::fs::write(p.join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{}", declares("tables = [\"scores\", \"audits\"]"))).unwrap();
    let out = ok(&fire(p, &public, &token, "fire-2", "2030-01-01T00:02:00Z", &env));
    assert!(out.contains("score-documents: fire-2 success · 6 rows landed from 3 input rows"), "{out}");
    assert_eq!(paid(), 3, "the resume replays every recorded call");
    let history: serde_json::Value = serde_json::from_str(&ok(&cf(p, &["run", "history", "--project", "research", "--pipeline", "score-documents"]))).unwrap();
    let mut runs: Vec<(String, String, String, u64)> = history["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["run_id"].as_str().unwrap().to_string(), r["table"].as_str().unwrap().to_string(), r["status"].as_str().unwrap().to_string(), r["rows"].as_u64().unwrap()))
        .collect();
    runs.sort();
    assert_eq!(
        runs,
        vec![("fire-2.audits".to_string(), "audits".to_string(), "success".to_string(), 3), ("fire-2.scores".to_string(), "scores".to_string(), "success".to_string(), 3)],
        "one run per declared output table, and none for the refused landing"
    );
    assert_eq!(select(p, "SELECT doc_id FROM audits ORDER BY doc_id"), vec![vec!["d1".to_string()], vec!["d2".to_string()], vec!["d3".to_string()]]);
    let resumed: serde_json::Value = serde_json::from_str(&ok(&cf(p, &["run", "show", "fire-2", "--project", "research"]))).unwrap();
    let first: serde_json::Value = serde_json::from_str(&ok(&cf(p, &["run", "show", "fire-1", "--project", "research"]))).unwrap();
    assert_eq!(resumed["execution_id"], first["execution_id"], "the resume keys on the held execution");

    // The landing retired the owner: the next fire opens a fresh execution and pays again.
    ok(&fire(p, &public, &token, "fire-3", "2030-01-01T00:03:00Z", &env));
    assert_eq!(paid(), 6);
    let fresh: serde_json::Value = serde_json::from_str(&ok(&cf(p, &["run", "show", "fire-3", "--project", "research"]))).unwrap();
    assert_ne!(fresh["execution_id"], first["execution_id"]);
}

#[test]
fn a_fire_killed_mid_input_resumes_paying_only_for_unrecorded_calls() {
    let (dir, public, token) = project(&job("max_in_flight = 1\n"));
    let p = dir.path();
    let ledger = p.join("ledger.txt");
    let ledger_env = ledger.to_str().unwrap();
    let died = fire(p, &public, &token, "fire-1", "2030-01-01T00:01:00Z", &[("SCORE_LEDGER", ledger_env), ("SCORE_DIE_AFTER", "2")]);
    assert_eq!(died.status.code(), Some(9), "{}", String::from_utf8_lossy(&died.stderr));
    let execution: serde_json::Value = serde_json::from_str(&ok(&cf(p, &["run", "show", "fire-1", "--project", "research"]))).unwrap();
    let execution_id = execution["execution_id"].as_str().unwrap().to_string();

    // Past the dead attempt's owner lease, the next fire resumes its execution.
    let out = ok(&fire(p, &public, &token, "fire-2", "2030-01-01T00:02:00Z", &[("SCORE_LEDGER", ledger_env)]));
    assert!(out.contains("3 rows landed from 3 input rows"), "{out}");
    let served: Vec<(String, String)> =
        std::fs::read_to_string(&ledger).unwrap().lines().map(|l| l.split_once(' ').map(|(d, k)| (d.to_string(), k.to_string())).unwrap()).collect();
    assert_eq!(served.iter().map(|(d, _)| d.as_str()).collect::<Vec<_>>(), ["d1", "d2", "d3"], "each document is paid for once");
    for (ordinal, (doc, key)) in served.iter().enumerate() {
        let entry = EntryKey::new(&execution_id, &row_label(&ordinal.to_string(), "model"), doc.as_bytes());
        assert_eq!(key, &entry.idempotency_key(), "{doc}");
    }
    assert_eq!(select(p, "SELECT count(*) FROM scores"), vec![vec!["3".to_string()]]);
}

/// A `fold` target naming no produced table, or a `build` target naming no produced table declaring its `columns`,
/// raises `JobTargetUnbound` at validation.
// spec: surface.fire.target-unbound@1ad27792
#[test]
fn a_job_target_naming_nothing_produced_is_refused_at_validation() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let pipelines = "[[pipeline]]\nid = \"meta_ads\"\ntables = [\"insights\", { name = \"spend\", columns = { day = \"timestamp\" } }]\n\
                     [pipeline.source]\nname = \"http\"\nconfig = { endpoint = \"https://api.vendor.example/v1\" }\n\n";
    let with = |jobs: &str| std::fs::write(p.join("contextful.toml"), format!("{pipelines}{jobs}")).unwrap();
    with("[[job]]\nname = \"nightly-fold\"\nkind = \"fold\"\ntarget = \"meta_ads_insights\"\n[[job]]\nname = \"spend\"\nkind = \"build\"\ntarget = \"meta_ads_spend\"\n");
    let out = ok(&cf(p, &["job", "validate"]));
    assert!(out.contains("nightly-fold: valid (fold)") && out.contains("spend: valid (build)"), "{out}");
    for (kind, target) in [("fold", "meta_ads_clicks"), ("build", "meta_ads_insights")] {
        with(&format!("[[job]]\nname = \"j\"\nkind = \"{kind}\"\ntarget = \"{target}\"\n"));
        let e = err(&cf(p, &["job", "validate"]));
        assert!(e.contains("JobTargetUnbound") && e.contains(target), "{e}");
    }
}
