//! `contextful job` through an embedding binary registering the `score` row body: job
//! blocks against the kind union, and a store-driven fire reading its input through the
//! read face under the job's credential.
#![cfg(feature = "data-plane")]

use contextful_core::run::drive::row_label;
use contextful_core::run::journal::EntryKey;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const AUD: &str = "contextful://acme-research";

#[test]
fn isolated_items_skip_completed_paid_calls_and_outputs_on_later_fires() {
    for prepared in [false,true] {
        let manifest = job("max_in_flight=2\n[job.isolated_items]\nlogical_key='doc_id'\nentity_key='doc_id'\nentity_namespace='documents'\nmax_attempts=2\n");
        let manifest=if prepared {format!("[[pipeline.tables]]\nname='scores'\n{manifest}")} else {manifest};
        let (dir, public, token) = project(&manifest);
        let ledger = dir.path().join("calls.txt");
        let mut env = vec![("SCORE_LEDGER",ledger.to_str().unwrap())];
        if prepared {env.push(("SCORE_RECORDED","1"));}
        let first = ok(&fire(dir.path(),&public,&token,"first","2030-01-01T00:00:30Z",&env));
        assert!(first.contains("3 successful items"),"{first}");
        ok(&fire(dir.path(),&public,&token,"second","2030-01-01T00:00:40Z",&env));
        assert_eq!(std::fs::read_to_string(ledger).unwrap().lines().count(),3);
        assert_eq!(select(dir.path(),"SELECT doc_id FROM scores ORDER BY doc_id").len(),3);
    }
}

/// One exhausted and two succeeded items each read back from the item record with their
/// job, keys, attempt count and outcome; a later fire holding their receipts adds no row.
// spec: run.items.outcome-record@3702f38d
// spec: run.items.terminal-outcome@8b1abe1a
#[test]
fn terminal_items_read_back_from_the_item_record_by_job_and_entity() {
    let (dir, public, token) = project(&job("max_in_flight=1\n[job.isolated_items]\nlogical_key='doc_id'\nentity_key='doc_id'\nentity_namespace='documents'\nmax_attempts=1\n"));
    let p = dir.path();
    let failed = err(&fire(p, &public, &token, "first", "2030-01-01T00:00:30Z", &[("SCORE_TRANSIENT", "be")]));
    assert!(failed.contains("2 successful items, 1 failed"), "{failed}");
    let sql = "SELECT job, logical_key, entity_namespace, entity_key, attempts, outcome, reason IS NULL, ended_at IS NOT NULL FROM _items ORDER BY entity_key";
    let rows = select(p, sql);
    assert_eq!(
        rows,
        [
            ["score-documents", "d1", "documents", "d1", "1", "succeeded", "true", "true"],
            ["score-documents", "d2", "documents", "d2", "1", "exhausted", "false", "true"],
            ["score-documents", "d3", "documents", "d3", "1", "succeeded", "true", "true"],
        ]
    );
    let reason = select(p, "SELECT reason FROM _items WHERE outcome = 'exhausted'");
    assert!(reason[0][0].starts_with("Transient: ") && reason[0][0].contains("provider unavailable"), "{reason:?}");

    // The terminal receipts skip every body, and the record keeps one row per item.
    err(&fire(p, &public, &token, "second", "2030-01-01T00:00:40Z", &[]));
    assert_eq!(select(p, sql), rows);
}

