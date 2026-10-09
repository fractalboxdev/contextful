//! `contextful run` through the built binary: history, and the refusals each surface raises.
#![cfg(feature = "data-plane")]

use std::path::Path;
use std::process::{Command, Output};

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(".contextful/context/research");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    std::fs::write(dir.path().join("contextful.toml"), "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"filings\"\n").unwrap();
    std::fs::write(dir.path().join("empty.sh"), "printf '{\"rows\":[],\"more\":false}'\n").unwrap();
    for p in ["feed-a", "feed-b"] {
        std::fs::write(
            dir.path().join(format!("{p}.toml")),
            format!("pipeline = \"{p}\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1\"\ncommand = [\"sh\", \"empty.sh\"]\n"),
        )
        .unwrap();
    }
    dir
}

fn cf(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful")).args(args).current_dir(dir).env_remove("CONTEXTFUL_NODE_ID").output().unwrap()
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

fn start(dir: &Path, plan: &str, run: &str, now: &str) -> Output {
    cf(dir, &["run", "start", "--plan", plan, "--project", "research", "--run-id", run, "--site-id", "site-a", "--now", now])
}

fn history(dir: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["run", "history", "--project", "research"];
    args.extend_from_slice(extra);
    cf(dir, &args)
}

fn removal_rule() -> &'static str {
    r#"{ table = "filings", column = "body", operation = "replace", argument = "phone" }"#
}

fn canary_source(dir: &Path) {
    std::fs::write(dir.join("empty.sh"), "touch source-was-called\nprintf '{\"rows\":[{\"body\":\"415-555-0100\"}],\"more\":false}'\n").unwrap();
}

