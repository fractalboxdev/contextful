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
        let mut build = Command::new(env!("CARGO"));
        // Package metadata belongs to this integration executable, not the nested build.
        // Build scripts track these variables; inheriting them invalidates native artifacts.
        for (name, _) in std::env::vars_os() {
            if name.to_str().is_some_and(|name| name.starts_with("CARGO_PKG_") || name.starts_with("CARGO_CFG_") || matches!(name, "CARGO_MANIFEST_DIR" | "CARGO_MANIFEST_PATH" | "CARGO_MANIFEST_LINKS" | "OUT_DIR")) {
                build.env_remove(name);
            }
        }
        build.args(["build", "--locked", "-q", "-p", "contextful-cli", "--example", "store_driven"]);
        if cfg!(feature = "contextful-full") {
            build.args(["--no-default-features", "--features", "contextful-full"]);
        }
        let status = build.status().unwrap();
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

#[test]
fn canonical_output_removal_refuses_before_recorded_body_calls_but_unprotected_jobs_run() {
    for protected in [false, true] {
        let declaration = if protected {
            "[[pipeline.tables]]\nname = \"scores\"\nredaction = [{ table = \"scores\", column = \"score\", operation = \"drop\" }]\n"
        } else { "" };
        let (dir, public, token) = project(&format!("{declaration}\n{}", job("max_in_flight = 1\n")));
        let ledger = dir.path().join("body-calls.txt");
        let out = fire(dir.path(), &public, &token, "protected-output", "2030-01-01T00:01:00Z", &[("SCORE_LEDGER", ledger.to_str().unwrap())]);
        if protected {
            assert!(err(&out).contains("JournalRedactionConflict"));
            assert!(!ledger.exists(), "no recorded body effect runs before canonical output admission");
            assert!(!dir.path().join(".contextful/context/research/machine.sqlite").exists());
        } else {
            ok(&out);
            assert_eq!(std::fs::read_to_string(ledger).unwrap().lines().count(), 3);
        }
    }
}

#[test]
fn a_registered_prepared_body_lands_protected_effect_results() {
    let declaration = "[[pipeline.tables]]\nname = \"scores\"\nredaction = [{ table = \"scores\", column = \"score\", operation = \"drop\" }]\n";
    let (dir, public, token) = project(&format!("{declaration}\n{}", job("max_in_flight = 1\n")));
    let ledger = dir.path().join("prepared-body-calls.txt");
    let out = fire(dir.path(), &public, &token, "prepared-body", "2030-01-01T00:01:00Z", &[("SCORE_RECORDED", "1"), ("SCORE_LEDGER", ledger.to_str().unwrap())]);
    ok(&out);
    assert_eq!(std::fs::read_to_string(ledger).unwrap().lines().count(), 3);
    assert_eq!(select(dir.path(), "SELECT doc_id FROM scores ORDER BY doc_id"), vec![vec!["d1".to_string()], vec!["d2".to_string()], vec!["d3".to_string()]]);
    assert!(select(dir.path(), "SELECT score FROM scores").iter().all(|row| row == &["null".to_string()]));
    let declaration = "[[pipeline.tables]]\nname = \"scores\"\nredaction = [{ table = \"scores\", column = \"score\", match = { pattern = \"private\" }, operation = \"drop\" }]\n";
    let (guarded, public, token) = project(&format!("{declaration}\n{}", job("max_in_flight = 1\n")));
    ok(&fire(guarded.path(), &public, &token, "guarded-body", "2030-01-01T00:01:00Z", &[("SCORE_RECORDED", "1"), ("SCORE_CANARY", "AKIA0123456789ABCDEF")]));
    assert!(select(guarded.path(), "SELECT score FROM scores").iter().all(|row| row == &[contextful_core::pipeline::guard::MARKER.to_string()]), "retained paid result cells receive the ordinary guard before recording and staging");
}

