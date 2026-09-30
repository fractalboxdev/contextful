//! `contextful formal differential` over the Lean reference binary and over stand-in
//! reference executables, beside the decision module's native and WebAssembly builds.
//!
//! A stand-in is a shell script around `contextful formal differential --decide`, which
//! prints the native build's decision for one case: it agrees with the engine everywhere,
//! and a `sed` over its output corrupts it into a reference that disagrees on a known
//! class of case. Tests reaching the Lean reference skip when `lake` is absent, unless
//! `CONTEXTFUL_REQUIRE_LEAN` is set; tests reaching the WebAssembly build skip when the
//! `wasm32-unknown-unknown` target is not installed, unless `CONTEXTFUL_REQUIRE_WASM` is
//! set. Either variable set turns the skip into a failure.

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

/// Whether the toolchain holds the `wasm32-unknown-unknown` standard library.
fn wasm_target_installed() -> bool {
    let out = Command::new("rustc").args(["--print", "target-libdir", "--target", "wasm32-unknown-unknown"]).output();
    out.ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        .is_some_and(|dir| std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_some()))
}

/// The decision module built for `wasm32-unknown-unknown` once per test process, with the
/// command the harness runs; `None` when the target is not installed.
fn wasm_module() -> Option<PathBuf> {
    static BUILT: OnceLock<Option<PathBuf>> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            if !wasm_target_installed() {
                return None;
            }
            let target_dir = std::env::var_os("CARGO_TARGET_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| repo().join("target"))
                .join("decision-wasm");
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let out = Command::new(cargo)
                .args(["rustc", "-q", "-p", "contextful-policy", "--lib", "--release", "--target", "wasm32-unknown-unknown"])
                .args(["--crate-type", "cdylib", "--target-dir"])
                .arg(&target_dir)
                .current_dir(repo())
                .output()
                .unwrap();
            assert!(out.status.success(), "building the WebAssembly module failed:\n{}", String::from_utf8_lossy(&out.stderr));
            Some(target_dir.join("wasm32-unknown-unknown/release/contextful_policy.wasm"))
        })
        .clone()
}

/// Fail where the gate requires the WebAssembly target, else report the skip.
fn wasm_missing() {
    assert!(
        std::env::var_os("CONTEXTFUL_REQUIRE_WASM").is_none(),
        "CONTEXTFUL_REQUIRE_WASM is set and the wasm32-unknown-unknown target is not installed"
    );
    eprintln!("skipped: no wasm32-unknown-unknown target");
}

macro_rules! wasm_or_skip {
    () => {
        match wasm_module() {
            Some(module) => module,
            None => {
                wasm_missing();
                return;
            }
        }
    };
}

/// The arguments a stand-in run passes: the built module where the binary carries the
/// component host, which then compares it, and none otherwise; a skip where the binary
/// compares the module and none builds.
macro_rules! wasm_args_or_skip {
    () => {
        if cfg!(feature = "component-host") {
            vec![std::ffi::OsString::from("--wasm"), wasm_or_skip!().into_os_string()]
        } else {
            Vec::new()
        }
    };
}

/// The stand-in decision module deciding every case `covered`.
fn stub_module() -> PathBuf {
    repo().join("crates/contextful-wasm/tests/fixtures/decision-stub.wasm")
}

/// A scratch directory holding stand-in reference scripts and a corpus path.
struct Scratch {
    dir: tempfile::TempDir,
    wasm: Vec<std::ffi::OsString>,
}

impl Scratch {
    fn new(wasm: Vec<std::ffi::OsString>) -> Scratch {
        Scratch { dir: tempfile::tempdir().unwrap(), wasm }
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
            .args(&self.wasm)
            .args(args)
            .output()
            .unwrap()
    }
}