#[test]
fn removal_records_the_prepared_inline_and_blob_batch_and_resumes_without_repulling() {
    use contextful_core::run::journal::{Row, Stored, INLINE_CUTOFF_BYTES};
    use contextful_core::run::ports::{BlobStore, JournalStore};
    use contextful_core::store::declare::TableDecl;
    use contextful_policy::enforce::mask::Pepper;

    const RAW: &str = "private completion canary 98-record";
    const KEY: &str = "recorded-removal-pepper";
    let expected = Pepper::resolve(|_| Some(KEY.into())).digest(RAW);
    for large in [false, true] {
        let dir = project();
        std::fs::write(dir.path().join("contextful.toml"), r#"
authoring_posture = "per_request"
[[pipeline.tables]]
name = "filings"
redaction = [{ table = "filings", column = "body", operation = "hash" }]
[pipeline.tables.policy.columns]
body = { class = "completion", strategy = "hash" }
"#).unwrap();
        let public = if large { "x".repeat(INLINE_CUTOFF_BYTES + 1) } else { "keep".into() };
        let raw = serde_json::json!({"rows":[{"body":RAW,"public":public}],"more":true});
        std::fs::write(dir.path().join("pull.json"), raw.to_string()).unwrap();
        std::fs::write(dir.path().join("empty.sh"), "if [ \"$CONTEXTFUL_STEP\" = pull-0 ]; then echo pull-0 >> called; cat pull.json\nelif [ -f ready ]; then printf '{\"rows\":[],\"more\":false}'\nelse printf '{\"error\":{\"tag\":\"Permanent\",\"message\":\"feed paused\"}}'; exit 1; fi\n").unwrap();
        let run = |id: &str| Command::new(env!("CARGO_BIN_EXE_contextful"))
            .args(["run", "start", "--plan", "feed-a.toml", "--project", "research", "--run-id", id, "--site-id", "site-a", "--now", "2030-01-01T00:00:00Z"])
            .current_dir(dir.path()).env("CONTEXTFUL_PEPPER", KEY).env_remove("CONTEXTFUL_NODE_ID").output().unwrap();
        let first = run("attempt-a");
        assert!(!first.status.success());
        assert!(String::from_utf8_lossy(&first.stderr).contains("feed paused"), "the source must run under prepared recording: {}", String::from_utf8_lossy(&first.stderr));
        let listing: serde_json::Value = serde_json::from_str(&ok(&history(dir.path(), &[]))).unwrap();
        let execution = listing["runs"][0]["execution_id"].as_str().unwrap();
        let store = contextful_context::Store::open(dir.path(), "research").unwrap();
        let root = dir.path().join(".contextful/run/research");
        let journal = contextful_engine::stores::FileJournalStore::open(&root);
        let blobs = contextful_engine::stores::FileBlobStore::open(&root);
        let rows = journal.rows(execution).unwrap();
        let recorded = rows.iter().find_map(|row| match row { Row::Recorded { key, value } if key.step_label == "pull-0" => Some(value), _ => None }).unwrap();
        assert_eq!(matches!(recorded, Stored::Blob { .. }), large);
        let bytes = recorded.inline_bytes().unwrap_or_else(|| blobs.get(recorded.blob().unwrap()).unwrap().unwrap());
        assert!(!bytes.is_empty(), "the exclusion below ranges over no element");
        assert!(!bytes.windows(RAW.len()).any(|window| window == RAW.as_bytes()));
        assert!(bytes.windows(expected.len()).any(|window| window == expected.as_bytes()));
        if !large {
            let manifest = dir.path().join("contextful.toml");
            let original = std::fs::read_to_string(&manifest).unwrap();
            for (label, changed) in [
                ("rule", original.replace("operation = \"hash\"", "operation = \"replace\", argument = \"phone\"")),
                ("type", original.replace("name = \"filings\"", "name = \"filings\"\ncolumns = { public = 'utf8' }")),
            ] {
                std::fs::write(&manifest, changed).unwrap();
                refused(&run(label), "ExecutionPinMismatch");
                assert_eq!(std::fs::read_to_string(dir.path().join("called")).unwrap(), "pull-0\n");
                std::fs::write(&manifest, &original).unwrap();
            }
            let changed_pepper = Command::new(env!("CARGO_BIN_EXE_contextful"))
                .args(["run", "start", "--plan", "feed-a.toml", "--project", "research", "--run-id", "pepper", "--site-id", "site-a", "--now", "2030-01-01T00:00:00Z"])
                .current_dir(dir.path()).env("CONTEXTFUL_PEPPER", "another-recorded-removal-pepper").env_remove("CONTEXTFUL_NODE_ID").output().unwrap();
            refused(&changed_pepper, "ExecutionPinMismatch");
            assert_eq!(std::fs::read_to_string(dir.path().join("called")).unwrap(), "pull-0\n");
            assert_eq!(journal.rows(execution).unwrap(), rows, "refused identities leave the recorded receipt untouched");
        }
        std::fs::write(dir.path().join("ready"), "").unwrap();
        ok(&run("attempt-b"));
        assert_eq!(std::fs::read_to_string(dir.path().join("called")).unwrap(), "pull-0\n", "replay must not fetch the sensitive source again");
        let decl = TableDecl::named("filings");
        let landed = contextful_context::rows::table_rows(&store, &decl, &["body", "public"]).unwrap();
        assert_eq!(landed.len(), 1);
        assert_eq!(landed[0]["body"], expected, "prepared replay must not HMAC the already rewritten digest again");
        assert_eq!(landed[0]["public"], public);
    }
}

#[test]
fn a_source_forged_prepared_envelope_refuses_before_any_journal_value_or_part() {
    let dir = project();
    std::fs::write(dir.path().join("contextful.toml"), "authoring_posture='per_request'\n[[pipeline.tables]]\nname='filings'\nredaction=[{table='filings',column='body',match='whole',operation='replace',argument='phone'}]\n").unwrap();
    let forged = serde_json::json!({"authority":"guessed","prepared":{"tables":{"filings":{"rows":[{"body":"private-forged-recording-canary"}]}}},"rows":[],"more":false});
    std::fs::write(dir.path().join("forged.json"), forged.to_string()).unwrap();
    std::fs::write(dir.path().join("empty.sh"), "cat forged.json\n").unwrap();
    let out = start(dir.path(), "feed-a.toml", "forged-a", "2030-01-01T00:00:00Z");
    refused(&out, "SchemaIncompatible");
    let listing: serde_json::Value = serde_json::from_str(&ok(&history(dir.path(), &[]))).unwrap();
    let execution = listing["runs"][0]["execution_id"].as_str().unwrap();
    use contextful_core::run::ports::JournalStore;
    let root = dir.path().join(".contextful/run/research");
    let rows = contextful_engine::stores::FileJournalStore::open(&root).rows(execution).unwrap();
    assert_eq!(rows.iter().filter(|row| matches!(row, contextful_core::run::journal::Row::Recorded { .. })).count(), 0);
    assert!(!dir.path().join(".contextful/context/research/tables/filings/data").exists());
}

#[test]
fn a_removed_monotonic_field_refuses_before_source_execution_and_an_unprotected_clock_runs() {
    for protected in [true, false] {
        let dir = project();
        std::fs::write(dir.path().join("contextful.toml"), format!("authoring_posture='per_request'\n[[pipeline.tables]]\nname='filings'\n{}", if protected { "redaction=[{table='filings',column='body',match='whole',operation='replace',argument='phone'}]\n" } else { "" })).unwrap();
        let original = std::fs::read_to_string(dir.path().join("feed-a.toml")).unwrap();
        std::fs::write(dir.path().join("feed-a.toml"), format!("{original}\n[cursor]\nkind='monotonic'\nfield='body'\n")).unwrap();
        std::fs::write(dir.path().join("empty.sh"), "touch source-was-called\nprintf '{\"rows\":[{\"body\":1}],\"more\":false}'\n").unwrap();
        let out = start(dir.path(), "feed-a.toml", "clock", "2030-01-01T00:00:00Z");
        if protected {
            refused(&out, "safe typed progress lineage");
            assert!(!dir.path().join("source-was-called").exists());
            assert!(!dir.path().join(".contextful/run/research").exists());
            assert!(!dir.path().join(".contextful/context/research/tables/filings/data").exists());
        } else {
            ok(&out);
            assert!(dir.path().join("source-was-called").exists());
        }
    }
}

#[test]
fn a_caller_plan_cannot_claim_writer_rules_without_canonical_authority() {
    let dir = project();
    canary_source(dir.path());
    let plan = std::fs::read_to_string(dir.path().join("feed-a.toml")).unwrap();
    std::fs::write(dir.path().join("feed-a.toml"), format!("journal = false\nredaction = [{}]\n{plan}", removal_rule())).unwrap();
    refused(&start(dir.path(), "feed-a.toml", "unbound", "2030-01-01T00:00:00Z"), "canonical writer");
    assert!(!dir.path().join("source-was-called").exists());
    assert!(!dir.path().join(".contextful/context/research/machine.sqlite").exists());
}

#[test]
fn omitting_caller_rules_cannot_omit_canonical_source_preparation() {
    let dir = project();
    canary_source(dir.path());
    std::fs::write(dir.path().join("contextful.toml"), format!("authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"filings\"\nredaction = [{}]\n", removal_rule())).unwrap();
    ok(&start(dir.path(), "feed-a.toml", "journal", "2030-01-01T00:00:00Z"));
    assert!(dir.path().join("source-was-called").exists());
    let store = contextful_context::Store::open(dir.path(), "research").unwrap();
    let rows = contextful_context::rows::table_rows(&store, &contextful_core::store::declare::TableDecl::named("filings"), &["body"]).unwrap();
    assert_eq!(rows[0]["body"], serde_json::json!("[REDACTED:phone]"));
}

#[test]
fn a_nojournal_plan_lands_under_complete_canonical_writer_authority() {
    let dir = project();
    canary_source(dir.path());
    std::fs::write(dir.path().join("contextful.toml"), format!("authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"filings\"\nredaction = [{}]\n", removal_rule())).unwrap();
    let plan = std::fs::read_to_string(dir.path().join("feed-a.toml")).unwrap();
    std::fs::write(dir.path().join("feed-a.toml"), format!("journal = false\nredaction = [{}]\n{plan}", removal_rule())).unwrap();
    ok(&start(dir.path(), "feed-a.toml", "bound", "2030-01-01T00:00:00Z"));
    assert!(dir.path().join("source-was-called").exists());
    let rows: serde_json::Value = serde_json::from_str(&ok(&cf(dir.path(), &["query", "--json", "--project", "research", "SELECT body FROM filings"]))).unwrap();
    assert_eq!(rows["rows"], serde_json::json!([["[REDACTED:phone]"]]));
}

#[test]
fn encrypted_runs_replay_history_without_cleartext_run_state() {
    let dir = project();
    let store = dir.path().join(".contextful/context/research");
    std::fs::write(
        store.join("config.toml"),
        "[node]\nid = \"ingest-a\"\n[encryption]\nkey_source = \"env:CONTEXTFUL_TEST_RUN_KEY\"\n",
    )
    .unwrap();
    const CANARY: &str = "sealed-run-payload-74-9c87";
    std::fs::write(dir.path().join("empty.sh"), format!("printf '{{\"rows\":[{{\"body\":\"{CANARY}\"}}],\"more\":false}}'\n")).unwrap();
    let invoke = |args: &[&str], key: &str| {
        Command::new(env!("CARGO_BIN_EXE_contextful"))
            .args(args)
            .current_dir(dir.path())
            .env_remove("CONTEXTFUL_NODE_ID")
            .env("CONTEXTFUL_TEST_RUN_KEY", key)
            .output()
            .unwrap()
    };
    let key = "0123456789abcdef0123456789abcdef";
    let started = invoke(&["run", "start", "--plan", "feed-a.toml", "--project", "research", "--run-id", CANARY, "--site-id", "site-a", "--now", "2030-01-01T00:00:00Z"], key);
    ok(&started);
    let history_args = ["run", "history", "--project", "research"];
    let listing: serde_json::Value = serde_json::from_str(&ok(&invoke(&history_args, key))).unwrap();
    assert_eq!(listing["runs"][0]["run_id"], CANARY);
    assert!(!invoke(&history_args, "fedcba9876543210fedcba9876543210").status.success());
    assert_eq!(serde_json::from_str::<serde_json::Value>(&ok(&invoke(&history_args, key))).unwrap(), listing);
    let machine = std::fs::read(store.join("machine.sqlite")).unwrap();
    assert!(!machine.starts_with(b"SQLite format 3\0"));
    fn scan(path: &Path, canary: &[u8]) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scan(&path, canary);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                assert!(!bytes.windows(canary.len()).any(|w| w == canary), "cleartext run payload in {}", path.display());
            }
        }
    }
    scan(&dir.path().join(".contextful"), CANARY.as_bytes());
}