#[test]
fn a_cancelled_second_prepared_output_collects_its_relational_children_and_keeps_the_first() {
    let declares = job("max_in_flight = 1\n").replace("tables = [\"scores\"]", "tables = [\"audits\", \"scores\"]");
    let declaration = "[[pipeline.tables]]\nname='audits'\n[[pipeline.tables]]\nname='scores'\n[[pipeline.tables]]\nname='scores_score'\nredaction=[{table='scores_score', column='text', operation='hash'}]\n";
    let (dir, public, token) = project(&format!("{declaration}\n{declares}"));
    let p = dir.path();
    let store = contextful_context::Store::open(p, "research").unwrap();
    let lock = store.lock_commit("scores").unwrap();
    let ledger = p.join("paid.txt");
    let mut child = Command::new(host_binary()).args(["job", "fire", "score-documents", "--project", "research", "--run-id", "second-output", "--site-id", "site", "--now", "2030-01-01T00:01:00Z", "--public-key", &public, "--audience", AUD])
        .current_dir(p).env_remove("CONTEXTFUL_NODE_ID").env("CONTEXTFUL_TOKEN", &token)
        .env("SCORE_RECORDED", "1").env("SCORE_RELATIONAL", "1").env("SCORE_AUDIT", "1")
        .env("SCORE_CANARY", "private-relational-model-canary").env("SCORE_LEDGER", &ledger)
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
    let node_dir = store.table_dir("scores_score").unwrap().join("data/runs/second-output.scores/ingest-a");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !node_dir.join("_manifest.json").is_file() {
        if child.try_wait().unwrap().is_some() {
            let out = child.wait_with_output().unwrap();
            panic!("second output never reached its real child commit: {}", String::from_utf8_lossy(&out.stderr));
        }
        assert!(std::time::Instant::now() < deadline, "second output reaches child commit while its root lock is held");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // The fixture holds the root table lock outside the frontier-before-table order, so a
    // read here would wait on the frontier the blocked child holds; publication of the first
    // output is checked once the lock is released.
    ok(&cf(p, &["run", "cancel", "second-output", "--project", "research", "--reason", "fixture parent retirement"]));
    drop(lock);
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success(), "the cancelled live parent cannot publish the second root");
    let row: serde_json::Value = serde_json::from_str(&ok(&cf(p, &["run", "show", "second-output", "--project", "research"]))).unwrap();
    assert_eq!(row["status"], "canceled", "the existing stop closes the attempt under the cancellation policy");
    assert_eq!(std::fs::read_to_string(&ledger).unwrap().lines().count(), 3);
    assert_eq!(select(p, "SELECT doc_id FROM audits ORDER BY doc_id").len(), 3, "the first declared output stays published");
    assert!(!store.table_dir("scores").unwrap().join("data/runs/second-output.scores/ingest-a/_group.json").exists());
    assert!(select(p, "SELECT text FROM scores_score").is_empty(), "the unpublished second group is invisible");
    assert!(!node_dir.join("stage.staging").exists(), "failed output collects its child's staged directory");
    assert!(!store.table_dir("scores").unwrap().join("data/runs/second-output.scores/ingest-a/stage.staging").exists(), "failed output collects its root staged directory");
    fn no_canary(path: &Path) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() { no_canary(&path); } else {
                let bytes = std::fs::read(&path).unwrap();
                assert!(!bytes.windows(b"private-relational-model-canary".len()).any(|value| value == b"private-relational-model-canary"), "normalized paid results remove the designated canary before identity and persistence");
            }
        }
    }
    no_canary(p);
}