/// A JSON Lines file; a line that is not UTF-8 JSON reads as the string `bytes <hex>`, the
/// form the harness prints such a case in.
fn read_jsonl(path: &Path) -> Vec<Value> {
    std::fs::read(path)
        .unwrap_or_default()
        .split(|b| *b == b'\n')
        .filter(|l| !l.iter().all(u8::is_ascii_whitespace))
        .map(|l| {
            serde_json::from_slice(l)
                .unwrap_or_else(|_| Value::String(format!("bytes {}", l.iter().map(|b| format!("{b:02x}")).collect::<String>())))
        })
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

/// Feed one case text, newline-terminated as the harness sends it, and read back the decision.
fn decide_bytes(exe: &Path, bytes: &[u8]) -> Value {
    use std::io::Write;
    let mut child = Command::new(exe).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(&[bytes, b"\n"].concat()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{} failed on {bytes:?}", exe.display());
    serde_json::from_slice(&out.stdout).unwrap()
}

fn engine_bytes(bytes: &[u8]) -> Value {
    use std::io::Write;
    let mut child = Command::new(BIN)
        .args(["formal", "differential", "--decide"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&[bytes, b"\n"].concat()).unwrap();
    serde_json::from_slice(&child.wait_with_output().unwrap().stdout).unwrap()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
}

/// Every case with one character removed from one string, at any depth.
fn shortenings(v: &Value) -> Vec<Value> {
    match v {
        Value::String(text) => {
            let chars: Vec<char> = text.chars().collect();
            (0..chars.len())
                .map(|i| Value::String(chars.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, c)| c).collect()))
                .collect()
        }
        Value::Object(map) => map
            .iter()
            .flat_map(|(k, child)| {
                shortenings(child).into_iter().map(move |short| {
                    let mut m = map.clone();
                    m.insert(k.clone(), short);
                    Value::Object(m)
                })
            })
            .collect(),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .flat_map(|(i, child)| {
                shortenings(child).into_iter().map(move |short| {
                    let mut a = items.clone();
                    a[i] = short;
                    Value::Array(a)
                })
            })
            .collect(),
        _ => Vec::new(),
    }
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

/// The harness drives the reference binary and the decision module's native and WebAssembly builds over each generated case and compares their decisions field by field.
// spec: assurance.differential-test.harness@0f6edc10
#[test]
fn the_engine_and_the_lean_reference_agree_over_generated_cases() {
    let _ = lean_or_skip!();
    let s = Scratch::new(wasm_args_or_skip!());
    std::fs::copy(reference_root().join("corpus/counterexamples.jsonl"), s.corpus()).unwrap();
    let out = Command::new(BIN)
        .args(["formal", "differential", "--seed", "20260922", "--cases", "400", "--root"])
        .arg(reference_root())
        .arg("--corpus")
        .arg(s.corpus())
        .args(&s.wasm)
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
    let s = Scratch::new(wasm_args_or_skip!());
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
    let s = Scratch::new(wasm_args_or_skip!());
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
    let s = Scratch::new(wasm_args_or_skip!());
    let entries: Vec<Value> = (0..3).map(agreeing_entry).collect();
    s.write_corpus(&entries);
    let log = s.path("seen.jsonl");
    let report = passed(&s.run(&s.logging(&log), &["--seed", "11", "--cases", "10"]));
    assert_eq!(line_value(&report, "replayed"), "3");
    let seen = read_jsonl(&log);
    // Every replayed case and every generated case but a credential case reaches the reference.
    let credential: usize = line_value(&report, "operations")
        .split(", ")
        .find_map(|part| part.strip_prefix("verify "))
        .map_or(0, |n| n.parse().unwrap());
    assert_eq!(seen.len(), 13 - credential, "every replayed and generated case reaches the reference");
    for (i, entry) in entries.iter().enumerate() {
        assert_eq!(seen[i], entry["case"], "case {i} reaching the reference is corpus entry {i}");
    }
}

/// A case on which any two of the three decisions differ raises `ReferenceModelDrift`, printing the minimized case and every decision.
// spec: assurance.differential-test.disagreement@fbc5083a
#[test]
fn a_disagreement_raises_reference_model_drift_with_both_decisions() {
    let s = Scratch::new(wasm_args_or_skip!());
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

/// A disagreement is shrunk until no single field removal, string shortening or byte removal keeps any two of the three decisions apart, and the minimized case is recorded.
// spec: assurance.differential-test.minimized@c4ca9d1e
#[test]
fn a_recorded_disagreement_is_minimal_under_every_shrinking_step() {
    let s = Scratch::new(wasm_args_or_skip!());
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
        for smaller in removals(case).into_iter().chain(shortenings(case)) {
            assert_eq!(
                engine(&smaller),
                decide(&corrupted, &[], &smaller),
                "shrinking {case} to {smaller} leaves it disagreeing"
            );
        }
    }

    // A byte case shrinks by single-byte removal, under a reference deciding every text
    // that is not UTF-8 apart from the native build.
    s.write_corpus(&[]);
    let garbling = s.script(
        "garbling.sh",
        &format!(
            "f=$(mktemp '{dir}/case.XXXXXX')\ncat > \"$f\"\nif iconv -f UTF-8 -t UTF-8 < \"$f\" > /dev/null 2>&1; then exec '{BIN}' formal differential --decide < \"$f\"; fi\necho '{{\"verdict\":\"refused\",\"error\":\"Garbled\",\"dimension\":null}}'",
            dir = s.dir.path().display()
        ),
    );
    let err = refused(&s.run(&garbling, &["--seed", "17", "--cases", "600"]), "ReferenceModelDrift");
    let entry = s.read_corpus().pop().expect("the disagreement is recorded");
    let bytes = unhex(entry["bytes"].as_str().unwrap_or_else(|| panic!("a byte case records its bytes: {entry}")));
    assert!(err.contains("Garbled"), "{err}");
    assert_ne!(engine_bytes(&bytes), decide_bytes(&garbling, &bytes), "the recorded bytes still disagree");
    for i in 0..bytes.len() {
        let mut smaller = bytes.clone();
        smaller.remove(i);
        assert_eq!(engine_bytes(&smaller), decide_bytes(&garbling, &smaller), "removing byte {i} of {bytes:?} leaves it disagreeing");
    }
}

/// The counterexample corpus retains at most 256 entries, evicting the oldest case a fresh seed reproduces.
// spec: assurance.differential-test.corpus-entries@d7ad49bf
#[test]
fn a_full_corpus_evicts_the_oldest_reproducible_case() {
    let s = Scratch::new(wasm_args_or_skip!());
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
    let s = Scratch::new(wasm_args_or_skip!());
    std::fs::write(s.path("blocker"), "a file, not a directory").unwrap();
    let out = Command::new(BIN)
        .args(["formal", "differential", "--seed", "5", "--cases", "200", "--reference"])
        .arg(s.corrupted())
        .arg("--corpus")
        .arg(s.path("blocker/corpus.jsonl"))
        .args(&s.wasm)
        .output()
        .unwrap();
    let err = refused(&out, "CounterexampleDiscarded");
    assert!(err.contains("ReferenceModelDrift"), "names the disagreement it could not record:\n{err}");
}

/// One invocation, corpus replay included, completes within 300 s.
// spec: assurance.differential-test.run-budget@eaff528d
#[test]
fn a_run_past_its_budget_stops() {
    let s = Scratch::new(wasm_args_or_skip!());
    // Each reference call records itself, then takes at least 11 s.
    let calls = s.path("calls");
    let slow = s.script("slow.sh", &format!("echo call >> '{}'\nsleep 11\nexec '{BIN}' formal differential --decide", calls.display()));
    s.write_corpus(&[agreeing_entry(0), agreeing_entry(1), agreeing_entry(2)]);
    let out = s.run(&slow, &["--seed", "1", "--cases", "50", "--budget-secs", "30"]);
    assert!(!out.status.success(), "a run past its budget fails");
    assert!(stderr(&out).contains("budget"), "{}", stderr(&out));
    // Against a 30 s budget, loading the builds included, and calls of at least 11 s, the
    // harness issues at most 3 of the 53 calls the run holds, however slowly the machine
    // serves them.
    let made = std::fs::read_to_string(&calls).unwrap_or_default().lines().count();
    assert!((1..=3).contains(&made), "the run stops at its budget after {made} reference calls");
    let help = Command::new(BIN).args(["formal", "differential", "--help"]).output().unwrap();
    assert!(!stdout(&help).contains("budget"), "the budget override is hidden");
    let over = s.run(&s.agreeing(), &["--seed", "1", "--cases", "1", "--budget-secs", "301"]);
    assert!(!over.status.success(), "no override raises the budget past 300 s");
}

/// `contextful formal differential --seed <n> --cases <n>` replays the corpus, then runs the generated cases, exiting non-zero on the first disagreement.
// spec: assurance.differential-test.command@4809f915
#[test]
fn the_command_replays_then_generates_and_stops_at_the_first_disagreement() {
    let s = Scratch::new(wasm_args_or_skip!());
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

/// One module owns the enforcement decision; a gateway and the engine reaching the same verdict execute it, compiled native and to WebAssembly from one pinned source.
// spec: assurance.structure-tree.decision-module@793caa98
#[test]
fn the_native_and_webassembly_builds_agree_with_the_reference_over_the_seeded_budget() {
    let _ = lean_or_skip!();
    if cfg!(not(feature = "component-host")) {
        eprintln!("skipped: the binary carries no component host");
        return;
    }
    let _ = wasm_or_skip!();
    let s = Scratch::new(Vec::new());
    std::fs::copy(reference_root().join("corpus/counterexamples.jsonl"), s.corpus()).unwrap();
    // No `--wasm`: the command compiles the module from this working tree.
    let out = Command::new(BIN)
        .args(["formal", "differential", "--seed", "20260930", "--root"])
        .arg(reference_root())
        .arg("--corpus")
        .arg(s.corpus())
        .current_dir(repo())
        .output()
        .unwrap();
    let report = passed(&out);
    assert_eq!(line_value(&report, "builds"), "native, wasm32-unknown-unknown");
    assert_eq!(line_value(&report, "generated"), "1000", "the default case budget runs");
    assert_eq!(line_value(&report, "disagreements"), "0");
    let operations = line_value(&report, "operations");
    for op in ["covers_name", "covers_pattern", "narrow", "zone_admits", "session_zone", "bytes"] {
        assert!(operations.contains(op), "no `{op}` case ran: {operations}");
    }
}

/// The native build decides in process and the `wasm32-unknown-unknown` build under the decision-module host; absent `--wasm`, the command compiles the module from the working tree, and the report names the builds compared.
// spec: assurance.differential-test.builds@50e61b90
#[test]
fn a_webassembly_build_deciding_apart_from_the_native_build_is_a_disagreement() {
    if cfg!(not(feature = "component-host")) {
        eprintln!("skipped: the binary carries no component host");
        return;
    }
    let _ = wasm_or_skip!();
    // The module compiled from the working tree agrees; the report names both builds.
    let s = Scratch::new(Vec::new());
    let out = Command::new(BIN)
        .args(["formal", "differential", "--seed", "4", "--cases", "40", "--reference"])
        .arg(s.agreeing())
        .arg("--corpus")
        .arg(s.corpus())
        .current_dir(repo())
        .output()
        .unwrap();
    assert_eq!(line_value(&passed(&out), "builds"), "native, wasm32-unknown-unknown");

    // A module deciding every case `covered` parts from the native build and the reference,
    // which agree with each other.
    let stub = Scratch::new(vec!["--wasm".into(), stub_module().into_os_string()]);
    let err = refused(&stub.run(&stub.agreeing(), &["--seed", "4", "--cases", "200"]), "ReferenceModelDrift");
    let entry = stub.read_corpus().pop().expect("the disagreement is recorded");
    assert_eq!(entry["wasm"]["verdict"], "covered", "{entry}");
    assert_ne!(entry["engine"]["verdict"], "covered", "{entry}");
    assert_eq!(entry["engine"], entry["reference"], "{entry}");
    assert!(err.contains("wasm build"), "names the WebAssembly build's decision:\n{err}");
    assert!(err.contains(&entry["wasm"].to_string()), "{err}");
}

/// The malformed class draws case texts carrying invalid UTF-8 or an integer literal outside the unsigned 64-bit range, handed as bytes to every decider, and each decides such a text malformed.
// spec: assurance.differential-test.malformed-bytes@1ef6b509
#[test]
fn malformed_bytes_reach_every_decider_and_each_decides_them_malformed() {
    let exe = lean_or_skip!();
    let s = Scratch::new(wasm_args_or_skip!());
    let log = s.path("seen.bin");
    let logging = s.script("lean-logging.sh", &format!("tee -a '{}' | '{}'", log.display(), exe.display()));
    let report = passed(&s.run(&logging, &["--seed", "17", "--cases", "600"]));
    assert_eq!(line_value(&report, "disagreements"), "0");
    let seen = std::fs::read(&log).unwrap();
    let lines: Vec<&[u8]> = seen.split(|b| *b == b'\n').filter(|l| !l.is_empty()).collect();
    let invalid: Vec<&&[u8]> = lines.iter().filter(|l| std::str::from_utf8(l).is_err()).collect();
    let out_of_range: Vec<&&[u8]> = lines
        .iter()
        .filter(|l| {
            let text = String::from_utf8_lossy(l);
            ["18446744073709551616", "100000000000000000000", "-9223372036854775809"].iter().any(|n| text.contains(n))
        })
        .collect();
    assert!(!invalid.is_empty(), "no case carried invalid UTF-8");
    assert!(!out_of_range.is_empty(), "no case carried an integer past the unsigned 64-bit range");
    for case in invalid.iter().chain(&out_of_range) {
        let mut child = Command::new(&exe).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(case).unwrap();
        let reference: Value = serde_json::from_slice(&child.wait_with_output().unwrap().stdout).unwrap();
        assert_eq!(reference["error"], "CaseMalformed", "{}", String::from_utf8_lossy(case));
        let mut child = Command::new(BIN)
            .args(["formal", "differential", "--decide"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(case).unwrap();
        let native: Value = serde_json::from_slice(&child.wait_with_output().unwrap().stdout).unwrap();
        assert_eq!(native, reference, "{}", String::from_utf8_lossy(case));
    }
}

/// A case names one decision: table-pattern coverage, grant narrowing, zone admission against an allow-set, session-zone resolution, or credential admission; a placement decision also carries the zone it resolved.
// spec: assurance.differential-test.decision-cases@5bf84625
#[test]
fn placement_cases_resolve_zones_in_the_reference_and_the_native_build_alike() {
    let exe = lean_or_skip!();
    let placed = |verdict: &str, zone: &str| json!({"verdict": verdict, "error": null, "dimension": null, "zone": zone});
    let refused = |error: &str| json!({"verdict": "refused", "error": error, "dimension": null});
    let cases = [
        (json!({"op": "zone_admits", "zone": " on-prem:hq ", "allow": ["on-prem:*"]}), placed("admitted", "on-prem:hq")),
        (json!({"op": "zone_admits", "zone": "on-prem:hq", "allow": ["on-prem:ward-3", "local:device"]}), placed("excluded", "on-prem:hq")),
        (json!({"op": "zone_admits", "zone": "cloud", "allow": ["on-prem:*", "public-cloud:*"]}), placed("excluded", "undeclared")),
        (json!({"op": "zone_admits", "zone": "cloud", "allow": ["*"]}), placed("admitted", "undeclared")),
        (json!({"op": "zone_admits", "zone": "\u{a0}local:device", "allow": ["local:device"]}), placed("admitted", "local:device")),
        (json!({"op": "zone_admits", "zone": "local:device", "allow": ["on-prem:**"]}), refused("EnforceZonePatternUnparsed")),
        (json!({"op": "session_zone", "signed": "on-prem:hq", "incognito": false}), placed("placed", "on-prem:hq")),
        (json!({"op": "session_zone", "incognito": true}), placed("placed", "local:device")),
        (json!({"op": "session_zone", "incognito": false}), placed("placed", "undeclared")),
        (
            json!({"op": "session_zone", "asserted": "public-cloud:x", "signed": "on-prem:hq", "incognito": false}),
            refused("EnforceZoneAssertionWidens"),
        ),
        (json!({"op": "session_zone", "signed": "public-cloud:x", "incognito": true}), refused("EnforceIncognitoWidening")),
        (json!({"op": "session_zone", "signed": "on-prem:hq"}), refused("CaseMalformed")),
    ];
    for (case, want) in cases {
        assert_eq!(decide(&exe, &[], &case), want, "reference on {case}");
        assert_eq!(engine(&case), want, "native build on {case}");
    }
}

/// A `verify` case admits one credential against pinned keys as a network checkpoint does; both builds decide it and are compared, the reference model decides none, and the report counts each credential verdict.
// spec: assurance.differential-test.credential-cases@ca93477e
#[test]
fn credential_cases_run_through_both_builds_and_never_reach_the_reference() {
    let exe = lean_or_skip!();
    let s = Scratch::new(wasm_args_or_skip!());
    let log = s.path("seen.jsonl");
    let logging = s.script("lean-logging.sh", &format!("tee -a '{}' | '{}'", log.display(), exe.display()));
    let report = passed(&s.run(&logging, &["--seed", "8", "--cases", "200"]));
    assert_eq!(line_value(&report, "disagreements"), "0");
    let operations = line_value(&report, "operations");
    assert!(operations.contains("verify"), "no credential case ran: {operations}");
    let verdicts = line_value(&report, "verify verdicts");
    for verdict in ["admitted", "not_covered", "refused"] {
        let count: u64 = verdicts
            .split(", ")
            .find_map(|part| part.strip_prefix(&format!("{verdict} ")))
            .unwrap_or_else(|| panic!("no `{verdict}` count: {verdicts}"))
            .parse()
            .unwrap();
        assert!(count > 0, "no credential case decided `{verdict}`: {verdicts}");
    }
    let seen = read_jsonl(&log);
    assert!(!seen.is_empty(), "the reference decided nothing");
    assert!(seen.iter().all(|case| case["op"] != "verify"), "a credential case reached the reference");
}