#[test]
fn encrypted_retained_pulls_and_cli_awake_payloads_reopen_without_plaintext() {
    use contextful_core::run::journal::{Row, Stored, INLINE_CUTOFF_BYTES};
    use contextful_core::run::ports::{BlobStore, JournalStore};
    use contextful_core::time::Instant;
    use contextful_engine::awake::{Awaited, Registry};
    use contextful_engine::Journal;

    let dir = project();
    let p = dir.path();
    let key_var = "CONTEXTFUL_TEST_KEY_74_RETAINED_RUN";
    let key = "0123456789abcdef0123456789abcdef";
    unsafe { std::env::set_var(key_var, key) };
    std::fs::write(p.join(".contextful/context/research/config.toml"), format!("[node]\nid = \"ingest-a\"\n[encryption]\nkey_source = \"env:{key_var}\"\n")).unwrap();
    const SOURCE: &str = "sealed-retained-source-74-821a";
    const CALLBACK: &str = "sealed-retained-callback-74-4ec2";
    let body = format!("{SOURCE}{}", "x".repeat(INLINE_CUTOFF_BYTES + 1));
    let pull = serde_json::to_vec(&serde_json::json!({"rows":[{"body":body}],"more":true})).unwrap();
    std::fs::write(p.join("pull.json"), &pull).unwrap();
    std::fs::write(p.join("retained.sh"), "if [ \"$CONTEXTFUL_STEP\" = pull-0 ]; then echo pull-0 >> called; cat pull.json\nelif [ -f ready ]; then printf '{\"rows\":[],\"more\":false}'\nelse printf '{\"error\":{\"tag\":\"Permanent\",\"message\":\"feed paused\"}}'; exit 1; fi\n").unwrap();
    std::fs::write(p.join("retained.toml"), "pipeline = \"retained\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1\"\ncommand = [\"sh\", \"retained.sh\"]\n").unwrap();
    let invoke = |args: &[&str]| Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(args).current_dir(p).env_remove("CONTEXTFUL_NODE_ID").env(key_var, key).output().unwrap();
    let run = |id: &str| invoke(&["run", "start", "--plan", "retained.toml", "--project", "research", "--run-id", id, "--site-id", "site-a", "--now", "2030-01-01T00:00:00Z"]);
    refused(&run("retained-1"), "feed paused");
    let history_args = ["run", "history", "--project", "research"];
    let history: serde_json::Value = serde_json::from_str(&ok(&invoke(&history_args))).unwrap();
    let execution = history["runs"][0]["execution_id"].as_str().unwrap();
    let store = contextful_context::Store::open(p, "research").unwrap();
    let machine = store.root().join("machine.sqlite");
    let stores = contextful_sqlite::SqliteRunStores::open_sealed(&machine, store.file_cipher().unwrap()).unwrap();
    let recorded = stores.journal.rows(execution).unwrap();
    assert!(recorded.iter().any(|row| matches!(row, Row::Recorded { value: Stored::Blob { bytes, .. }, .. } if *bytes > INLINE_CUTOFF_BYTES as u64)), "the retained source pull uses the blob port");
    for row in &recorded {
        if let Row::Recorded { value: Stored::Blob { sha256, .. }, .. } = row {
            let bytes = stores.blobs.get(sha256).unwrap().unwrap();
            assert_eq!(serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["rows"][0]["body"], body);
        }
    }
    let registry = Registry::over(stores.awakeables.clone(), Journal::over(stores.journal.clone(), stores.blobs.clone()));
    // Command sources do not mint awakeables: the public registry creates one in this
    // retained execution; callback delivery and subsequent reads use the built CLI.
    let awakeable = registry.suspend(execution, "approval", "2030-01-01T00:00:00Z", 3600).unwrap();
    let payload = serde_json::to_vec(&serde_json::json!({"answer":format!("{CALLBACK}{}", "y".repeat(INLINE_CUTOFF_BYTES + 1))})).unwrap();
    std::fs::write(p.join("callback.json"), &payload).unwrap();
    let delivered = invoke(&["run", "awake", &awakeable.token, "--project", "research", "--payload", "callback.json", "--now", "2030-01-01T00:00:00Z"]);
    assert_eq!(ok(&delivered), String::from_utf8(payload.clone()).unwrap());
    drop(registry);
    drop(stores);
    let reopened = contextful_sqlite::SqliteRunStores::open_sealed(&machine, store.file_cipher().unwrap()).unwrap();
    let resume = contextful_engine::awake::resume_key(execution, &awakeable.token);
    assert!(matches!(reopened.journal.read(&resume).unwrap(), Some(Row::Recorded { value: Stored::Blob { bytes, .. }, .. }) if bytes > INLINE_CUTOFF_BYTES as u64));
    let registry = Registry::over(reopened.awakeables.clone(), Journal::over(reopened.journal.clone(), reopened.blobs.clone()));
    assert_eq!(registry.awaited(execution, &awakeable.token, Instant::parse("2030-01-01T00:01:00Z").unwrap()).unwrap(), Awaited::Resumed(payload));
    assert_eq!(serde_json::from_str::<serde_json::Value>(&ok(&invoke(&history_args))).unwrap(), history);
    fn scan(path: &Path) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() { scan(&path); } else {
                let bytes = std::fs::read(&path).unwrap();
                for canary in [SOURCE, CALLBACK] {
                    assert!(!bytes.windows(canary.len()).any(|w| w == canary.as_bytes()), "plaintext retained payload in {}", path.display());
                }
            }
        }
    }
    scan(&p.join(".contextful"));
    std::fs::write(p.join("ready"), "").unwrap();
    ok(&run("retained-2"));
    assert_eq!(std::fs::read_to_string(p.join("called")).unwrap(), "pull-0\n", "the successful retry replays the retained source instead of rerunning it");
    let rows = contextful_context::rows::table_rows(&store, &contextful_core::store::declare::TableDecl::named("filings"), &["body"]).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["body"], body);
    scan(&p.join(".contextful"));
}