/// An entity key its source masks lands in the item record as the masked value.
// spec: run.items.served-key@f3cb5be5
#[test]
fn a_masked_entity_key_lands_in_the_item_record_masked() {
    let masked = format!("[[pipeline.tables]]\nname='documents'\n[pipeline.tables.policy.columns]\ndoc_id={{strategy='hash'}}\n{}", job("max_in_flight=1\n[job.isolated_items]\nlogical_key='doc_id'\nentity_key='doc_id'\nentity_namespace='documents'\nmax_attempts=1\n"));
    let (dir, public, token) = project(&masked);
    let p = dir.path();
    ok(&fire(p, &public, &token, "masked", "2030-01-01T00:00:30Z", &[]));
    let rows = select(p, "SELECT logical_key, entity_key, outcome FROM _items ORDER BY entity_key");
    assert_eq!(rows.len(), 3, "{rows:?}");
    for row in &rows {
        assert_eq!(row[0], row[1]);
        assert!(row[1].len() == 32 && row[1].bytes().all(|b| b.is_ascii_hexdigit()), "{row:?}");
        assert_eq!(row[2], "succeeded");
    }
    let everything = select(p, "SELECT * FROM _items");
    assert_eq!(everything.len(), 3, "{everything:?}");
    for clear in ["d1", "d2", "d3"] {
        assert!(everything.iter().flatten().all(|cell| cell != clear && !cell.contains(&format!("\"{clear}\""))), "{clear} in the clear: {everything:?}");
    }
}

#[path = "job_applied.rs"]
mod applied;

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
    // The embedding binary builds before the credential's 600 s lifetime starts: a cold build
    // outlasts it, and every waiting test would then present an expired credential.
    host_binary();
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

#[test]
fn a_successful_job_records_one_coordinated_output_frontier() {
    let group = "[[export]]\nname='scores-copy'\ntable='scores'\nendpoint='http://127.0.0.1:1/events'\nformat='changes-v1'\nkey=['doc_id']\nschedule='every 30s'\n[[publication]]\nname='scoring'\njob='score-documents'\nmembers=['scores-copy']\n";
    let (dir, public, token) = project(&format!("{}\n{group}",job("max_in_flight=1\n")));
    ok(&fire(dir.path(),&public,&token,"group-run","2030-01-01T00:01:00Z",&[]));
    let ledger = contextful_sqlite::PublicationLedger::open(&dir.path().join(".contextful/context/research/machine.sqlite")).unwrap();
    let source = ledger.source("scoring").unwrap().unwrap();
    assert_eq!(source.run,"group-run");
    assert_eq!(source.frontiers.keys().collect::<Vec<_>>(),["scores"]);
    assert_eq!(source.input["as_of"],"2030-01-01T00:00:10Z");
    assert_eq!(source.input["rows"],3);
}

#[test]
fn isolated_aggregate_publication_records_only_successful_complete_frontiers() {
    let group="[[export]]\nname='scores-copy'\ntable='scores'\nendpoint='http://127.0.0.1:1/events'\nformat='changes-v1'\nkey=['doc_id']\nschedule='every 30s'\n[[publication]]\nname='scoring'\njob='score-documents'\nmembers=['scores-copy']\n";
    for prepared in [false,true] {
        let isolated=job("max_in_flight=2\n[job.isolated_items]\nlogical_key='doc_id'\nentity_key='doc_id'\nentity_namespace='documents'\nmax_attempts=2\n");
        let declared=if prepared {format!("[[pipeline.tables]]\nname='scores'\n{isolated}\n{group}")} else {format!("{isolated}\n{group}")};
        let (dir,public,token)=project(&declared);
        let env=if prepared {vec![("SCORE_RECORDED","1")]} else {Vec::new()};
        ok(&fire(dir.path(),&public,&token,"isolated-group","2030-01-01T00:01:00Z",&env));
        let ledger=contextful_sqlite::PublicationLedger::open(&dir.path().join(".contextful/context/research/machine.sqlite")).unwrap();
        let source=ledger.source("scoring").unwrap().expect("successful isolated aggregate records its complete producer frontier");
        assert_eq!(source.run,"isolated-group");assert_eq!(source.input["rows"],3);assert_eq!(source.frontiers.keys().collect::<Vec<_>>(),["scores"]);
        assert_eq!(select(dir.path(),"SELECT doc_id FROM scores ORDER BY doc_id").len(),3);
    }
    let isolated=job("max_in_flight=2\n[job.isolated_items]\nlogical_key='doc_id'\nentity_key='doc_id'\nentity_namespace='documents'\nmax_attempts=2\n");
    let (dir,public,token)=project(&format!("{isolated}\n{group}"));
    let store=contextful_context::Store::open(dir.path(),"research").unwrap();
    let data=store.table_dir("scores").unwrap().join("data");std::fs::create_dir_all(&data).unwrap();std::fs::write(data.join("runs"),b"landing obstruction").unwrap();
    assert!(!fire(dir.path(),&public,&token,"failed-group","2030-01-01T00:01:00Z",&[]).status.success());
    let ledger=contextful_sqlite::PublicationLedger::open(&dir.path().join(".contextful/context/research/machine.sqlite")).unwrap();
    assert!(ledger.source("scoring").unwrap().is_none(),"failed isolated aggregate publishes no completion frontier");
}

