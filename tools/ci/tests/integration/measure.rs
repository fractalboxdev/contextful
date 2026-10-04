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

/// An entry whose owning clause, method or metric path resolves to nothing raises `MeasureEntryUnresolved` before any measure runs.
// spec: assurance.measure.unresolved-entry@60be6252
#[test]
fn an_entry_naming_no_clause_refuses_before_any_test_runs() {
    let ledger = entry("demo-doubles", "run.journal.entry-keys", "{ test = \"demo::measured::doubles_measured\" }", "target = { op = \"==\", value = 4 }")
        + &entry("demo-missing", "run.journal.entry-key", "{ test = \"demo::measured::doubles_nowhere\" }", "target = { op = \"==\", value = 4 }");
    let r = repo(&ledger);
    let o = r.gate(&["--stage", "evaluate"]);
    assert!(!o.status.success());
    let err = stderr(&o);
    assert!(err.contains("MeasureEntryUnresolved: `demo-doubles`: clause `run.journal.entry-keys`"), "{err}");
    assert!(err.contains("MeasureEntryUnresolved: `demo-missing`: test `demo::measured::doubles_nowhere`"), "{err}");
    assert!(!r.root.join("target/evaluate/records/ran.marker").exists(), "a test ran: {err}");
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

/// A record whose seed differs from its entry's declared seed raises `MeasureSeedMismatch`, and the entry counts as red.
// spec: assurance.measure.seed-mismatch@4f090e30
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