/// History leaves the machine as a process listing and an NDJSON export over one run projection; the export's
/// first line carries store, window, count and truncation flag, then one run per line. It resolves no bucket.
// spec: run.record.history-export@f66be52e
#[test]
fn history_exports_a_header_then_one_run_per_line() {
    let dir = project();
    for (i, run) in ["a1", "a2", "a3"].iter().enumerate() {
        ok(&start(dir.path(), "feed-a.toml", run, &format!("2030-01-01T00:0{i}:00Z")));
    }
    let listing: serde_json::Value = serde_json::from_str(&ok(&history(dir.path(), &[]))).unwrap();
    assert_eq!(listing["runs"].as_array().unwrap().len(), 3);
    let text = ok(&history(dir.path(), &["--export", "--since", "2030-01-01T00:01:00Z"]));
    let lines: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines[0]["store"], "research");
    assert_eq!(lines[0]["window"]["since"], "2030-01-01T00:01:00Z");
    assert_eq!(lines[0]["count"], 2);
    assert_eq!(lines[0]["truncated"], false);
    assert_eq!(lines[1..].iter().map(|r| r["run_id"].as_str().unwrap()).collect::<Vec<_>>(), ["a3", "a2"]);
    // Each run line is the run row the listing serves.
    assert_eq!(lines[1], listing["runs"][0]);
}

/// The export answers with 500 rows by default and clamps at 5000 rows, each pipeline's window taking the full
/// ceiling before the merged result is clipped once.
// spec: run.record.export-ceiling@81c87e7e
#[test]
fn each_pipeline_takes_the_full_ceiling_and_the_merge_clips_once() {
    use contextful_core::run::record::export_ceiling;
    assert_eq!((export_ceiling(None), export_ceiling(Some(9_999))), (500, 5_000));
    let dir = project();
    for (i, run) in ["a1", "a2", "a3"].iter().enumerate() {
        ok(&start(dir.path(), "feed-a.toml", run, &format!("2030-01-01T00:0{i}:00Z")));
    }
    ok(&start(dir.path(), "feed-b.toml", "b1", "2030-01-01T00:05:00Z"));
    let text = ok(&history(dir.path(), &["--export", "--limit", "2", "--pipeline", "feed-a", "--pipeline", "feed-b"]));
    let lines: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!((lines[0]["count"].as_u64(), lines[0]["truncated"].as_bool()), (Some(2), Some(true)));
    // feed-a contributes its two newest, feed-b its one; the merge keeps the two newest overall.
    assert_eq!(lines[1..].iter().map(|r| r["run_id"].as_str().unwrap()).collect::<Vec<_>>(), ["b1", "a3"]);
    let unasked = ok(&history(dir.path(), &["--export"]));
    assert_eq!(unasked.lines().count(), 1 + 4);
}

#[test]
fn every_run_surface_raises_its_refusal_by_name() {
    let dir = project();
    refused(&history(dir.path(), &["--since", "2030-01-01T00:00:00+08:00"]), "HistoryBoundSpelling");
    refused(&cf(dir.path(), &["run", "start", "--plan", "feed-a.toml", "--project", "research"]), "SiteIdUnresolved");
    refused(
        &cf(dir.path(), &["run", "start", "--plan", "feed-a.toml", "--project", "research", "--site-id-env", "CONTEXTFUL_TEST_UNSET_SITE"]),
        "SiteIdUnresolved",
    );
    ok(&start(dir.path(), "feed-a.toml", "a1", "2030-01-01T00:00:00Z"));
    refused(&cf(dir.path(), &["run", "cancel", "a1", "--project", "research"]), "CancelTargetNotInFlight");
    refused(&cf(dir.path(), &["run", "cancel", "nope", "--project", "research"]), "CancelTargetNotInFlight");
    std::fs::write(dir.path().join("payload.json"), "{}").unwrap();
    refused(&cf(dir.path(), &["run", "awake", "0123abcd", "--project", "research", "--payload", "payload.json"]), "AwakeableUnknown");
    std::fs::write(dir.path().join("redacting.toml"), format!("redact = [\"ssn\"]\n{}", std::fs::read_to_string(dir.path().join("feed-a.toml")).unwrap())).unwrap();
    refused(&start(dir.path(), "redacting.toml", "r1", "2030-01-01T00:00:00Z"), "JournalRedactionConflict");
}