#[test]
fn coordinated_job_exports_and_rebuilds_include_a_quiet_empty_member() {
    use contextful_acceptance::http::{Response,Server};
    use std::sync::{Arc,Mutex};
    let receiver_dir = tempfile::tempdir().unwrap();
    let receiver = Arc::new(Mutex::new(contextful_sqlite::PublicationReceiver::open(&receiver_dir.path().join("receiver.sqlite")).unwrap()));
    let sink = receiver.clone();
    let server = Server::start(move |request| {
        let batch = serde_json::from_slice(&request.body).unwrap();
        sink.lock().unwrap().receive(&batch).unwrap();
        Response::json(200,"{}")
    });
    let group = format!("[[export]]\nname='scores-copy'\ntable='scores'\nendpoint='{}'\nformat='changes-v1'\nkey=['doc_id']\nschedule='every 30s'\n[[export]]\nname='audits-copy'\ntable='audits'\nendpoint='{}'\nformat='changes-v1'\nkey=['doc_id']\nschedule='every 30s'\n[[publication]]\nname='scoring'\njob='score-documents'\nmembers=['scores-copy','audits-copy']\n",server.url("/events"),server.url("/events"));
    let declares = job("max_in_flight=1\n").replace("tables = [\"scores\"]","tables = [\"scores\",\"audits\"]");
    let (dir,public,token) = project(&format!("[[pipeline.tables]]\nname='audits'\n{declares}\n{group}"));
    let p = dir.path();
    let paid = p.join("paid.txt");
    ok(&fire(p,&public,&token,"quiet-group","2030-01-01T00:01:00Z",&[("SCORE_LEDGER",paid.to_str().unwrap())]));
    let export_token = ok(&cf(p,&["token","mint","--issuer-key",".contextful/issuer.seed","--on-behalf-of","svc://projection","--zone","on-prem:hq","--table","scores","--table","audits","--ttl","600"]));
    let deliver = |command:&str,extra:&[&str]| {
        let mut args = vec!["export",command,"scoring","--project","research","--public-key",&public,"--audience",AUD];
        args.extend_from_slice(extra);
        Command::new(env!("CARGO_BIN_EXE_contextful")).args(args).current_dir(p).env("CONTEXTFUL_TOKEN",&export_token).env_remove("CONTEXTFUL_NODE_ID").output().unwrap()
    };
    ok(&deliver("group",&[]));
    let state = receiver.lock().unwrap().state("scoring").unwrap();
    assert_eq!(state["scores-copy"].len(),3);
    assert!(state["audits-copy"].is_empty());
    ok(&deliver("rebuild",&["--namespace","rebuilt"]));
    assert_eq!(receiver.lock().unwrap().state("scoring").unwrap(),state);
    assert_eq!(receiver.lock().unwrap().active("scoring").unwrap().unwrap().plan.namespace,"rebuilt");
    assert_eq!(std::fs::read_to_string(paid).unwrap().lines().count(),3);
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
    assert!(!select(dir.path(), "SELECT score FROM scores").is_empty(), "the exclusion below ranges over no element");
    assert!(select(dir.path(), "SELECT score FROM scores").iter().all(|row| row == &["null".to_string()]));
    let declaration = "[[pipeline.tables]]\nname = \"scores\"\nredaction = [{ table = \"scores\", column = \"score\", match = { pattern = \"private\" }, operation = \"drop\" }]\n";
    let (guarded, public, token) = project(&format!("{declaration}\n{}", job("max_in_flight = 1\n")));
    ok(&fire(guarded.path(), &public, &token, "guarded-body", "2030-01-01T00:01:00Z", &[("SCORE_RECORDED", "1"), ("SCORE_CANARY", "AKIA0123456789ABCDEF")]));
    assert!(!select(guarded.path(), "SELECT score FROM scores").is_empty(), "the exclusion below ranges over no element");
    assert!(select(guarded.path(), "SELECT score FROM scores").iter().all(|row| row == &[contextful_core::pipeline::guard::MARKER.to_string()]), "retained paid result cells receive the ordinary guard before recording and staging");
}