#[test]
fn a_refused_second_output_child_commit_retains_names_for_existing_discard() {
    let declares = job("max_in_flight = 1\n").replace("tables = [\"scores\"]", "tables = [\"audits\", \"scores\"]");
    let declaration = "[[pipeline.tables]]\nname='audits'\n[[pipeline.tables]]\nname='scores'\n[[pipeline.tables]]\nname='scores_score'\nredaction=[{table='scores_score', column='text', operation='hash'}]\n";
    let (dir, public, token) = project(&format!("{declaration}\n{declares}"));
    let p = dir.path();
    let store = contextful_context::Store::open(p, "research").unwrap();
    let lock = store.lock_commit("scores_score").unwrap();
    let mut child = Command::new(host_binary()).args(["job", "fire", "score-documents", "--project", "research", "--run-id", "child-refusal", "--site-id", "site", "--now", "2030-01-01T00:01:00Z", "--public-key", &public, "--audience", AUD])
        .current_dir(p).env_remove("CONTEXTFUL_NODE_ID").env("CONTEXTFUL_TOKEN", &token)
        .env("SCORE_RECORDED", "1").env("SCORE_RELATIONAL", "1").env("SCORE_AUDIT", "1")
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
    let node_dir = store.table_dir("scores_score").unwrap().join("data/runs/child-refusal.scores/ingest-a");
    let stage = node_dir.join("stage.staging");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !stage.is_dir() {
        if child.try_wait().unwrap().is_some() {
            let out = child.wait_with_output().unwrap();
            panic!("second output never staged its real child: {}", String::from_utf8_lossy(&out.stderr));
        }
        assert!(std::time::Instant::now() < deadline, "second output reaches child stage while its commit lock is held");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    std::fs::create_dir(node_dir.join("_manifest.json")).unwrap();
    drop(lock);
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success(), "the real child manifest obstruction refuses publication");
    assert_eq!(select(p, "SELECT doc_id FROM audits ORDER BY doc_id").len(), 3, "the earlier group remains published");
    assert!(!store.table_dir("scores").unwrap().join("data/runs/child-refusal.scores/ingest-a/_group.json").exists());
    assert!(!stage.exists(), "the existing discard owner retains failed child names until root publication succeeds");
    assert!(!store.table_dir("scores").unwrap().join("data/runs/child-refusal.scores/ingest-a/stage.staging").exists());
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

/// A `fold` target naming no produced table, or a `build` target naming no declared SQL model,
/// raises `JobTargetUnbound` at validation.
// spec: surface.fire.target-unbound@c1cb4441
#[test]
fn a_job_target_naming_nothing_produced_is_refused_at_validation() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let pipelines = "[[pipeline]]\nid = \"meta_ads\"\ntables = [\"insights\", { name = \"spend\", columns = { day = \"timestamp\" } }]\n\
                     [pipeline.source]\nname = \"http\"\nconfig = { endpoint = \"https://api.vendor.example/v1\" }\n\n";
    let with = |jobs: &str| std::fs::write(p.join("contextful.toml"), format!("{pipelines}{jobs}\n[[model]]\nid=\"daily\"\nsql=\"SELECT day FROM meta_ads_spend\"\npublish=false\n")).unwrap();
    with("[[job]]\nname = \"nightly-fold\"\nkind = \"fold\"\ntarget = \"meta_ads_insights\"\n[[job]]\nname = \"spend\"\nkind = \"build\"\ntarget = \"daily\"\n");
    let out = ok(&cf(p, &["job", "validate"]));
    assert!(out.contains("nightly-fold: valid (fold)") && out.contains("spend: valid (build)"), "{out}");
    for (kind, target) in [("fold", "meta_ads_clicks"), ("build", "meta_ads_insights"), ("build", "meta_ads_spend")] {
        with(&format!("[[job]]\nname = \"j\"\nkind = \"{kind}\"\ntarget = \"{target}\"\n"));
        let e = err(&cf(p, &["job", "validate"]));
        assert!(e.contains("JobTargetUnbound") && e.contains(target), "{e}");
    }
}


fn scheduled_host(dir: &Path, public: &str, token: &str, args: &[&str], extra: &[(&str, &str)]) -> Output {
    let mut vars = vec![("CONTEXTFUL_TOKEN", token), ("CONTEXTFUL_ISSUER_PUBKEY", public), ("CONTEXTFUL_AUDIENCE", AUD)];
    vars.extend_from_slice(extra);
    host(dir, args, &vars)
}

#[test]
fn scheduled_jobs_use_the_applied_body_and_preserve_the_cadence_across_restart() {
    let declaration = format!("site_id = \"site\"\n{}", job("max_in_flight = 1\nschedule = \"every 1m\"\n"));
    let (dir, public, token) = project(&declaration);
    let p = dir.path();
    let ledger = p.join("scheduled-paid.txt");
    ok(&scheduled_host(p, &public, &token, &["pipeline", "import", "--project", "research"], &[]));
    // The running snapshot retains its registered body and pinned input after a draft edit.
    std::fs::write(p.join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{}", declaration.replace("body = \"score\"", "body = \"unregistered\""))).unwrap();
    let args = ["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T00:01:00Z"];
    let first = ok(&scheduled_host(p, &public, &token, &args, &[("SCORE_LEDGER", ledger.to_str().unwrap())]));
    let first: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(first["fired"], serde_json::json!(["job:score-documents"]));
    assert_eq!(std::fs::read_to_string(&ledger).unwrap().lines().count(), 3);
    let second = ok(&scheduled_host(p, &public, &token, &args, &[("SCORE_LEDGER", ledger.to_str().unwrap())]));
    let second: serde_json::Value = serde_json::from_str(&second).unwrap();
    assert_eq!(second["fired"], serde_json::json!([]));
    assert_eq!(std::fs::read_to_string(&ledger).unwrap().lines().count(), 3);
    assert_eq!(select(p, "SELECT count(*) FROM scores")[0][0], "3");
}