#[test]
fn an_unknown_stop_scope_is_refused() {
    let dir = project();
    ok(&start(dir.path(), "feed-a.toml", "a1", "2030-01-01T00:00:00Z"));
    let out = cf(dir.path(), &["run", "cancel", "a1", "--project", "research", "--scope", "fire"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("fire") && stderr.contains("pipeline"), "{stderr}");
}

/// The binary keeps its run rows, cursor cache and lease rows in the store root's
/// `machine.sqlite`, and every run surface reads them back from there.
#[test]
fn run_rows_live_in_the_store_roots_machine_catalog() {
    use contextful_core::coordinate::Catalog;
    use contextful_core::ports::FixedClock;
    use contextful_core::time::Instant;
    let dir = project();
    ok(&start(dir.path(), "feed-a.toml", "a1", "2030-01-01T00:00:00Z"));
    let file = dir.path().join(".contextful/context/research/machine.sqlite");
    assert!(file.is_file(), "no machine catalog at {}", file.display());
    assert!(!dir.path().join(".contextful/run/research/runs").exists(), "no run row lands beside the journal");

    let clock = std::sync::Arc::new(FixedClock(Instant::parse("2030-01-01T00:00:00Z").unwrap()));
    let catalog = contextful_sqlite::MachineCatalog::open(&file, clock).unwrap();
    let row = catalog.run("a1").unwrap().expect("the run row");
    assert_eq!((row.pipeline_id.as_str(), row.table.as_str()), ("feed-a", "filings"));
    let shown: serde_json::Value = serde_json::from_str(&ok(&cf(dir.path(), &["run", "show", "a1", "--project", "research"]))).unwrap();
    assert_eq!(shown["run_id"], "a1");
    assert_eq!(shown["status"], serde_json::to_value(row.status).unwrap());
}

/// A pull's `types` object maps a column to a type spelled as {{store.reconcile.typed-landing}} reads it, and the
/// run commit lands that column in it; an unreadable spelling fails the pull as {{store.reconcile.incompatible}}.
// spec: run.land.typed-pull@cb800296
#[test]
fn a_pulled_type_lands_its_column_in_that_type() {
    let dir = project();
    let schema = |d: &Path| -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(d.join(".contextful/context/research/tables/filings/schema.json")).unwrap()).unwrap()
    };
    let field = |s: &serde_json::Value, name: &str| s["fields"].as_array().unwrap().iter().find(|f| f["name"] == name).cloned().unwrap_or_else(|| panic!("no `{name}` in {s}"));
    std::fs::write(
        dir.path().join("typed.sh"),
        "printf '{\"rows\":[{\"id\":\"a\",\"digest\":\"3q2+7w==\",\"embedding\":[0.5,-1,0.25]}],\"types\":{\"digest\":\"binary(4)\",\"embedding\":\"float16[3]\"},\"more\":false}'\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("typed.toml"), "pipeline = \"typed\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1\"\ncommand = [\"sh\", \"typed.sh\"]\n").unwrap();
    ok(&start(dir.path(), "typed.toml", "t1", "2030-01-01T00:00:00Z"));
    let s = schema(dir.path());
    assert_eq!(field(&s, "digest")["type"], serde_json::json!({"name": "fixedsizebinary", "byteWidth": 4}), "{s}");
    assert_eq!(field(&s, "embedding")["type"], serde_json::json!({"name": "fixedsizelist", "listSize": 3}), "{s}");
    assert_eq!(field(&s, "embedding")["children"][0]["type"]["precision"], "HALF", "{s}");

    // A later pull declaring nothing lands in the stored types.
    std::fs::write(dir.path().join("untyped.sh"), "printf '{\"rows\":[{\"id\":\"b\",\"digest\":\"AAECAw==\",\"embedding\":[1,0,0]}],\"more\":false}'\n").unwrap();
    std::fs::write(dir.path().join("untyped.toml"), "pipeline = \"untyped\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1\"\ncommand = [\"sh\", \"untyped.sh\"]\n").unwrap();
    ok(&start(dir.path(), "untyped.toml", "u1", "2030-01-01T00:01:00Z"));
    assert_eq!(schema(dir.path()), s);

    // An unreadable spelling fails the pull.
    std::fs::write(dir.path().join("bad.sh"), "printf '{\"rows\":[{\"id\":\"c\",\"v\":\"qg==\"}],\"types\":{\"v\":\"bytes(4)\"},\"more\":false}'\n").unwrap();
    std::fs::write(dir.path().join("bad.toml"), "pipeline = \"bad\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1\"\ncommand = [\"sh\", \"bad.sh\"]\n").unwrap();
    refused(&start(dir.path(), "bad.toml", "b1", "2030-01-01T00:02:00Z"), "StoreSchemaIncompatible");
    assert_eq!(schema(dir.path()), s, "the failed pull lands nothing");
}

/// An arriving schema the store cannot reconcile fails the batch as {{store.reconcile.incompatible}}.
// spec: run.land.irreconcilable-schema@04ddeb28
#[test]
fn an_irreconcilable_pulled_schema_fails_the_batch() {
    let dir = project();
    std::fs::write(dir.path().join("number.sh"), "printf '{\"rows\":[{\"id\":\"a\",\"rev\":1}],\"more\":false}'\n").unwrap();
    std::fs::write(dir.path().join("number.toml"), "pipeline = \"number\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1\"\ncommand = [\"sh\", \"number.sh\"]\n").unwrap();
    ok(&start(dir.path(), "number.toml", "n1", "2030-01-01T00:00:00Z"));

    std::fs::write(dir.path().join("text.sh"), "printf '{\"rows\":[{\"id\":\"b\",\"rev\":\"later\"}],\"more\":false}'\n").unwrap();
    std::fs::write(dir.path().join("text.toml"), "pipeline = \"text\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1\"\ncommand = [\"sh\", \"text.sh\"]\n").unwrap();
    let out = start(dir.path(), "text.toml", "t1", "2030-01-01T00:01:00Z");
    refused(&out, "StoreSchemaIncompatible");
    let store = contextful_context::Store::open(dir.path(), "research").unwrap();
    let decl = contextful_core::store::declare::TableDecl::named("filings");
    let rows = contextful_context::rows::table_rows(&store, &decl, &["id", "rev"]).unwrap();
    assert_eq!(rows.len(), 1, "the incompatible batch leaves no visible row");
    assert_eq!(rows[0]["id"], "a");
}

const AWS_KEY: &str = "AKIAIOSFODNN7EXAMPLE";
const GITHUB_TOKEN: &str = "ghp_0123456789abcdefghijABCDEFGHIJ012345";
const MARKER: &str = "[REDACTED:secret]";

