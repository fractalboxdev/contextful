//! `assurance.measure`: the target ledger resolved, run and rendered by the evaluate stage.

use crate::{stderr, Repo};
use std::process::{Command, Output};

const LOCK: &str = "{\"clauses\": [{\"id\": \"run.journal.entry-key\"}]}\n";

/// A demo test that records its figure under a measure run, and one that records nothing.
/// The first also leaves a marker, so a suite can tell whether any test ran.
const MEASURED: &str = r#"
#[test]
fn doubles_measured() {
    let dir = std::path::PathBuf::from(std::env::var_os("CONTEXTFUL_MEASURE_DIR").expect("a measure run"));
    std::fs::write(dir.join("ran.marker"), "").unwrap();
    let record = format!(
        "{{\"id\": \"demo-doubles\", \"value\": {}, \"n\": 1, \"seed\": 7, \"run\": {{\"processor\": \"t\", \"nproc\": 1, \"memory_limit\": null}}}}",
        demo::double(2)
    );
    std::fs::write(dir.join("demo-doubles.json"), record).unwrap();
}

#[test]
fn doubles_silently() {
    assert_eq!(demo::double(3), 6);
}

#[test]
fn doubles_panics() {
    panic!("trend method failed");
}

#[test]
fn doubles_twice() {
    let dir = std::path::PathBuf::from(std::env::var_os("CONTEXTFUL_MEASURE_DIR").expect("a measure run"));
    let count = dir.join("invocations");
    let n = std::fs::read_to_string(&count).unwrap_or_default().parse::<u64>().unwrap_or(0) + 1;
    std::fs::write(&count, n.to_string()).unwrap();
    for id in ["demo-doubles", "demo-extra"] {
        let record = format!(
            "{{\"id\": \"{id}\", \"value\": 4, \"n\": 1, \"seed\": 7, \"run\": {{\"processor\": \"t\", \"nproc\": 1, \"memory_limit\": null}}}}"
        );
        std::fs::write(dir.join(format!("{id}.json")), record).unwrap();
    }
}
"#;

fn entry(id: &str, clause: &str, method: &str, target: &str) -> String {
    format!("[entry.{id}]\nclause = \"{clause}\"\nmetric = \"doubled.value\"\nkind = \"test\"\ntier = \"gate\"\nmethod = {method}\n{target}\n")
}

fn repo(ledger: &str) -> Repo {
    let r = Repo::init();
    r.write("spec/spec.lock.json", LOCK);
    r.write("crates/demo/tests/integration/main.rs", "mod double;\nmod measured;\n");
    r.write("crates/demo/tests/integration/measured.rs", MEASURED);
    r.write("evals/ledger.toml", ledger);
    r.commit("ledger");
    r
}

fn measure(r: &Repo, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .arg("measure")
        .args(args)
        .current_dir(&r.root)
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap()
}

#[test]
fn inherited_targets_hold_measure_builds_outside_the_checkout_and_records_at_a_stable_address() {
    let r = repo(&entry("demo-doubles", "run.journal.entry-key", "{ test = \"demo::measured::doubles_measured\" }", "target = { op = \">=\", value = 5 }"));
    let pool = tempfile::tempdir().unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .args(["gate", "--stage", "evaluate"])
        .current_dir(&r.root)
        .env("CARGO_TARGET_DIR", pool.path())
        .output().unwrap();
    assert!(!o.status.success());
    assert!(stderr(&o).contains("demo-doubles = 4 against >= 5"), "{}", stderr(&o));
    assert!(pool.path().join("contextful-ci/evaluate/debug").exists());
    assert!(r.root.join("target/evaluate/records/demo-doubles.json").exists());
    assert!(!r.root.join("target/evaluate/debug").exists());
}

/// An entry whose owning clause, method or metric path resolves to nothing raises `MeasureEntryUnresolved` before any measure runs.
// spec: assurance.measure.unresolved-entry@60be6252
#[test]
fn an_entry_naming_no_clause_refuses_before_any_test_runs() {
    let ledger = entry("demo-doubles", "run.journal.entry-keys", "{ test = \"demo::measured::doubles_measured\" }", "target = { op = \"==\", value = 4 }")
        + &entry("demo-missing", "run.journal.entry-key", "{ test = \"demo::measured::doubles_nowhere\" }", "target = { op = \"==\", value = 4 }");
    let r = repo(&ledger);
    r.write("target/evaluate/records/old.json", "stale");
    let o = measure(&r, &[]);
    assert!(!o.status.success());
    let err = stderr(&o);
    assert!(err.contains("MeasureEntryUnresolved: `demo-doubles`: clause `run.journal.entry-keys`"), "{err}");
    assert!(err.contains("MeasureEntryUnresolved: `demo-missing`: test `demo::measured::doubles_nowhere`"), "{err}");
    assert!(!r.root.join("target/evaluate/records/ran.marker").exists(), "a test ran: {err}");
    assert!(!r.root.join("target/evaluate/records/old.json").exists(), "a stale measure record survived: {err}");
    assert!(!err.contains("Running"), "{err}");
}