#[test]
fn scheduled_jobs_validate_registration_schedule_and_dispatch_identity_before_import() {
    for (extra, replacement, expected) in [
        ("max_in_flight = 1\nschedule = \"nonsense\"\n", "score", "schedule"),
        ("max_in_flight = 1\nschedule = \"every 1m\"\n", "absent", "JobBodyUnregistered"),
    ] {
        let declaration = job(extra).replace("body = \"score\"", &format!("body = {replacement:?}"));
        let (dir, public, token) = project(&declaration);
        let failure = err(&scheduled_host(dir.path(), &public, &token, &["pipeline", "import", "--project", "research"], &[]));
        assert!(failure.contains(expected), "{failure}");
        assert!(!dir.path().join(".contextful/control/research/manifest@current").exists());
    }
    let declaration = format!("{}\n[[pipeline]]\nid = \"job:score-documents\"\ntables = [\"items\"]\n[pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"https://example.com/items\" }}\n", job("max_in_flight = 1\nschedule = \"every 1m\"\n"));
    let (dir, public, token) = project(&declaration);
    let failure = err(&scheduled_host(dir.path(), &public, &token, &["pipeline", "import", "--project", "research"], &[]));
    assert!(failure.contains("job:score-documents"), "{failure}");
}

#[test]
fn unscheduled_jobs_remain_applied_without_running() {
    let (dir, public, token) = project(&format!("site_id = \"site\"\n{}", job("max_in_flight = 1\n")));
    ok(&scheduled_host(dir.path(), &public, &token, &["pipeline", "import", "--project", "research"], &[]));
    let out = ok(&scheduled_host(dir.path(), &public, &token, &["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T00:01:00Z"], &[]));
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["armed"], 0);
    assert_eq!(value["fired"], serde_json::json!([]));
    let snapshot = std::fs::read_to_string(dir.path().join(".contextful/control/research/manifest@v1.toml")).unwrap();
    assert!(snapshot.contains("score-documents"));
}

#[test]
fn scheduled_jobs_resume_paid_calls_after_a_failed_child() {
    let (dir, public, token) = project(&format!("site_id = \"site\"\n{}", job("max_in_flight = 1\nschedule = \"every 1m\"\n")));
    let p = dir.path();
    let ledger = p.join("scheduled-resume.txt");
    ok(&scheduled_host(p, &public, &token, &["pipeline", "import", "--project", "research"], &[]));
    let failed = scheduled_host(p, &public, &token, &["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T00:01:00Z"], &[("SCORE_LEDGER", ledger.to_str().unwrap()), ("SCORE_DIE_AFTER", "2")]);
    assert!(!failed.status.success());
    assert_eq!(std::fs::read_to_string(&ledger).unwrap().lines().count(), 2);
    let same = ok(&scheduled_host(p, &public, &token, &["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T00:01:00Z"], &[("SCORE_LEDGER", ledger.to_str().unwrap())]));
    assert_eq!(serde_json::from_str::<serde_json::Value>(&same).unwrap()["fired"], serde_json::json!([]));
    ok(&scheduled_host(p, &public, &token, &["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T00:02:00Z"], &[("SCORE_LEDGER", ledger.to_str().unwrap())]));
    let paid = std::fs::read_to_string(ledger).unwrap();
    assert_eq!(paid.lines().map(|line| line.split_whitespace().next().unwrap()).collect::<Vec<_>>(), ["d1", "d2", "d3"]);
    assert_eq!(select(p, "SELECT count(*) FROM scores")[0][0], "3");
}

#[test]
fn a_job_only_apply_changes_the_snapshot_and_stock_serve_refuses_its_body() {
    let declaration = format!("site_id = \"site\"\n{}", job("max_in_flight = 1\nschedule = \"every 1m\"\n"));
    let (dir, public, token) = project(&declaration);
    let p = dir.path();
    ok(&scheduled_host(p, &public, &token, &["pipeline", "import", "--project", "research"], &[]));
    let stock = err(&cf(p, &["pipeline", "serve", "--cycle", "--project", "research"]));
    assert!(stock.contains("JobBodyUnregistered"), "{stock}");
    std::fs::write(p.join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{}", declaration.replace("every 1m", "every 2m"))).unwrap();
    let planned = ok(&scheduled_host(p, &public, &token, &["pipeline", "plan", "--json", "--project", "research"], &[]));
    let planned: serde_json::Value = serde_json::from_str(&planned).unwrap();
    assert_eq!(planned["jobs"][0]["id"], "job:score-documents");
    assert_eq!(planned["jobs"][0]["action"], "change");
    assert_eq!(planned["jobs"][0]["schedule"], "every 2m");
    let changed = ok(&scheduled_host(p, &public, &token, &["pipeline", "apply", "--project", "research"], &[]));
    assert!(changed.contains("applied v2"), "{changed}");
    let snapshot = std::fs::read_to_string(p.join(".contextful/control/research/manifest@v2.toml")).unwrap();
    assert!(snapshot.contains("every 2m"));
    let unchanged = ok(&scheduled_host(p, &public, &token, &["pipeline", "apply", "--project", "research"], &[]));
    assert!(unchanged.contains("unchanged at v2"), "{unchanged}");
}
