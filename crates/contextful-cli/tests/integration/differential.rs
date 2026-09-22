//! `contextful formal differential` over the Lean reference binary and over stand-in
//! reference executables.
//!
//! A stand-in is a shell script around `contextful formal differential --decide`, which
//! prints the engine's own decision for one case: it agrees with the engine everywhere,
//! and a `sed` over its output corrupts it into a reference that disagrees on a known
//! class of case. Tests reaching the Lean reference skip when `lake` is absent, unless
//! `CONTEXTFUL_REQUIRE_LEAN` is set, in which case they fail.

use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;

const BIN: &str = env!("CARGO_BIN_EXE_contextful");

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn reference_root() -> PathBuf {
    repo().join("formal/reference")
}

/// Build the Lean reference package once per test process; `None` when `lake` is absent.
fn reference_exe() -> Option<PathBuf> {
    static BUILT: OnceLock<Option<PathBuf>> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let root = reference_root();
            let out = Command::new("lake").arg("build").current_dir(&root).output().ok()?;
            assert!(
                out.status.success(),
                "lake build failed in {}:\n{}{}",
                root.display(),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            Some(root.join(".lake/build/bin/contextful-reference"))
        })
        .clone()
}

macro_rules! lean_or_skip {
    () => {
        match reference_exe() {
            Some(exe) => exe,
            None => {
                assert!(
                    std::env::var_os("CONTEXTFUL_REQUIRE_LEAN").is_none(),
                    "CONTEXTFUL_REQUIRE_LEAN is set and `lake` is not on PATH"
                );
                eprintln!("skipped: no `lake` on PATH");
                return;
            }
        }
    };
}