/// A measure writes one JSON record carrying its entry id, value, sample count, seed and run stamp; a gate-tier method finishing without one raises `MeasureRecordMissing`.
// spec: assurance.measure.record@34ac5fa6
#[test]
fn a_gate_method_writing_no_record_is_refused() {
    let r = repo(&entry("demo-doubles", "run.journal.entry-key", "{ test = \"demo::measured::doubles_silently\" }", "target = { op = \"==\", value = 4 }"));
    let o = r.gate(&["--stage", "evaluate"]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("MeasureRecordMissing: `demo-doubles`"), "{}", stderr(&o));
}

/// A native benchmark generated from a real corpus is the red or green gate; public benchmark sets run beside it as held-out comparison and decide nothing.
// spec: assurance.baseline.native-gate@b7278bc1
#[test]
fn the_native_set_gates_and_a_public_set_decides_nothing() {
    let method = "{ test = \"demo::measured::doubles_measured\" }";
    let r = repo(&entry("demo-doubles", "run.journal.entry-key", method, "target = { op = \"==\", value = 4 }"));
    r.write("spec/spec.lock.json", "{\"clauses\": [{\"id\": \"run.journal.entry-key\"}, {\"id\": \"assurance.baseline.native-gate\"}]}\n");
    r.write("evals/cases/native.jsonl", "{}\n");
    r.commit("a native set no entry runs");
    let o = r.gate(&["--stage", "evaluate"]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("no gate-tier ledger entry owned by `assurance.baseline.native-gate` runs it"), "{}", stderr(&o));

    // The native set's entry decides the stage, and a public set at the trend tier sits beside it.
    let public = "[entry.public-recall]\nclause = \"assurance.baseline.native-gate\"\nmetric = \"retrieval.hybrid.recall_at_k\"\nkind = \"eval\"\ntier = \"trend\"\ndirection = \"higher_is_better\"\nmethod = { cases = \"evals/cases/public.jsonl\" }\n";
    r.write("evals/cases/public.jsonl", "{}\n");
    r.write("evals/ledger.toml", &(entry("demo-doubles", "assurance.baseline.native-gate", method, "target = { op = \"==\", value = 4 }") + public));
    r.commit("the native set gates");
    let o = r.gate(&["--stage", "evaluate"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stderr(&o).contains("baseline verdict held for evals/cases/native.jsonl (demo-doubles)"), "{}", stderr(&o));
    assert!(!stderr(&o).contains("public-recall"), "a public set decided the stage: {}", stderr(&o));

    // A red native figure reds the stage.
    r.write("evals/ledger.toml", &(entry("demo-doubles", "assurance.baseline.native-gate", method, "target = { op = \">=\", value = 5 }") + public));
    r.commit("a native figure below its target");
    let o = r.gate(&["--stage", "evaluate"]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("baseline verdict red for evals/cases/native.jsonl"), "{}", stderr(&o));
}

#[test]
fn a_held_target_passes_and_a_missed_one_reds_the_stage() {
    let r = repo(&entry("demo-doubles", "run.journal.entry-key", "{ test = \"demo::measured::doubles_measured\" }", "target = { op = \"==\", value = 4 }"));
    let o = r.gate(&["--stage", "evaluate"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let err = stderr(&o);
    assert!(err.contains("measure: demo-doubles = 4 (n = 1, seed = 7), target == 4: holds"), "{err}");
    assert!(!r.root.join("target/evaluate").exists(), "the stage reclaims its build directory");

    r.write("evals/ledger.toml", &entry("demo-doubles", "run.journal.entry-key", "{ test = \"demo::measured::doubles_measured\" }", "target = { op = \">=\", value = 5 }"));
    r.commit("a tighter target");
    let o = r.gate(&["--stage", "evaluate"]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("demo-doubles = 4 against >= 5"), "{}", stderr(&o));
}

#[test]
fn one_test_measures_two_ledger_entries_in_one_invocation() {
    let method = "{ test = \"demo::measured::doubles_twice\" }";
    let ledger = entry("demo-doubles", "run.journal.entry-key", method, "target = { op = \"==\", value = 4 }")
        + &entry("demo-extra", "run.journal.entry-key", method, "target = { op = \"==\", value = 4 }");
    let r = repo(&ledger);
    let o = measure(&r, &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    let count = std::fs::read_to_string(r.root.join("target/evaluate/records/invocations")).unwrap();
    assert_eq!(count, "1", "one test emits both figures");
    assert!(stderr(&o).contains("2 held, 0 red, 0 unrecorded"), "{}", stderr(&o));
}

/// A record whose seed differs from its entry's declared seed raises `MeasureSeedMismatch`, and the entry counts as red.
// spec: assurance.measure.seed-mismatch@d5ef949b
#[test]
fn a_record_under_another_seed_than_its_entry_declares_is_refused() {
    let held = entry("demo-doubles", "run.journal.entry-key", "{ test = \"demo::measured::doubles_measured\" }", "target = { op = \"==\", value = 4 }\nseed = 7");
    let r = repo(&held);
    let o = r.gate(&["--stage", "evaluate"]);
    assert!(o.status.success(), "{}", stderr(&o));

    r.write("evals/ledger.toml", &held.replace("seed = 7", "seed = 8"));
    r.commit("another seed");
    let o = r.gate(&["--stage", "evaluate"]);
    assert!(!o.status.success());
    let err = stderr(&o);
    assert!(err.contains("MeasureSeedMismatch: `demo-doubles` recorded seed 7, the ledger declares 8"), "{err}");
    assert!(!err.contains("target == 4: holds"), "a figure under another seed holds nothing: {err}");
}

/// An entry naming an issue in place of a method reports open and gates nothing, and `evals/ledger.md` carries every entry's computed status.
// spec: assurance.measure.open-entry@87391660
#[test]
fn an_issue_entry_is_listed_open_and_gates_nothing() {
    let r = repo(&entry("deep-recall", "run.journal.entry-key", "{ issue = 43 }", ""));
    let o = r.gate(&["--stage", "evaluate"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stderr(&o).contains("0 held, 0 red, 0 unrecorded, 1 open"), "{}", stderr(&o));

    let o = measure(&r, &["--status"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let md = std::fs::read_to_string(r.root.join("evals/ledger.md")).unwrap();
    assert!(md.contains("| `deep-recall` | `run.journal.entry-key` | `doubled.value` | gate | issue 43 | — | open (issue 43) |"), "{md}");
    assert!(measure(&r, &["--status", "--check"]).status.success());

    std::fs::write(r.root.join("evals/ledger.md"), md.replace("open (issue 43)", "gated")).unwrap();
    let o = measure(&r, &["--status", "--check"]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("evals/ledger.md differs"), "{}", stderr(&o));
}

/// A probe method runs its named binary in a fresh process and reads its record.
#[test]
fn a_probe_binary_records_a_gate_figure() {
    let r = repo(&entry("demo-doubles", "run.journal.entry-key", "{ probe = \"demo-probe\" }", "target = { op = \"==\", value = 4 }"));
    r.write("Cargo.toml", "[workspace]\nresolver = \"2\"\nmembers = [\"crates/*\", \"tools/*\"]\n");
    r.write("tools/probe/Cargo.toml", "[package]\nname = \"demo-probes\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[[bin]]\nname = \"demo-probe\"\npath = \"src/main.rs\"\n");
    r.write("tools/probe/src/main.rs", r##"fn main() {
    let dir = std::path::PathBuf::from(std::env::var_os("CONTEXTFUL_MEASURE_DIR").unwrap());
    std::fs::write(dir.join("demo-doubles.json"),
        r#"{"id":"demo-doubles","value":4,"n":1,"seed":7,"run":{"processor":"t","nproc":1,"memory_limit":null}}"#).unwrap();
}

"##);
    r.lock();
    let o = measure(&r, &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stderr(&o).contains("demo-doubles = 4"), "{}", stderr(&o));
}

// spec: assurance.measure.trend-baseline@e59a4889
#[test]
fn a_trend_report_uses_the_latest_successful_matching_history_without_failing_the_run() {
    let r = Repo::init();
    r.write("evals/ledger.toml", "[entry.latency]\nclause = \"run.journal.entry-key\"\nmetric = \"run.latency_ms\"\nkind = \"bench\"\ntier = \"trend\"\ndirection = \"lower_is_better\"\nmethod = { issue = 81 }\n");
    let old = r.head();
    let older = r#"{"id":"latency","value":30,"n":200,"seed":7,"run":{"processor":"t","nproc":1,"memory_limit":null}}"#;
    let baseline = r#"{"id":"latency","value":40,"n":200,"seed":7,"run":{"processor":"t","nproc":1,"memory_limit":null}}"#;
    let failed = r#"{"id":"latency","value":100,"n":200,"seed":7,"run":{"processor":"t","nproc":1,"memory_limit":null}}"#;
    r.git(&["notes", "--ref=measures", "add", "-m", &format!("{{\"commit\":\"{old}\",\"run_id\":1,\"run_attempt\":1,\"exit_code\":0,\"records\":[{older}]}}"), &old]);
    r.git(&["notes", "--ref=measures", "append", "-m", &format!("{{\"commit\":\"{old}\",\"run_id\":2,\"run_attempt\":1,\"exit_code\":0,\"records\":[{baseline}]}}"), &old]);
    r.git(&["notes", "--ref=measures", "append", "-m", &format!("{{\"commit\":\"{old}\",\"run_id\":3,\"run_attempt\":1,\"exit_code\":1,\"records\":[{failed}]}}"), &old]);
    r.write("target/evaluate/records/latency.json", &baseline.replace("\"value\":40", "\"value\":52"));
    r.write("README", "next commit\n");
    let current = r.commit("next");
    r.git(&["notes", "--ref=measures", "add", "-m", &format!("{{\"commit\":\"{current}\",\"run_id\":99,\"run_attempt\":1,\"exit_code\":0,\"records\":[{failed}]}}"), &current]);
    let o = r.run_ci(&["measure-report", "--commit", &current, "--run-id", "4", "--run-attempt", "1", "--exit-code", "0", "--out", "target/measure-report.json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let report: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(r.root.join("target/measure-report.json")).unwrap()).unwrap();
    assert_eq!(report["records"][0]["value"], 52.0);
    assert_eq!(report["annotations"][0]["id"], "latency");
    assert_eq!(report["annotations"][0]["baseline"], 40.0);
    assert_eq!(report["annotations"][0]["baseline_run_id"], 2);
    assert_eq!(report["annotations"][0]["worse_percent"], 30.0);
    assert_eq!(report["exit_code"], 0);

    r.write("target/evaluate/records/latency.json", &baseline.replace("\"value\":40", "\"value\":52").replace("\"nproc\":1", "\"nproc\":2"));
    let o = r.run_ci(&["measure-report", "--commit", &current, "--run-id", "5", "--run-attempt", "1", "--exit-code", "0", "--out", "target/measure-report.json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let report: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(r.root.join("target/measure-report.json")).unwrap()).unwrap();
    assert_eq!(report["annotations"].as_array().unwrap().len(), 0);

    r.write("target/evaluate/records/latency.json", &baseline.replace("\"value\":40", "\"value\":52").replace("\"seed\":7", "\"seed\":8"));
    let o = r.run_ci(&["measure-report", "--commit", &current, "--run-id", "6", "--run-attempt", "1", "--exit-code", "0", "--out", "target/measure-report.json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let report: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(r.root.join("target/measure-report.json")).unwrap()).unwrap();
    assert_eq!(report["annotations"].as_array().unwrap().len(), 0);

    r.git(&["update-ref", "-d", "refs/notes/measures"]);
    let o = r.run_ci(&["measure-report", "--commit", &current, "--run-id", "7", "--run-attempt", "1", "--exit-code", "0", "--out", "target/measure-report.json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let report: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(r.root.join("target/measure-report.json")).unwrap()).unwrap();
    assert_eq!(report["annotations"].as_array().unwrap().len(), 0);
}

#[test]
fn a_zero_baseline_report_remains_publishable() {
    let r = Repo::init();
    r.write("evals/ledger.toml", "[entry.latency]\nclause = \"run.journal.entry-key\"\nmetric = \"run.latency_ms\"\nkind = \"bench\"\ntier = \"trend\"\ndirection = \"lower_is_better\"\nmethod = { issue = 81 }\n");
    let old = r.head();
    let baseline = r#"{"id":"latency","value":0,"n":1,"seed":7,"run":{"processor":"t","nproc":1,"memory_limit":null}}"#;
    r.git(&["notes", "--ref=measures", "add", "-m", &format!("{{\"commit\":\"{old}\",\"run_id\":1,\"run_attempt\":1,\"exit_code\":0,\"records\":[{baseline}]}}"), &old]);
    r.write("README", "next commit\n");
    let current = r.commit("next");
    r.write("target/evaluate/records/latency.json", &baseline.replace("\"value\":0", "\"value\":1"));
    let out = r.run_ci(&["measure-report", "--commit", &current, "--run-id", "2", "--run-attempt", "1", "--exit-code", "0", "--out", "target/measure-report.json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let report: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(r.root.join("target/measure-report.json")).unwrap()).unwrap();
    assert_eq!(report["annotations"][0]["annotation"], "worse than the zero baseline");
    assert!(report["annotations"][0]["worse_percent"].is_null());

    let remote = tempfile::tempdir().unwrap();
    r.git(&["init", "--bare", remote.path().to_str().unwrap()]);
    r.git(&["remote", "add", "origin", remote.path().to_str().unwrap()]);
    r.git(&["push", "origin", "HEAD:refs/heads/main"]);
    r.git(&["push", "origin", "refs/notes/measures"]);
    let out = r.run_ci(&["measure-publish", "--commit", &current, "--report", "target/measure-report.json", "--remote", "origin"]);
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn a_report_reads_noted_history_with_bounded_git_processes() {
    let r = Repo::init();
    for n in 1..=4 {
        r.write("README", &format!("history {n}\n"));
        let commit = r.commit(&format!("history {n}"));
        r.git(&["notes", "--ref=measures", "add", "-m", &format!("{{\"commit\":\"{commit}\",\"run_id\":{n},\"run_attempt\":1,\"exit_code\":0,\"records\":[]}}"), &commit]);
    }
    r.write("README", "current\n");
    let current = r.commit("current");
    let bin = tempfile::tempdir().unwrap();
    let git = bin.path().join("git");
    std::fs::write(&git, "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$GIT_CALLS\"\nexec \"$REAL_GIT\" \"$@\"\n").unwrap();
    std::fs::set_permissions(&git, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("git"))
        .find(|path| path.is_file())
        .unwrap();
    let calls = bin.path().join("calls");
    let path = format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap());
    let out = Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .args(["measure-report", "--commit", &current, "--run-id", "5", "--run-attempt", "1", "--exit-code", "0", "--out", "target/measure-report.json"])
        .current_dir(&r.root)
        .env("PATH", path)
        .env("REAL_GIT", real_git)
        .env("GIT_CALLS", &calls)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let count = std::fs::read_to_string(calls).unwrap().lines().count();
    assert!(count <= 4, "{count} Git processes for four noted ancestors");
}

#[test]
fn a_trend_report_stops_before_older_unneeded_notes() {
    let r = Repo::init();
    r.write("evals/ledger.toml", "[entry.latency]\nclause = \"run.journal.entry-key\"\nmetric = \"run.latency_ms\"\nkind = \"bench\"\ntier = \"trend\"\ndirection = \"lower_is_better\"\nmethod = { issue = 81 }\n");
    let oldest = r.head();
    r.git(&["notes", "--ref=measures", "add", "-m", "malformed older report", &oldest]);
    r.write("README", "baseline\n");
    let baseline = r.commit("baseline");
    let figure = r#"{"id":"latency","value":40,"n":200,"seed":7,"run":{"processor":"t","nproc":1,"memory_limit":null}}"#;
    r.git(&["notes", "--ref=measures", "add", "-m", &format!("{{\"commit\":\"{baseline}\",\"run_id\":2,\"run_attempt\":1,\"exit_code\":0,\"records\":[{figure}]}}"), &baseline]);
    r.write("README", "current\n");
    let current = r.commit("current");
    r.write("target/evaluate/records/latency.json", &figure.replace("\"value\":40", "\"value\":52"));
    let out = r.run_ci(&["measure-report", "--commit", &current, "--run-id", "3", "--run-attempt", "1", "--exit-code", "0", "--out", "target/measure-report.json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let report: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(r.root.join("target/measure-report.json")).unwrap()).unwrap();
    assert_eq!(report["annotations"][0]["baseline_run_id"], 2);
}

#[test]
fn publishing_a_report_keeps_notes_added_by_another_writer() {
    let r = Repo::init();
    let commit = r.head();
    let remote = tempfile::tempdir().unwrap();
    r.git(&["init", "--bare", remote.path().to_str().unwrap()]);
    r.git(&["remote", "add", "origin", remote.path().to_str().unwrap()]);
    r.git(&["push", "origin", "HEAD:refs/heads/main"]);
    r.git(&["notes", "--ref=measures", "add", "-m", "original report", &commit]);
    r.git(&["push", "origin", "refs/notes/measures"]);
    let external = Command::new("git")
        .args(["--git-dir", remote.path().to_str().unwrap(), "-c", "user.name=t", "-c", "user.email=t@example.com", "notes", "--ref=measures", "append", "-m", "external report", &commit])
        .output()
        .unwrap();
    assert!(external.status.success(), "{}", String::from_utf8_lossy(&external.stderr));
    let report = format!("{{\"commit\":\"{commit}\",\"run_id\":3,\"run_attempt\":1,\"exit_code\":0,\"records\":[]}}\n");
    r.write("target/measure-report.json", &report);
    let out = r.run_ci(&["measure-publish", "--commit", &commit, "--report", "target/measure-report.json", "--remote", "origin"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let published = Command::new("git")
        .args(["--git-dir", remote.path().to_str().unwrap(), "notes", "--ref=measures", "show", &commit])
        .output()
        .unwrap();
    assert!(published.status.success(), "{}", String::from_utf8_lossy(&published.stderr));
    let note = String::from_utf8_lossy(&published.stdout);
    assert!(note.contains("original report") && note.contains("external report") && note.contains(&report), "{note}");
}

#[test]
fn publishing_a_report_retries_a_competing_push() {
    let r = Repo::init();
    let commit = r.head();
    let remote = tempfile::tempdir().unwrap();
    r.git(&["init", "--bare", remote.path().to_str().unwrap()]);
    r.git(&["remote", "add", "origin", remote.path().to_str().unwrap()]);
    r.git(&["push", "origin", "HEAD:refs/heads/main"]);
    let report = format!("{{\"commit\":\"{commit}\",\"run_id\":4,\"run_attempt\":1,\"exit_code\":0,\"records\":[]}}\n");
    r.write("target/measure-report.json", &report);
    let bin = tempfile::tempdir().unwrap();
    let git = bin.path().join("git");
    std::fs::write(&git, "#!/bin/sh\nif [ \"$1\" = push ] && [ ! -e \"$RACE_MARKER\" ]; then\n  : > \"$RACE_MARKER\"\n  \"$REAL_GIT\" --git-dir=\"$RACE_REMOTE\" -c user.name=t -c user.email=t@example.com notes --ref=measures add -m 'racer report' \"$RACE_COMMIT\" || exit $?\nfi\nexec \"$REAL_GIT\" \"$@\"\n").unwrap();
    std::fs::set_permissions(&git, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("git"))
        .find(|path| path.is_file())
        .unwrap();
    let path = format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap());
    let out = Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .args(["measure-publish", "--commit", &commit, "--report", "target/measure-report.json", "--remote", "origin"])
        .current_dir(&r.root)
        .env("PATH", path)
        .env("REAL_GIT", real_git)
        .env("RACE_REMOTE", remote.path())
        .env("RACE_COMMIT", &commit)
        .env("RACE_MARKER", bin.path().join("raced"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let published = Command::new("git")
        .args(["--git-dir", remote.path().to_str().unwrap(), "notes", "--ref=measures", "show", &commit])
        .output()
        .unwrap();
    assert!(published.status.success(), "{}", String::from_utf8_lossy(&published.stderr));
    let note = String::from_utf8_lossy(&published.stdout);
    assert!(note.contains("racer report") && note.contains(&report), "{note}");
}

// spec: assurance.measure.tier@0feb15e4
#[test]
fn a_failed_or_reseeded_trend_method_does_not_fail_the_measure_run() {
    let trend = entry("demo-doubles", "run.journal.entry-key", "{ test = \"demo::measured::doubles_measured\" }", "seed = 8")
        .replace("tier = \"gate\"", "tier = \"trend\"\ndirection = \"lower_is_better\"");
    let r = repo(&trend);
    let o = measure(&r, &["--tier", "trend"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stderr(&o).contains("MeasureSeedMismatch"), "{}", stderr(&o));

    r.write("evals/ledger.toml", &trend.replace("doubles_measured", "doubles_panics"));
    let o = measure(&r, &["--tier", "trend"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stderr(&o).contains("trend method failed") || stderr(&o).contains("failed"), "{}", stderr(&o));
}