/// Polls until `reached` holds while `child` runs, and panics with the child's stderr when
/// it exits first. The verdict rests on the child's state transition; the bound only stops a
/// hung child from holding the suite, so it sits far above a loaded host's latency.
fn await_while_running(child: &mut std::process::Child, what: &str, reached: impl Fn() -> bool) {
    const HANG_GUARD: std::time::Duration = std::time::Duration::from_secs(600);
    let started = std::time::Instant::now();
    while !reached() {
        if child.try_wait().unwrap().is_some() {
            let mut stderr = String::new();
            std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
            panic!("{what}: the child exited first: {stderr}");
        }
        assert!(started.elapsed() < HANG_GUARD, "{what}: the child still runs after {HANG_GUARD:?}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
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
    await_while_running(&mut child, "second output reaches its real child commit while its root lock is held", || node_dir.join("_manifest.json").is_file());
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
    let group = "[[export]]\nname='scores-copy'\ntable='scores'\nendpoint='http://127.0.0.1:1/events'\nformat='changes-v1'\nkey=['doc_id']\nschedule='every 30s'\n[[export]]\nname='audits-copy'\ntable='audits'\nendpoint='http://127.0.0.1:1/events'\nformat='changes-v1'\nkey=['doc_id']\nschedule='every 30s'\n[[publication]]\nname='scoring'\njob='score-documents'\nmembers=['scores-copy','audits-copy']\n";
    let (dir, public, token) = project(&format!("{declaration}\n{declares}\n{group}"));
    let p = dir.path();
    let store = contextful_context::Store::open(p, "research").unwrap();
    let lock = store.lock_commit("scores_score").unwrap();
    let mut child = Command::new(host_binary()).args(["job", "fire", "score-documents", "--project", "research", "--run-id", "child-refusal", "--site-id", "site", "--now", "2030-01-01T00:01:00Z", "--public-key", &public, "--audience", AUD])
        .current_dir(p).env_remove("CONTEXTFUL_NODE_ID").env("CONTEXTFUL_TOKEN", &token)
        .env("SCORE_RECORDED", "1").env("SCORE_RELATIONAL", "1").env("SCORE_AUDIT", "1")
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
    let node_dir = store.table_dir("scores_score").unwrap().join("data/runs/child-refusal.scores/ingest-a");
    let stage = node_dir.join("stage.staging");
    await_while_running(&mut child, "second output stages its real child while its commit lock is held", || stage.is_dir());
    std::fs::create_dir(node_dir.join("_manifest.json")).unwrap();
    drop(lock);
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success(), "the real child manifest obstruction refuses publication");
    assert_eq!(select(p, "SELECT doc_id FROM audits ORDER BY doc_id").len(), 3, "the earlier group remains published");
    assert!(!store.table_dir("scores").unwrap().join("data/runs/child-refusal.scores/ingest-a/_group.json").exists());
    assert!(!stage.exists(), "the existing discard owner retains failed child names until root publication succeeds");
    assert!(!store.table_dir("scores").unwrap().join("data/runs/child-refusal.scores/ingest-a/stage.staging").exists());
    let ledger = contextful_sqlite::PublicationLedger::open(&p.join(".contextful/context/research/machine.sqlite")).unwrap();
    assert!(ledger.source("scoring").unwrap().is_none(),"a partial producer publishes no coordinated completion frontier");
}

/// The rows every body emits land after the last input row completes and before the owner retires, one run per
/// declared output table through {{run.land.stage-order}}.
// spec: run.journal.row-output@90602540
#[test]
fn each_declared_output_table_lands_in_its_own_run_and_a_failed_landing_holds_the_owner() {
    let declares = |tables: &str| job("max_in_flight = 2\n").replace("tables = [\"scores\"]", tables);
    let declaration = declares("tables = [\"scores\", \"audits\"]");
    let (dir, public, token) = project(&declaration);
    let p = dir.path();
    let ledger = p.join("ledger.txt");
    let ledger_env = ledger.to_str().unwrap();
    let env = [("SCORE_LEDGER", ledger_env), ("SCORE_AUDIT", "1")];
    let paid = || std::fs::read_to_string(&ledger).unwrap().lines().count();

    // A real publication obstruction holds the owner after all body calls completed.
    let store = contextful_context::Store::open(p, "research").unwrap();
    let obstruction = store.table_dir("scores").unwrap().join("data/runs/fire-1.scores/ingest-a/_manifest.json");
    std::fs::create_dir_all(&obstruction).unwrap();
    let refused = err(&fire(p, &public, &token, "fire-1", "2030-01-01T00:01:00Z", &env));
    assert!(refused.contains("fire-1 failed"), "{refused}");
    assert_eq!(paid(), 3, "every row paid before the landing");

    // A changed output membership cannot reinterpret this pending execution.
    std::fs::write(p.join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{}", declares("tables = [\"scores\"]"))).unwrap();
    let changed = err(&fire(p, &public, &token, "changed-output", "2030-01-01T00:02:00Z", &env));
    assert!(changed.contains("ExecutionPinMismatch"), "{changed}");
    assert_eq!(paid(), 3);
    std::fs::write(p.join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{declaration}")).unwrap();
    std::fs::remove_dir(&obstruction).unwrap();
    // Repairing storage with the original contract resumes without another paid call.
    let out = ok(&fire(p, &public, &token, "fire-2", "2030-01-01T00:02:00Z", &env));
    assert!(out.contains("score-documents: fire-2 success · 6 rows landed from 3 input rows"), "{out}");
    assert_eq!(paid(), 3, "the resume replays every recorded call");
    let history: serde_json::Value = serde_json::from_str(&ok(&cf(p, &["run", "history", "--project", "research", "--pipeline", "score-documents"]))).unwrap();
    let mut runs: Vec<(String, String, String, u64)> = history["runs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["status"] == "success")
        .map(|r| (r["run_id"].as_str().unwrap().to_string(), r["table"].as_str().unwrap().to_string(), r["status"].as_str().unwrap().to_string(), r["rows"].as_u64().unwrap()))
        .collect();
    runs.sort();
    assert_eq!(
        runs,
        vec![("fire-2.audits".to_string(), "audits".to_string(), "success".to_string(), 3), ("fire-2.scores".to_string(), "scores".to_string(), "success".to_string(), 3)],
        "one successful run per declared output table"
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

// spec: surface.dispatch.fire-credential-inherited@fe4cd292
#[test]
fn scheduled_jobs_use_the_applied_body_and_preserve_the_cadence_across_restart() {
    let group = "[[export]]\nname='scores-copy'\ntable='scores'\nendpoint='http://127.0.0.1:1/events'\nformat='changes-v1'\nkey=['doc_id']\nschedule='every 30s'\n[[publication]]\nname='scoring'\njob='score-documents'\nmembers=['scores-copy']\n";
    let declaration = format!("site_id = \"site\"\n{}\n{group}", job("max_in_flight = 1\nschedule = \"every 1m\"\n"));
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
    let source = contextful_sqlite::PublicationLedger::open(&p.join(".contextful/context/research/machine.sqlite")).unwrap().source("scoring").unwrap().expect("the applied body publishes its local output group");
    assert_eq!(source.input["rows"],3);
    assert_eq!(source.frontiers.len(),1);
    assert_eq!(std::fs::read_to_string(&ledger).unwrap().lines().count(), 3);
    let second = ok(&scheduled_host(p, &public, &token, &args, &[("SCORE_LEDGER", ledger.to_str().unwrap())]));
    let second: serde_json::Value = serde_json::from_str(&second).unwrap();
    assert_eq!(second["fired"], serde_json::json!([]));
    assert_eq!(std::fs::read_to_string(&ledger).unwrap().lines().count(), 3);
    assert_eq!(select(p, "SELECT count(*) FROM scores")[0][0], "3");
}

/// A cycle of the embedding binary holding the issuer pin and audience and no credential.
fn tokenless_cycle(dir: &Path, public: &str, extra_args: &[&str], extra: &[(&str, &str)]) -> Output {
    let mut args = vec!["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T00:01:00Z"];
    args.extend_from_slice(extra_args);
    Command::new(host_binary())
        .args(&args)
        .envs([("CONTEXTFUL_ISSUER_PUBKEY", public), ("CONTEXTFUL_AUDIENCE", AUD)])
        .envs(extra.iter().copied())
        .current_dir(dir)
        .env_remove("CONTEXTFUL_NODE_ID")
        .env_remove("CONTEXTFUL_TOKEN")
        .output()
        .unwrap()
}

// spec: surface.dispatch.fire-credential@ec4a47af
// spec: surface.dispatch.fire-credential-lifetime@0f8b546a
#[test]
fn an_issuer_key_mints_each_store_driven_fire_a_credential_over_its_own_tables() {
    let declaration = format!("site_id = \"site\"\n{}\n[project.zone]\ndefault = \"on-prem:hq\"\n", job("max_in_flight = 1\nschedule = \"every 1m\"\n"));
    let (dir, public, token) = project(&declaration);
    let p = dir.path();
    ok(&scheduled_host(p, &public, &token, &["pipeline", "import", "--project", "research"], &[]));
    let held = p.join("fire-credential.txt");
    let out = ok(&tokenless_cycle(p, &public, &["--issuer-key", ".contextful/issuer.seed"], &[("SCORE_CREDENTIAL", held.to_str().unwrap())]));
    let answer: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(answer["fired"], serde_json::json!(["job:score-documents"]), "{out}");
    assert_eq!(select(p, "SELECT count(*) FROM scores")[0][0], "3");
    let seen = std::fs::read_to_string(&held).unwrap();
    let credential = seen.lines().next().unwrap();
    assert_ne!(credential, token);
    let seed = std::fs::read_to_string(p.join(".contextful/issuer.seed")).unwrap();
    assert!(!seen.contains("issuer.seed") && !seen.contains(seed.trim()), "the issuer seed reaches no child argument or variable: {seen}");
    let card: serde_json::Value = serde_json::from_str(&ok(&cf(p, &["token", "introspect", "--token", credential]))).unwrap();
    let text = card.to_string();
    assert_eq!(card["authority"]["exp"].as_i64().unwrap() - card["authority"]["iat"].as_i64().unwrap(), 600, "{text}");
    assert!(text.contains("svc://job/score-documents"), "{text}");
    assert!(text.contains("on-prem:hq"), "the credential carries the project's default zone: {text}");
    let grants = card["authority"]["grants"].as_array().unwrap_or_else(|| panic!("{text}"));
    let scope: Vec<(Vec<String>, Vec<String>)> = grants
        .iter()
        .map(|g| {
            let strings = |v: &serde_json::Value| v.as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_string()).collect::<Vec<_>>();
            (strings(&g["actions"]), strings(&g["tables"]))
        })
        .collect();
    assert_eq!(scope, vec![(vec!["read".to_string()], vec!["documents".to_string()]), (vec!["write".to_string()], vec!["scores".to_string()])], "{text}");
}

/// A minted fire's child admits under the pin and audience serve minted it with, whatever
/// audience or pin serve's own environment carries.
// spec: surface.dispatch.fire-credential-pins@b6a0d0c7
#[test]
fn a_minted_fire_admits_under_the_minting_pin_and_audience_whatever_serve_inherited() {
    let declaration = format!("site_id = \"site\"\n{}\n[project.zone]\ndefault = \"on-prem:hq\"\n", job("max_in_flight = 1\nschedule = \"every 1m\"\n"));
    let (dir, public, token) = project(&declaration);
    let p = dir.path();
    ok(&scheduled_host(p, &public, &token, &["pipeline", "import", "--project", "research"], &[]));
    let out = Command::new(host_binary())
        .args(["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T00:01:00Z", "--issuer-key", ".contextful/issuer.seed", "--public-key", &public])
        .env("CONTEXTFUL_AUDIENCE", "contextful://another-project")
        .current_dir(p)
        .env_remove("CONTEXTFUL_NODE_ID")
        .env_remove("CONTEXTFUL_TOKEN")
        .env_remove("CONTEXTFUL_ISSUER_PUBKEY")
        .output()
        .unwrap();
    let out = ok(&out);
    let answer: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(answer["fired"], serde_json::json!(["job:score-documents"]), "{out}");
    assert_eq!(select(p, "SELECT count(*) FROM scores")[0][0], "3");
}

// spec: surface.dispatch.fire-credential-missing@49c1976e
#[test]
fn a_serve_scheduling_a_store_driven_job_without_key_or_credential_refuses_at_start() {
    let (dir, public, token) = project(&format!("site_id = \"site\"\n{}", job("max_in_flight = 1\nschedule = \"every 1m\"\n")));
    let p = dir.path();
    ok(&scheduled_host(p, &public, &token, &["pipeline", "import", "--project", "research"], &[]));
    let ledger = p.join("refused-paid.txt");
    let failure = err(&tokenless_cycle(p, &public, &[], &[("SCORE_LEDGER", ledger.to_str().unwrap())]));
    assert!(failure.contains("FireCredentialMissing"), "{failure}");
    assert!(failure.contains("score-documents"), "{failure}");
    assert!(!ledger.exists(), "no fire runs");
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

/// A plan fanning out three translations, the French one failing, rejoined at `merge`.
fn fan_out_plan(allow_partial: bool) -> String {
    let step = |id: &str, script: &str| serde_json::json!({ "id": id, "kind": "step", "connector": { "id": "translator", "version": "1", "command": ["sh", "-c", script] } });
    let mut publish = step("publish", "printf done");
    publish["after"] = serde_json::json!(["merge"]);
    serde_json::json!({
        "id": "translate",
        "nodes": [
            step("fetch", "printf doc"),
            { "id": "fan", "kind": "parallel", "nodes": ["en", "fr", "de"], "after": ["fetch"] },
            step("en", "printf hello"),
            step("fr", "echo the vendor refused >&2; exit 3"),
            step("de", "printf hallo"),
            { "id": "merge", "kind": "join", "parallel": "fan", "allow_partial": allow_partial },
            publish,
        ],
    })
    .to_string()
}

/// The run record of `run_id`, read through `run history`.
fn recorded(dir: &Path, run_id: &str) -> serde_json::Value {
    let history: serde_json::Value = serde_json::from_str(&ok(&cf(dir, &["run", "history", "--project", "research"]))).unwrap();
    let runs = history["runs"].as_array().unwrap();
    assert!(!runs.is_empty(), "the history lists the fired runs");
    runs.iter().find(|r| r["run_id"] == run_id).unwrap_or_else(|| panic!("no run `{run_id}` in {history}")).clone()
}

/// Fan-out bodies rejoin only at an explicit join node; a join reached by a failed branch fails the run unless it
/// declares `allow_partial`, which records each failed branch's label and failure tag on the run record.
// spec: run.journal.fan-out-join@ca72f881
#[test]
fn a_fired_plan_rejoins_its_fan_out_and_a_partial_join_records_the_failed_branch() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::create_dir_all(p.join(".contextful/context/research")).unwrap();
    std::fs::write(p.join(".contextful/context/research/config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    std::fs::create_dir_all(p.join("plans")).unwrap();
    std::fs::write(p.join("plans/partial.json"), fan_out_plan(true)).unwrap();
    std::fs::write(p.join("plans/strict.json"), fan_out_plan(false)).unwrap();
    std::fs::write(
        p.join("contextful.toml"),
        "[[job]]\nname = \"translate\"\nkind = \"plan\"\nplan = \"plans/partial.json\"\n\n[[job]]\nname = \"translate-strict\"\nkind = \"plan\"\nplan = \"plans/strict.json\"\n",
    )
    .unwrap();
    let valid = ok(&cf(p, &["job", "validate"]));
    assert!(valid.contains("translate: valid (plan, plan translate version") && valid.contains("7 nodes"), "{valid}");

    // A join declaring `allow_partial` passes the others on and records the failed branch.
    let fire = |job: &str, run: &str| cf(p, &["job", "fire", job, "--project", "research", "--run-id", run, "--site-id", "site", "--now", "2030-01-01T00:00:00Z"]);
    let partial = ok(&fire("translate", "plan-partial"));
    assert!(partial.contains("plan-partial success") && partial.contains("failed branches: fr Permanent"), "{partial}");
    let row = recorded(p, "plan-partial");
    assert_eq!(row["status"], "success", "{row}");
    assert_eq!(row["host_scope"], "job:translate");
    assert_eq!(row["failed_branches"], serde_json::json!([{ "label": "fr", "tag": "Permanent" }]), "{row}");

    // A join declaring no `allow_partial` fails the run on the branch's failure.
    let strict = err(&fire("translate-strict", "plan-strict"));
    assert!(strict.contains("plan-strict failed") && strict.contains("branch `fr`") && strict.contains("join `merge`"), "{strict}");
    let row = recorded(p, "plan-strict");
    assert_eq!((&row["status"], &row["error_kind"]), (&serde_json::json!("failed"), &serde_json::json!("Permanent")), "{row}");
    assert!(row.get("failed_branches").is_none(), "a failed run records no partial result: {row}");
}

/// The authoring surface runs at build time only and emits a content-hashed plan; nothing from it executes where the
/// engine serves, and no build profile embeds a scripting runtime.
// spec: run.compile.authoring-surface@9426a5eb
#[test]
fn a_plan_compiles_at_validate_to_a_content_hashed_version_and_no_profile_links_a_script_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::create_dir_all(p.join("plans")).unwrap();
    let plan = r#"{"id":"pull","nodes":[{"id":"fetch","kind":"step","connector":{"id":"vendor","version":"1","command":["sh","-c","touch ran"]}}]}"#;
    std::fs::write(p.join("plans/pull.json"), plan).unwrap();
    std::fs::write(p.join("contextful.toml"), "[[job]]\nname = \"pull\"\nkind = \"plan\"\nplan = \"plans/pull.json\"\n").unwrap();
    let version = contextful_core::run::nodes::NodePlan::compile(plan.as_bytes()).unwrap().version;
    let valid = ok(&cf(p, &["job", "validate"]));
    assert!(valid.contains(&format!("plan pull version {version}, 1 nodes")), "{valid}");
    assert!(!p.join("ran").exists() && !p.join(".contextful").exists(), "validate compiles and runs nothing");

    // A step body written as code never reaches the engine.
    std::fs::write(p.join("plans/pull.json"), r#"{"id":"pull","nodes":[{"id":"fetch","kind":"step","script":"fetch('https://vendor.test')"}]}"#).unwrap();
    assert!(err(&cf(p, &["job", "validate"])).contains("PipelineInlineStepBody"));

    let lock = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock")).unwrap();
    let packages: Vec<&str> = lock.lines().filter_map(|l| l.strip_prefix("name = \"")).map(|n| n.trim_end_matches('"')).collect();
    assert!(packages.contains(&"contextful-cli"), "the exclusion below ranges over the workspace graph");
    for runtime in ["deno_core", "deno_runtime", "v8", "rusty_v8", "boa_engine", "rquickjs", "js-sandbox", "mozjs", "mlua", "rhai", "pyo3"] {
        assert!(!packages.contains(&runtime), "the workspace graph resolves `{runtime}`");
    }
    assert!(!packages.iter().any(|n| n.starts_with("quickjs")), "the workspace graph resolves a QuickJS binding");
}