/// A scratch directory holding stand-in reference scripts and a corpus path.
struct Scratch {
    dir: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Scratch {
        Scratch { dir: tempfile::tempdir().unwrap() }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn script(&self, name: &str, body: &str) -> PathBuf {
        let path = self.path(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// A reference that agrees with the engine on every case.
    fn agreeing(&self) -> PathBuf {
        self.script("agreeing.sh", &format!("exec '{BIN}' formal differential --decide"))
    }

    /// A reference that reports every covered pattern as not covered.
    fn corrupted(&self) -> PathBuf {
        self.script(
            "corrupted.sh",
            &format!("'{BIN}' formal differential --decide | sed 's/\"verdict\":\"covered\"/\"verdict\":\"not_covered\"/'"),
        )
    }

    /// An agreeing reference appending every case it receives to `log`.
    fn logging(&self, log: &Path) -> PathBuf {
        self.script("logging.sh", &format!("tee -a '{}' | '{BIN}' formal differential --decide", log.display()))
    }

    fn corpus(&self) -> PathBuf {
        self.path("corpus.jsonl")
    }

    fn write_corpus(&self, entries: &[Value]) {
        let text: String = entries.iter().map(|e| format!("{e}\n")).collect();
        std::fs::write(self.corpus(), text).unwrap();
    }

    fn read_corpus(&self) -> Vec<Value> {
        read_jsonl(&self.corpus())
    }

    fn run(&self, reference: &Path, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(["formal", "differential", "--reference"])
            .arg(reference)
            .arg("--corpus")
            .arg(self.corpus())
            .args(args)
            .output()
            .unwrap()
    }
}

fn read_jsonl(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn passed(out: &Output) -> String {
    assert!(out.status.success(), "expected success\nstdout:\n{}\nstderr:\n{}", stdout(out), stderr(out));
    stdout(out)
}

fn refused(out: &Output, error: &str) -> String {
    assert!(!out.status.success(), "expected {error}, got success:\n{}", stdout(out));
    let err = stderr(out);
    assert!(err.starts_with(&format!("{error}:")), "expected {error}, got:\n{err}");
    err
}

/// Feed one case to an executable and read back its decision.
fn decide(exe: &Path, args: &[&str], case: &Value) -> Value {
    use std::io::Write;
    let mut child =
        Command::new(exe).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(format!("{case}\n").as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{} failed on {case}", exe.display());
    serde_json::from_slice(&out.stdout).unwrap()
}

fn engine(case: &Value) -> Value {
    decide(Path::new(BIN), &["formal", "differential", "--decide"], case)
}

fn line_value<'a>(report: &'a str, key: &str) -> &'a str {
    report
        .lines()
        .find_map(|l| l.strip_prefix(key))
        .unwrap_or_else(|| panic!("report has no `{key}` line:\n{report}"))
        .trim()
}

/// Every case with one object key or one array element removed, at any depth.
fn removals(v: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    match v {
        Value::Object(map) => {
            for key in map.keys() {
                let mut m = map.clone();
                m.remove(key);
                out.push(Value::Object(m));
            }
            for (key, child) in map {
                for r in removals(child) {
                    let mut m = map.clone();
                    m.insert(key.clone(), r);
                    out.push(Value::Object(m));
                }
            }
        }
        Value::Array(items) => {
            for i in 0..items.len() {
                let mut a = items.clone();
                a.remove(i);
                out.push(Value::Array(a));
            }
            for (i, child) in items.iter().enumerate() {
                for r in removals(child) {
                    let mut a = items.clone();
                    a[i] = r;
                    out.push(Value::Array(a));
                }
            }
        }
        _ => {}
    }
    out
}

fn agreeing_entry(n: usize) -> Value {
    let case = json!({"op": "covers_name", "pattern": format!("t{n}"), "name": format!("u{n}")});
    let decision = json!({"verdict": "not_covered", "error": null, "dimension": null});
    json!({"case": case, "engine": decision, "reference": decision})
}

/// The reference model is an executable Lean rendering of the decision functions, built to a binary that reads one case on standard input and prints one decision.
// spec: assurance.differential-test.reference-model@837f2377
#[test]
fn the_reference_binary_reads_one_case_and_prints_one_decision() {
    let exe = lean_or_skip!();
    let covered = |pattern: &str, name: &str| {
        let d = decide(&exe, &[], &json!({"op": "covers_name", "pattern": pattern, "name": name}));
        d["verdict"] == "covered"
    };
    // The coverage table in the authority contract's Shapes section.
    let rows = [
        ("research", [false, false, true]),
        ("research/filings", [true, true, true]),
        ("research/filings/eu", [true, false, true]),
        ("sales/invoices", [false, false, true]),
    ];
    for (name, expected) in rows {
        for (pattern, want) in ["research/*", "research/filings", "*"].into_iter().zip(expected) {
            assert_eq!(covered(pattern, name), want, "{pattern} over {name}");
        }
    }
    let widen = json!({"op": "narrow",
        "parent": [{"actions": ["read"], "tables": ["research/*"]}],
        "child": [{"actions": ["read", "write"], "tables": ["research/filings"]}]});
    assert_eq!(
        decide(&exe, &[], &widen),
        json!({"verdict": "refused", "error": "AttenuationWidens", "dimension": "actions"})
    );
    let dropped = json!({"op": "narrow",
        "parent": [{"actions": ["read"], "tables": ["*"], "tenant": {"table": "t", "value": "acme"}}],
        "child": [{"actions": ["read"], "tables": ["x"]}]});
    assert_eq!(decide(&exe, &[], &dropped)["error"], "AttenuationTenantDropped");
    let malformed = json!({"op": "covers_name", "pattern": "a*b", "name": "a"});
    assert_eq!(decide(&exe, &[], &malformed)["error"], "GrantPatternMalformed");
    assert_eq!(decide(&exe, &[], &json!([1]))["error"], "CaseMalformed");
}

/// The harness drives the reference binary and the engine's decision function over each generated case and compares the two decisions field by field.
// spec: assurance.differential-test.harness@c63ca77a
#[test]
fn the_engine_and_the_lean_reference_agree_over_generated_cases() {
    let _ = lean_or_skip!();
    let s = Scratch::new();
    std::fs::copy(reference_root().join("corpus/counterexamples.jsonl"), s.corpus()).unwrap();
    let out = Command::new(BIN)
        .args(["formal", "differential", "--seed", "20260922", "--cases", "400", "--root"])
        .arg(reference_root())
        .arg("--corpus")
        .arg(s.corpus())
        .output()
        .unwrap();
    let report = passed(&out);
    assert_eq!(line_value(&report, "generated"), "400");
    assert_eq!(line_value(&report, "disagreements"), "0");
    assert!(line_value(&report, "compared fields").contains("verdict, error, dimension"), "{report}");
}

/// Each run generates malformed inputs, boundary values and well-formed requests, and its report names the classes it drew from.
// spec: assurance.differential-test.case-classes@f6578611
#[test]
fn the_report_names_the_three_case_classes() {
    let s = Scratch::new();
    let report = passed(&s.run(&s.agreeing(), &["--seed", "3", "--cases", "120"]));
    let classes = line_value(&report, "classes");
    let mut total = 0;
    for class in ["malformed", "boundary", "well-formed"] {
        let n: usize = classes
            .split(',')
            .find_map(|c| c.trim().strip_prefix(class).map(|n| n.trim().parse().unwrap()))
            .unwrap_or_else(|| panic!("no `{class}` count in `{classes}`"));
        assert!(n > 0, "class {class} drew no case: {classes}");
        total += n;
    }
    assert_eq!(total, 120);
}

/// Each run records its generator seed, and re-running with that seed reproduces the same case sequence.
// spec: assurance.differential-test.seed@4ee57136
#[test]
fn a_recorded_seed_reproduces_the_case_sequence() {
    let s = Scratch::new();
    let unseeded = passed(&s.run(&s.agreeing(), &["--cases", "60"]));
    let seed = line_value(&unseeded, "seed").to_string();
    let digest = line_value(&unseeded, "cases sha256").to_string();
    let again = passed(&s.run(&s.agreeing(), &["--seed", &seed, "--cases", "60"]));
    assert_eq!(line_value(&again, "cases sha256"), digest, "the recorded seed reproduces the sequence");
    let other = format!("{}", seed.parse::<u64>().unwrap().wrapping_add(1));
    let different = passed(&s.run(&s.agreeing(), &["--seed", &other, "--cases", "60"]));
    assert_ne!(line_value(&different, "cases sha256"), digest, "another seed draws another sequence");
}

/// Every counterexample-corpus case replays before any freshly generated case.
// spec: assurance.differential-test.corpus-replay@c5aa3af1
#[test]
fn corpus_cases_replay_before_generated_cases() {
    let s = Scratch::new();
    let entries: Vec<Value> = (0..3).map(agreeing_entry).collect();
    s.write_corpus(&entries);
    let log = s.path("seen.jsonl");
    let report = passed(&s.run(&s.logging(&log), &["--seed", "11", "--cases", "10"]));
    assert_eq!(line_value(&report, "replayed"), "3");
    let seen = read_jsonl(&log);
    assert!(seen.len() >= 13, "every replayed and generated case reaches the reference");
    for (i, entry) in entries.iter().enumerate() {
        assert_eq!(seen[i], entry["case"], "case {i} reaching the reference is corpus entry {i}");
    }
}

/// A case on which the two decisions differ raises `ReferenceModelDrift`, printing the minimized case and both decisions.
// spec: assurance.differential-test.disagreement@3dc0437e
#[test]
fn a_disagreement_raises_reference_model_drift_with_both_decisions() {
    let s = Scratch::new();
    let err = refused(&s.run(&s.corrupted(), &["--seed", "5", "--cases", "200"]), "ReferenceModelDrift");
    let recorded = s.read_corpus();
    assert_eq!(recorded.len(), 1, "the minimized case is recorded");
    let entry = &recorded[0];
    assert!(err.contains(&entry["case"].to_string()), "prints the minimized case:\n{err}");
    assert_eq!(entry["engine"]["verdict"], "covered");
    assert_eq!(entry["reference"]["verdict"], "not_covered");
    assert!(err.contains(&entry["engine"].to_string()), "prints the engine's decision:\n{err}");
    assert!(err.contains(&entry["reference"].to_string()), "prints the reference's decision:\n{err}");
    assert!(err.contains("verdict"), "names the differing field:\n{err}");
}

/// A disagreement is shrunk until removing any further field makes the two agree, and the minimized case is recorded.
// spec: assurance.differential-test.minimized@4408492b
#[test]
fn a_recorded_disagreement_is_minimal_under_field_removal() {
    let s = Scratch::new();
    let corrupted = s.corrupted();
    for seed in ["8", "13", "34"] {
        s.write_corpus(&[]);
        let err = refused(&s.run(&corrupted, &["--seed", seed, "--cases", "200"]), "ReferenceModelDrift");
        let recorded = s.read_corpus();
        let entry = recorded.last().unwrap();
        let case = &entry["case"];
        let original = &entry["original"];
        assert!(err.contains(&case.to_string()), "{err}");
        assert_ne!(engine(case), decide(&corrupted, &[], case), "the recorded case still disagrees");
        assert_ne!(engine(original), decide(&corrupted, &[], original), "the generated case disagreed");
        for smaller in removals(case) {
            assert_eq!(
                engine(&smaller),
                decide(&corrupted, &[], &smaller),
                "removing a field from {case} leaves {smaller} disagreeing"
            );
        }
    }
}

/// The counterexample corpus retains at most 256 entries, evicting the oldest case a fresh seed reproduces.
// spec: assurance.differential-test.corpus-entries@d7ad49bf
#[test]
fn a_full_corpus_evicts_the_oldest_reproducible_case() {
    let s = Scratch::new();
    let corrupted = s.corrupted();
    // Find a seed and index whose generated case disagrees under the corrupted reference.
    s.write_corpus(&[]);
    refused(&s.run(&corrupted, &["--seed", "21", "--cases", "200"]), "ReferenceModelDrift");
    let found = s.read_corpus().remove(0);
    let (seed, index) = (found["seed"].clone(), found["index"].clone());
    assert!(seed.is_u64() && index.is_u64(), "an entry records its seed and index: {found}");

    let mut entries: Vec<Value> = (0..256).map(agreeing_entry).collect();
    entries[3]["seed"] = seed;
    entries[3]["index"] = index;
    s.write_corpus(&entries);
    refused(&s.run(&corrupted, &["--seed", "22", "--cases", "200"]), "ReferenceModelDrift");
    let after = s.read_corpus();
    assert_eq!(after.len(), 256, "the corpus holds at most 256 entries");
    assert_eq!(after[0], entries[0], "an unreproducible older entry stays");
    assert!(!after.contains(&entries[3]), "the oldest entry a seed reproduces is evicted");
    assert_eq!(after[255]["seed"], 22, "the new entry lands last");

    // With no reproducible entry, the oldest goes.
    let entries: Vec<Value> = (0..256).map(agreeing_entry).collect();
    s.write_corpus(&entries);
    refused(&s.run(&corrupted, &["--seed", "23", "--cases", "200"]), "ReferenceModelDrift");
    let after = s.read_corpus();
    assert_eq!(after.len(), 256);
    assert_eq!(after[0], entries[1]);
}

/// A run reporting a disagreement and writing no corpus entry raises `CounterexampleDiscarded`.
// spec: assurance.differential-test.discarded-counterexample@1b10bb13
#[test]
fn a_disagreement_the_corpus_cannot_hold_is_discarded_loudly() {
    let s = Scratch::new();
    std::fs::write(s.path("blocker"), "a file, not a directory").unwrap();
    let out = Command::new(BIN)
        .args(["formal", "differential", "--seed", "5", "--cases", "200", "--reference"])
        .arg(s.corrupted())
        .arg("--corpus")
        .arg(s.path("blocker/corpus.jsonl"))
        .output()
        .unwrap();
    let err = refused(&out, "CounterexampleDiscarded");
    assert!(err.contains("ReferenceModelDrift"), "names the disagreement it could not record:\n{err}");
}

/// One invocation, corpus replay included, completes within 300 s.
// spec: assurance.differential-test.run-budget@eaff528d
#[test]
fn a_run_past_its_budget_stops() {
    let s = Scratch::new();
    let slow = s.script("slow.sh", &format!("sleep 1\nexec '{BIN}' formal differential --decide"));
    s.write_corpus(&[agreeing_entry(0), agreeing_entry(1), agreeing_entry(2)]);
    let started = std::time::Instant::now();
    let out = s.run(&slow, &["--seed", "1", "--cases", "50", "--budget-secs", "2"]);
    assert!(!out.status.success(), "a run past its budget fails");
    assert!(stderr(&out).contains("budget"), "{}", stderr(&out));
    assert!(started.elapsed().as_secs() < 10, "the run stops at its budget");
    let help = Command::new(BIN).args(["formal", "differential", "--help"]).output().unwrap();
    assert!(!stdout(&help).contains("budget"), "the budget override is hidden");
    let over = s.run(&s.agreeing(), &["--seed", "1", "--cases", "1", "--budget-secs", "301"]);
    assert!(!over.status.success(), "no override raises the budget past 300 s");
}

/// `contextful formal differential --seed <n> --cases <n>` replays the corpus, then runs the generated cases, exiting non-zero on the first disagreement.
// spec: assurance.differential-test.command@4809f915
#[test]
fn the_command_replays_then_generates_and_stops_at_the_first_disagreement() {
    let s = Scratch::new();
    s.write_corpus(&[agreeing_entry(0)]);
    let report = passed(&s.run(&s.agreeing(), &["--seed", "9", "--cases", "25"]));
    assert_eq!(line_value(&report, "replayed"), "1");
    assert_eq!(line_value(&report, "generated"), "25");

    let log = s.path("seen.jsonl");
    let stopping = s.script(
        "stopping.sh",
        &format!(
            "tee -a '{}' | '{BIN}' formal differential --decide | sed 's/\"verdict\":\"covered\"/\"verdict\":\"not_covered\"/'",
            log.display()
        ),
    );
    let out = s.run(&stopping, &["--seed", "9", "--cases", "400"]);
    refused(&out, "ReferenceModelDrift");
    let seen = read_jsonl(&log);
    let first = seen.iter().position(|c| engine(c)["verdict"] == "covered").unwrap();
    let recorded = s.read_corpus();
    let disagreeing = recorded.last().unwrap().clone();
    // One corpus case replays, then generated cases 0..=index run; the first disagreement is the last.
    assert_eq!(disagreeing["index"], first - 1, "the first disagreeing case is the one recorded");
    assert_eq!(seen[first], disagreeing["original"]);
    // Everything after it is a shrinker candidate: a strict reduction of the disagreeing case.
    let size = seen[first].to_string().len();
    let fresh_after: Vec<&Value> = seen[first + 1..].iter().filter(|c| c.to_string().len() >= size).collect();
    assert!(fresh_after.is_empty(), "no generated case runs after the first disagreement: {fresh_after:?}");

    // A disagreeing corpus entry stops the run before any generated case.
    s.write_corpus(std::slice::from_ref(&disagreeing));
    std::fs::remove_file(&log).unwrap();
    refused(&s.run(&stopping, &["--seed", "9", "--cases", "400"]), "ReferenceModelDrift");
    let seen = read_jsonl(&log);
    assert_eq!(seen.first(), Some(&disagreeing["case"]));
    assert_eq!(s.read_corpus(), vec![disagreeing], "a replayed disagreement is already recorded");
}