/// The count of files under `dir` whose bytes hold `needle`.
fn files_holding(dir: &Path, needle: &str) -> usize {
    let mut n = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            if e.path().is_dir() {
                stack.push(e.path());
            } else if String::from_utf8_lossy(&std::fs::read(e.path()).unwrap()).contains(needle) {
                n += 1;
            }
        }
    }
    n
}

/// An ordinary journaled pull records its guarded source batch before landing.
// spec: run.journal.recorded-batch@f5847e19
#[test]
fn run_start_journals_and_lands_only_masked_credentials() {
    let dir = project();
    // The first pull serves two credentials; the second fails until `ready` exists.
    let script = format!(
        "if [ \"$CONTEXTFUL_STEP\" = pull-0 ]; then printf '{{\"rows\":[{{\"id\":\"d1\",\"aws\":\"key {AWS_KEY}\",\"gh\":\"{GITHUB_TOKEN}\"}}],\"more\":true}}'\n\
         elif [ -f ready ]; then printf '{{\"rows\":[],\"more\":false}}'\n\
         else printf '{{\"error\":{{\"tag\":\"Permanent\",\"message\":\"feed down\"}}}}'; exit 1; fi\n"
    );
    std::fs::write(dir.path().join("leaky.sh"), script).unwrap();
    std::fs::write(
        dir.path().join("leaky.toml"),
        "pipeline = \"leaky\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1\"\ncommand = [\"sh\", \"leaky.sh\"]\n",
    )
    .unwrap();

    let first = start(dir.path(), "leaky.toml", "l1", "2030-01-01T00:00:00Z");
    assert!(!first.status.success(), "the second pull fails the first attempt");
    let journal = dir.path().join(".contextful/run/research");
    assert!(files_holding(&journal, MARKER) >= 1, "the journal records the masked pull");
    for secret in [AWS_KEY, GITHUB_TOKEN] {
        assert_eq!(files_holding(&journal, secret), 0, "the journal holds `{secret}`");
    }

    std::fs::write(dir.path().join("ready"), "").unwrap();
    ok(&start(dir.path(), "leaky.toml", "l2", "2030-01-01T00:01:00Z"));
    let store = contextful_context::Store::open(dir.path(), "research").unwrap();
    let decl = contextful_core::store::declare::TableDecl::named("filings");
    let rows = contextful_context::rows::table_rows(&store, &decl, &["aws", "gh"]).unwrap();
    assert_eq!(rows.len(), 1, "the replay lands the recorded pull");
    assert_eq!(rows[0]["aws"], format!("key {MARKER}"));
    assert_eq!(rows[0]["gh"], MARKER);
    for secret in [AWS_KEY, GITHUB_TOKEN] {
        assert_eq!(files_holding(dir.path(), secret), 1, "only the connector script holds `{secret}`");
    }
}

/// A run failing after a stage leaves no staged part under its run directory; a run committing two staged parts
/// stamps every row with its commit instant, the instant its marker carries.
// spec: run.own.stage-discard@b8087166
#[test]
fn a_failed_run_leaves_no_staged_part_and_a_commit_stamps_its_instant() {
    let dir = project();
    let script = "if [ \"$CONTEXTFUL_STEP\" = pull-0 ]; then printf '{\"rows\":[{\"id\":\"d1\"}],\"more\":true}'\n\
                  elif [ -f ready ]; then printf '{\"rows\":[{\"id\":\"d2\"}],\"more\":false}'\n\
                  else printf '{\"error\":{\"tag\":\"Permanent\",\"message\":\"feed down\"}}'; exit 1; fi\n";
    std::fs::write(dir.path().join("paged.sh"), script).unwrap();
    std::fs::write(dir.path().join("paged.toml"), "pipeline = \"paged\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1\"\ncommand = [\"sh\", \"paged.sh\"]\n").unwrap();
    let runs = dir.path().join(".contextful/context/research/tables/filings/data/runs");

    assert!(!start(dir.path(), "paged.toml", "p1", "2030-01-01T00:00:00Z").status.success(), "the second pull fails the run");
    assert!(!runs.join("p1/ingest-a/stage.staging").exists(), "the failed run's staged part is discarded");

    std::fs::write(dir.path().join("ready"), "").unwrap();
    ok(&start(dir.path(), "paged.toml", "p2", "2030-01-01T00:01:00Z"));
    assert!(!runs.join("p2/ingest-a/stage.staging").exists());
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(runs.join("p2/ingest-a/_manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["parts"].as_array().unwrap().len(), 2, "{manifest}");
    let q = "SELECT id, CAST(_ingested_at AS VARCHAR) FROM filings ORDER BY id";
    let r: serde_json::Value = serde_json::from_str(&ok(&cf(dir.path(), &["query", "--json", "--project", "research", q]))).unwrap();
    let stamp = "2030-01-01 00:01:00+00";
    assert_eq!(r["rows"], serde_json::json!([["d1", stamp], ["d2", stamp]]), "{r}");
}

/// A run takes its site id from the manifest's `site_id` or `site_id_env`; `--site-id` replaces it for one run.
#[test]
fn a_run_takes_its_site_id_from_the_manifest_unless_the_flag_names_one() {
    let dir = project();
    let manifest = dir.path().join("contextful.toml");
    let tables = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, format!("site_id = \"site-m\"\n\n{tables}")).unwrap();
    let site = |run: &str| -> serde_json::Value {
        let shown: serde_json::Value = serde_json::from_str(&ok(&cf(dir.path(), &["run", "show", run, "--project", "research"]))).unwrap();
        shown["site_id"].clone()
    };
    let bare = |run: &str, extra: &[&str]| {
        let mut args = vec!["run", "start", "--plan", "feed-a.toml", "--project", "research", "--run-id", run, "--now", "2030-01-01T00:00:00Z"];
        args.extend_from_slice(extra);
        cf(dir.path(), &args)
    };
    ok(&bare("m1", &[]));
    assert_eq!(site("m1"), "site-m");
    ok(&bare("f1", &["--site-id", "site-f"]));
    assert_eq!(site("f1"), "site-f");

    std::fs::write(&manifest, format!("site_id_env = \"CONTEXTFUL_TEST_SITE\"\n\n{tables}")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["run", "start", "--plan", "feed-a.toml", "--project", "research", "--run-id", "e1", "--now", "2030-01-01T00:00:00Z"])
        .current_dir(dir.path())
        .env_remove("CONTEXTFUL_NODE_ID")
        .env("CONTEXTFUL_TEST_SITE", "site-e")
        .output()
        .unwrap();
    ok(&out);
    assert_eq!(site("e1"), "site-e");
    refused(&bare("u1", &[]), "SiteIdUnresolved");

    std::fs::write(&manifest, format!("site_id = \"site-m\"\nsite_id_env = \"CONTEXTFUL_TEST_SITE\"\n\n{tables}")).unwrap();
    refused(&bare("b1", &[]), "SiteIdUnresolved");
}

const AUD: &str = "contextful://research";

/// An issuer for the scratch project at the default seed path; its public key pin.
fn issuer(dir: &Path) -> String {
    let public = ok(&cf(dir, &["token", "keygen"]));
    ok(&cf(dir, &["token", "policy", "init", "--audience", AUD]));
    public
}

/// A credential reading `pattern`, minted now under the default issuer key.
fn reader(dir: &Path, pattern: &str) -> String {
    ok(&cf(dir, &["token", "mint", "--on-behalf-of", "user://dana@acme.example", "--table", pattern]))
}

/// A run surface under the credential in `CONTEXTFUL_TOKEN`.
fn credentialed(dir: &Path, token: &str, public: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(args)
        .args(["--project", "research", "--public-key", public, "--audience", AUD])
        .current_dir(dir)
        .env_remove("CONTEXTFUL_NODE_ID")
        .env("CONTEXTFUL_TOKEN", token)
        .output()
        .unwrap()
}

/// Tracing a run gates on the pipeline resolved from the run record and raises `GrantRunTraceDenied` without naming that pipeline.
// spec: authority.grant.run-trace-denied@3e65cef9
#[test]
fn run_show_gates_on_the_pipeline_its_run_record_names() {
    let dir = project();
    let public = issuer(dir.path());
    ok(&start(dir.path(), "feed-a.toml", "a1", "2030-01-01T00:00:00Z"));
    ok(&start(dir.path(), "feed-b.toml", "b1", "2030-01-01T00:01:00Z"));
    let token = reader(dir.path(), "feed-a");
    let shown: serde_json::Value = serde_json::from_str(&ok(&credentialed(dir.path(), &token, &public, &["run", "show", "a1"]))).unwrap();
    assert_eq!(shown["pipeline_id"], "feed-a");
    let out = credentialed(dir.path(), &token, &public, &["run", "show", "b1"]);
    refused(&out, "GrantRunTraceDenied");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("feed-b"), "the refusal names no pipeline: {stderr}");
    // An absent run refuses alike, so a credential cannot tell it from an uncovered one.
    let absent = credentialed(dir.path(), &token, &public, &["run", "show", "zz"]);
    refused(&absent, "GrantRunTraceDenied");
    assert_eq!(String::from_utf8_lossy(&absent.stderr), stderr);
    // A grant over the landed table confers no trace.
    refused(&credentialed(dir.path(), &reader(dir.path(), "filings"), &public, &["run", "show", "a1"]), "GrantRunTraceDenied");
}

/// Describing an uncovered pipeline raises `GrantPipelineNotCovered`, naming it. Listing filters to covered identifiers and raises it when none is covered; a project declaring no pipelines lists empty.
// spec: authority.grant.pipeline-not-covered@a7aff20e
#[test]
fn run_history_lists_covered_pipelines_and_refuses_an_uncovered_one() {
    let dir = project();
    let public = issuer(dir.path());
    let token = reader(dir.path(), "feed-a");
    // No run recorded: the listing is empty, not refused.
    let empty: serde_json::Value = serde_json::from_str(&ok(&credentialed(dir.path(), &token, &public, &["run", "history"]))).unwrap();
    assert_eq!(empty["runs"], serde_json::json!([]));
    ok(&start(dir.path(), "feed-a.toml", "a1", "2030-01-01T00:00:00Z"));
    ok(&start(dir.path(), "feed-b.toml", "b1", "2030-01-01T00:01:00Z"));

    let listing: serde_json::Value = serde_json::from_str(&ok(&credentialed(dir.path(), &token, &public, &["run", "history"]))).unwrap();
    let ids: Vec<&str> = listing["runs"].as_array().unwrap().iter().map(|r| r["run_id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["a1"]);

    let out = credentialed(dir.path(), &token, &public, &["run", "history", "--pipeline", "feed-b"]);
    refused(&out, "GrantPipelineNotCovered");
    assert!(String::from_utf8_lossy(&out.stderr).contains("feed-b"));

    let none = reader(dir.path(), "research/*");
    let out = credentialed(dir.path(), &none, &public, &["run", "history"]);
    refused(&out, "GrantPipelineNotCovered");
    assert!(!String::from_utf8_lossy(&out.stderr).contains("feed-"), "the empty listing names no pipeline");
}

#[test]
fn a_repaired_typed_source_runs_after_schema_refusal_without_changing_its_plan() {
    let dir = project();
    let p = dir.path();
    std::fs::write(p.join("contextful.toml"), "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"filings\"\ncolumns = { score = 'int64' }\n").unwrap();
    std::fs::write(p.join("empty.sh"), "echo \"$CONTEXTFUL_STEP\" >> called\nif [ \"$CONTEXTFUL_STEP\" = pull-0 ]; then cat first.json; else cat second.json; fi\n").unwrap();
    let first = |rows: serde_json::Value, more: bool| std::fs::write(p.join("first.json"), serde_json::json!({"rows":rows,"cursor":"p1","more":more}).to_string()).unwrap();
    let second = |score: serde_json::Value| std::fs::write(p.join("second.json"), serde_json::json!({"rows":[{"id":"two","score":score}],"cursor":"p2","more":false}).to_string()).unwrap();
    first(serde_json::json!([{"id":"seed","score":0}]), false);
    ok(&start(p, "feed-a.toml", "seed", "2030-01-01T00:00:00Z"));
    first(serde_json::json!([{"id":"one","score":1}]), true);
    second(serde_json::json!({"invalid":true}));
    refused(&start(p, "feed-a.toml", "bad", "2030-01-01T00:01:00Z"), "StoreSchemaIncompatible");
    let store = contextful_context::Store::open(p, "research").unwrap();
    let decl = contextful_core::store::declare::TableDecl::named("filings");
    let rows = || contextful_context::rows::table_rows(&store, &decl, &["id", "score"]).unwrap();
    assert_eq!(rows().len(), 1, "the valid first batch of the failed attempt stays unpublished");
    assert_eq!(rows()[0]["id"], "seed");
    second(serde_json::json!(2));
    ok(&start(p, "feed-a.toml", "repaired", "2030-01-01T00:02:00Z"));
    let landed = rows();
    assert_eq!(landed.len(), 3);
    assert!(landed.iter().any(|row| row["id"] == "two" && row["score"] == 2));
    assert_eq!(std::fs::read_to_string(p.join("called")).unwrap(), "pull-0\npull-0\npull-1\npull-0\npull-1\n");
}

/// An in-flight copy of run `from`, recorded as `run_id` under `pipeline`.
fn put_running(dir: &Path, from: &str, run_id: &str, pipeline: &str) {
    use contextful_core::coordinate::Catalog;
    let clock = std::sync::Arc::new(contextful_core::ports::FixedClock(contextful_core::time::Instant::parse("2030-01-01T00:00:00Z").unwrap()));
    let catalog = contextful_sqlite::MachineCatalog::open(&dir.join(".contextful/context/research/machine.sqlite"), clock).unwrap();
    let mut row = catalog.run(from).unwrap().expect("the source row");
    row.run_id = run_id.into();
    row.pipeline_id = pipeline.into();
    row.status = contextful_core::run::record::RunStatus::Running;
    row.ended_at = None;
    row.stop = None;
    catalog.put_run(&row).unwrap();
}

fn stop_of(dir: &Path, run_id: &str) -> Option<contextful_core::run::record::StopMark> {
    use contextful_core::coordinate::Catalog;
    let clock = std::sync::Arc::new(contextful_core::ports::FixedClock(contextful_core::time::Instant::parse("2030-01-01T00:00:00Z").unwrap()));
    let catalog = contextful_sqlite::MachineCatalog::open(&dir.join(".contextful/context/research/machine.sqlite"), clock).unwrap();
    catalog.run(run_id).unwrap().expect("the run row").stop
}

/// A credentialed `run cancel` needs `execute` over the pipeline its run record holds, and its refusal names no
/// pipeline; an unrecorded run refuses alike.
#[test]
fn a_credentialed_cancel_gates_on_execute_over_the_recorded_pipeline() {
    let dir = project();
    let public = issuer(dir.path());
    ok(&start(dir.path(), "feed-a.toml", "a1", "2030-01-01T00:00:00Z"));
    put_running(dir.path(), "a1", "live-a", "feed-a");
    put_running(dir.path(), "a1", "live-b", "feed-b");
    let execute = ok(&cf(dir.path(), &["token", "mint", "--on-behalf-of", "user://dana@acme.example", "--action", "execute", "--table", "feed-a"]));

    let out = credentialed(dir.path(), &execute, &public, &["run", "cancel", "live-b"]);
    refused(&out, "CancelUnauthorized");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("feed-b"), "the refusal names no pipeline: {stderr}");
    assert!(stop_of(dir.path(), "live-b").is_none());
    let absent = credentialed(dir.path(), &execute, &public, &["run", "cancel", "zz"]);
    refused(&absent, "CancelUnauthorized");
    assert_eq!(String::from_utf8_lossy(&absent.stderr), stderr);
    // A read grant over the pipeline confers no stop.
    refused(&credentialed(dir.path(), &reader(dir.path(), "feed-a"), &public, &["run", "cancel", "live-a"]), "CancelUnauthorized");
    assert!(stop_of(dir.path(), "live-a").is_none());

    let stopped = ok(&credentialed(dir.path(), &execute, &public, &["run", "cancel", "live-a", "--reason", "ops"]));
    assert!(stopped.contains("live-a: stop requested"), "{stopped}");
    assert_eq!(stop_of(dir.path(), "live-a").unwrap().reason.as_deref(), Some("ops"));
    // The local owner, presenting no credential, stops as before.
    ok(&cf(dir.path(), &["run", "cancel", "live-b", "--project", "research"]));
}

/// Pull journaling defaults on. A source whose pre-pull cursor cannot name the content it reads opts out through one
/// constant, pinned against each source's declaration by a test. An empty pull is never journaled.
// spec: run.journal.opt-out@4367ed31
#[test]
fn each_built_in_source_journals_unless_the_one_constant_lists_it_and_an_empty_pull_records_nothing() {
    use contextful_core::pipeline::declare::{PipelineSpec, UNJOURNALED_SOURCES};
    use contextful_core::run::journal::Row;
    use contextful_core::run::ports::JournalStore;
    for source in contextful_connectors::BUILT_IN {
        let spec: PipelineSpec = serde_json::from_value(serde_json::json!({ "id": "p", "tables": ["t"], "source": { "name": source, "config": {} } })).unwrap();
        let listed = UNJOURNALED_SOURCES.iter().any(|(s, _)| *s == source);
        assert_eq!(spec.journals(), !listed, "source `{source}`");
    }
    assert!(UNJOURNALED_SOURCES.iter().all(|(s, _)| contextful_connectors::BUILT_IN.contains(s)), "the constant names only declared sources");

    // A pull with rows records; an empty pull beside it records nothing.
    let dir = project();
    std::fs::write(
        dir.path().join("empty.sh"),
        "case \"$CONTEXTFUL_STEP\" in pull-0) printf '{\"rows\":[{\"id\":\"a\"}],\"cursor\":\"c1\",\"more\":true}';; pull-1) printf '{\"rows\":[],\"cursor\":\"c2\",\"more\":true}';; *) printf '{\"error\":{\"tag\":\"Permanent\",\"message\":\"feed paused\"}}'; exit 1;; esac\n",
    )
    .unwrap();
    let failed = start(dir.path(), "feed-a.toml", "a1", "2030-01-01T00:00:00Z");
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("feed paused"), "{}", String::from_utf8_lossy(&failed.stderr));
    let listing: serde_json::Value = serde_json::from_str(&ok(&history(dir.path(), &[]))).unwrap();
    let execution = listing["runs"][0]["execution_id"].as_str().unwrap();
    let journal = contextful_engine::stores::FileJournalStore::open(&dir.path().join(".contextful/run/research"));
    let labels: Vec<String> = journal.rows(execution).unwrap().into_iter().filter_map(|row| match row {
        Row::Recorded { key, .. } => Some(key.step_label),
        _ => None,
    }).collect();
    assert_eq!(labels, ["pull-0"], "journaling defaults on, and the empty pull-1 records nothing");
}
