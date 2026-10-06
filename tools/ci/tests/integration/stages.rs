//! `assurance.gate`: the stage sequence, subset selection, every stage's report, and the
//! pins, schema, evaluate, formal and TypeScript surfaces stages.

use crate::{stderr, Repo};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

const SEQUENCE: [&str; 13] = [
    "pins",
    "toolchain",
    "schema",
    "test-first",
    "workspace",
    "acceptance",
    "evaluate",
    "features",
    "crate-graph",
    "connectors",
    "surfaces",
    "formal",
    "budget",
];

/// A directory of stand-in executables placed first on PATH.
struct Bin {
    dir: tempfile::TempDir,
}

impl Bin {
    fn new() -> Bin {
        Bin { dir: tempfile::tempdir().unwrap() }
    }

    /// A POSIX shell script answering for `name`.
    fn fake(&self, name: &str, script: &str) -> &Bin {
        let p = self.dir.path().join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{script}")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        self
    }

    fn path(&self) -> String {
        format!("{}:{}", self.dir.path().display(), std::env::var("PATH").unwrap_or_default())
    }

    /// Where the stand-ins append the arguments they were called with.
    fn log(&self) -> PathBuf {
        self.dir.path().join("calls.log")
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.log()).unwrap_or_default().lines().map(str::to_string).collect()
    }
}

/// The gate in `r`, with `bin`'s stand-ins first on PATH and logging, and `env` set. The
/// variables a stage sets are cleared, so the report reads this run's changes alone.
fn gate_env(r: &Repo, bin: Option<&Bin>, env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_contextful-ci"));
    c.arg("gate").args(args).current_dir(&r.root).env_remove("CARGO_TARGET_DIR");
    for v in ["CONTEXTFUL_REQUIRE_WASM", "CONTEXTFUL_REQUIRE_LEAN", "ELAN_TOOLCHAIN"] {
        c.env_remove(v);
    }
    if let Some(b) = bin {
        c.env("PATH", b.path()).env("CALLS", b.log());
    }
    c.envs(env.iter().copied());
    c.output().unwrap()
}

fn gate(r: &Repo, bin: Option<&Bin>, args: &[&str]) -> Output {
    gate_env(r, bin, &[], args)
}

fn ran(o: &Output) -> Vec<String> {
    stderr(o).lines().filter_map(|l| l.strip_prefix("--- stage ")).map(str::to_string).collect()
}

/// The gate runs its stages in order — pins, toolchain, schema, test-first, workspace, acceptance, evaluate, features, crate graph, connectors, TypeScript surfaces, formal, budget — and a subset is selectable by name.
// spec: assurance.gate.stage-sequence@cb4bfe5a
#[test]
fn the_gate_defines_the_thirteen_stages_in_run_order_and_runs_a_named_subset() {
    let out = Command::new(env!("CARGO_BIN_EXE_contextful-ci")).arg("stages").output().unwrap();
    assert!(out.status.success());
    let listed: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect();
    assert_eq!(listed, SEQUENCE);

    let r = Repo::init();
    // Named last first, the subset still runs in the sequence's order.
    let o = gate(&r, None, &["--stage", "features", "--stage", "pins"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(ran(&o), ["pins", "features"]);
}

/// A selected subset runs in the sequence's order; a stage reading an unselected predecessor's output, with that output absent, raises `StagePredecessorMissing`, naming both stages, before any stage runs.
// spec: assurance.gate.stage-subset@dfdbf69b
#[test]
fn a_subset_omitting_a_predecessor_whose_output_is_absent_is_refused_before_any_stage() {
    let r = Repo::init();
    let o = gate(&r, None, &["--stage", "pins", "--stage", "formal"]);
    let err = stderr(&o);
    assert!(!o.status.success(), "{err}");
    assert!(err.contains("StagePredecessorMissing: stage `formal` reads target/gate/toolchain.env, which stage `toolchain` writes"), "{err}");
    assert!(ran(&o).is_empty(), "a stage ran before the refusal: {err}");

    let o = gate(&r, None, &["--stage", "toolchain"]);
    let err = stderr(&o);
    assert!(err.contains("StagePredecessorMissing: stage `toolchain` reads target/gate/pins.json, which stage `pins` writes"), "{err}");
    assert!(ran(&o).is_empty(), "{err}");

    // The predecessor selected, the subset runs; its output on disk then satisfies a rerun.
    let o = gate(&r, None, &["--stage", "toolchain", "--stage", "pins"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(ran(&o), ["pins", "toolchain"]);
    assert!(r.root.join("target/gate/pins.json").is_file());
    let o = gate(&r, None, &["--stage", "toolchain"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(ran(&o), ["toolchain"]);
}

/// Each remote check runs its stage together with every predecessor whose output that stage reads, so no check reads another check's sandbox.
// spec: assurance.gate.remote-predecessors@899b36bf
#[test]
fn predecessors_adds_every_stage_whose_output_a_selected_stage_reads() {
    let r = Repo::init();
    let o = gate(&r, None, &["--predecessors", "--stage", "toolchain"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(ran(&o), ["pins", "toolchain"]);

    let yml = std::fs::read_to_string(crate::repo_root().join(".github/workflows/gate.yml")).unwrap();
    assert!(
        yml.contains("\"command\": \"cargo run --locked -q -p contextful-ci -- gate --predecessors --stage ${{ matrix.stage }}"),
        "the remote check runs its stage without the predecessors it reads"
    );
}

/// Each stage prints the environment it leaves and its memory limit, peak and event counts, and a failing stage prints its diagnostics before propagating its exit code.
// spec: assurance.gate.stage-reports@e77a2384
#[test]
fn every_stage_reports_its_environment_and_memory_and_a_failure_keeps_its_exit_code() {
    let r = Repo::init();
    let o = gate(&r, None, &["--stage", "pins", "--stage", "toolchain"]);
    let err = stderr(&o);
    assert!(o.status.success(), "{err}");
    assert!(err.contains("report: stage `pins` passed"), "{err}");
    assert!(err.contains("report: stage `toolchain` passed"), "{err}");
    let left = err.lines().find(|l| l.starts_with("report: environment left ") && l.contains("CONTEXTFUL_REQUIRE_WASM=1"));
    assert!(left.is_some(), "the toolchain stage reports no variable it set: {err}");
    assert!(err.contains("report: environment left unchanged"), "the pins stage sets nothing: {err}");
    assert_eq!(err.matches("report: memory limit ").count(), 2, "{err}");
    let line = err.lines().find(|l| l.starts_with("report: memory limit ")).unwrap();
    assert!(line.contains(", peak ") && line.contains(", events "), "{line}");

    // A stage whose child exits 3 propagates 3, printing its report and diagnostics first.
    r.write("Cargo.lock", "# a lock\n");
    r.commit("a lock file");
    let bin = Bin::new();
    bin.fake("cargo", "echo \"$@\" >> \"$CALLS\"\nexit 3\n");
    let o = gate(&r, Some(&bin), &["--stage", "pins"]);
    let err = stderr(&o);
    assert_eq!(o.status.code(), Some(3), "{err}");
    let report = err.find("report: stage `pins` failed").expect(&err);
    let diagnostic = err.find("report: diagnostic 1: `cargo fetch --locked` exited 3").expect(&err);
    assert!(report < diagnostic, "{err}");
    assert!(err.trim_end().ends_with("`cargo fetch --locked` exited 3"), "the diagnostics print before the exit: {err}");

    // A child killed by a signal reads as 128 plus its number.
    bin.fake("cargo", "kill -9 $$\n");
    let o = gate(&r, Some(&bin), &["--stage", "pins"]);
    let err = stderr(&o);
    assert_eq!(o.status.code(), Some(137), "{err}");
    assert!(err.contains("report: a child process was killed by signal 9"), "{err}");

    // Each stage's peak is its own: a later stage never reports an earlier stage's peak.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    bin.fake("cargo", &format!("if [ \"$1\" = fetch ]; then s=xxxxxxxxxxxxxxxx; i=0; while [ \"$i\" -lt 24 ]; do s=$s$s; i=$((i+1)); done; exit 0; fi\nexec \"{cargo}\" \"$@\"\n"));
    let o = gate(&r, Some(&bin), &["--stage", "pins", "--stage", "toolchain"]);
    let err = stderr(&o);
    assert!(o.status.success(), "{err}");
    let peaks: Vec<&str> = err.lines().filter(|l| l.starts_with("report: memory limit ")).collect();
    assert_eq!(peaks.len(), 2, "{err}");
    let exact = |line: &str| -> Option<u64> { line.split(", peak ").nth(1)?.split(" bytes").next()?.parse().ok() };
    assert!(exact(peaks[0]).is_some_and(|b| b >= 200_000_000), "the pins stage reports its own child's peak: {}", peaks[0]);
    assert!(exact(peaks[1]).is_none_or(|b| b < 200_000_000), "the toolchain stage reports the pins stage's peak: {}", peaks[1]);

    // The budget and crate-graph stages propagate a killed build or cargo-deny as 128 + n.
    r.write("deny.toml", &std::fs::read_to_string(crate::repo_root().join("deny.toml")).unwrap());
    r.write(
        "spec/terms/assurance.toml",
        "[limit]\nassurance-edge-compressed = { clause = \"assurance.gate.edge-budget\", value = 1, unit = \"MiB\", basis = \"chosen\", gloss = \"Compressed edge-profile artifact.\" }\n",
    );
    r.write("crates/contextful-cli/Cargo.toml", &format!("{}\n[features]\ncontextful-edge = []\n", crate::manifest("contextful-cli", "")));
    r.write("crates/contextful-cli/src/lib.rs", "");
    r.write("crates/contextful-cli/tests/integration/main.rs", "");
    std::fs::remove_file(r.root.join("Cargo.lock")).unwrap();
    r.lock();
    r.commit("the binary declaring the edge profile");
    bin.fake("cargo", &format!("if [ \"$1\" = build ]; then kill -9 $$; fi\nexec \"{cargo}\" \"$@\"\n"));
    bin.fake("docker", "kill -9 $$\n");
    bin.fake("cargo-deny", "if [ \"$1\" = --version ]; then echo \"cargo-deny 0.20.2\"; exit 0; fi\nkill -9 $$\n");
    for stage in ["budget", "crate-graph"] {
        let o = gate(&r, Some(&bin), &["--stage", stage]);
        let err = stderr(&o);
        assert_eq!(o.status.code(), Some(137), "{stage}: {err}");
        assert!(err.contains("report: a child process was killed by signal 9"), "{stage}: {err}");
    }
}

/// The pins stage resolves every pinned artifact identity a run depends on before any compilation.
// spec: assurance.gate.pins-stage@08596698
#[test]
fn the_pins_stage_records_every_pin_fetches_the_locked_crates_and_refuses_a_floating_one() {
    let r = Repo::init();
    r.write("formal/lean-toolchain", "leanprover/lean4:v4.29.1\n");
    r.write("formal/protocol/lean-toolchain", "leanprover/lean4:v4.29.1\n");
    r.write(".github/workflows/gate.yml", &format!("jobs:\n  a:\n    steps:\n      - uses: owner/action@{}\n", "a".repeat(40)));
    r.write("Cargo.lock", "# a lock\n");
    r.commit("pins");
    let bin = Bin::new();
    bin.fake("cargo", "echo \"$@\" >> \"$CALLS\"\n");
    let o = gate(&r, Some(&bin), &["--stage", "pins"]);
    assert!(o.status.success(), "{}", stderr(&o));
    // The locked crates resolve, and nothing compiles.
    assert_eq!(bin.calls(), ["fetch --locked"]);
    let record: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(r.root.join("target/gate/pins.json")).unwrap()).unwrap();
    assert_eq!(record["lean"], "leanprover/lean4:v4.29.1");
    assert_eq!(record["actions"][0], format!("owner/action@{}", "a".repeat(40)));
    assert!(record["cargo-lock"].as_str().unwrap().starts_with("sha256:"));

    // A floating Lean pin, two Lean pins apart, and an action on a tag each fail the stage.
    for (file, text, said) in [
        ("formal/protocol/lean-toolchain", "leanprover/lean4:stable\n", "names no exact release"),
        ("formal/protocol/lean-toolchain", "leanprover/lean4:v4.28.0\n", "pin 2 toolchains"),
        (".github/workflows/gate.yml", "jobs:\n  a:\n    steps:\n      - uses: owner/action@v4\n", "names no commit"),
    ] {
        let r = Repo::init();
        r.write("formal/lean-toolchain", "leanprover/lean4:v4.29.1\n");
        r.write(file, text);
        r.commit("a floating pin");
        let o = gate(&r, None, &["--stage", "pins"]);
        assert!(!o.status.success());
        assert!(stderr(&o).contains(said), "{file}: {}", stderr(&o));
    }
}

/// The schema stage regenerates each derived artifact into a scratch location, compares it byte for byte against the committed copy, and runs `contextful-spec lint`.
// spec: assurance.gate.schema-stage@d50ce5a0
#[test]
fn the_schema_stage_regenerates_into_scratch_and_refuses_a_stale_committed_copy() {
    // The stand-in regenerates the lock file as `{"clauses": []}` under the root it is given.
    let bin = Bin::new();
    bin.fake(
        "cargo",
        "echo \"$@\" >> \"$CALLS\"\nprev=\nfor a in \"$@\"; do\n  if [ \"$prev\" = \"--root\" ]; then root=\"$a\"; fi\n  prev=\"$a\"\ndone\ncase \"$*\" in *extract*) printf '{\"clauses\": []}\\n' > \"$root/spec/spec.lock.json\";; esac\n",
    );
    let r = Repo::init();
    r.write("spec/spec.lock.json", "{\"clauses\": [ ]}\n");
    r.commit("a stale lock file");
    let o = gate(&r, Some(&bin), &["--stage", "schema"]);
    let err = stderr(&o);
    assert!(!o.status.success(), "{err}");
    assert!(err.contains("schema: spec/spec.lock.json differs from its regeneration"), "{err}");
    // The committed copy is untouched, the scratch directory reclaimed, and lint never ran.
    assert_eq!(std::fs::read_to_string(r.root.join("spec/spec.lock.json")).unwrap(), "{\"clauses\": [ ]}\n");
    assert!(!r.root.join("target/gate/schema").exists());
    let calls = bin.calls();
    assert!(calls.iter().all(|c| !c.ends_with("lint")), "{calls:?}");
    assert!(calls.iter().any(|c| c.contains("--root") && c.contains("target/gate/schema") && c.ends_with("extract")), "{calls:?}");

    let r = Repo::init();
    r.write("spec/spec.lock.json", "{\"clauses\": []}\n");
    r.commit("a current lock file");
    std::fs::remove_file(bin.log()).unwrap();
    let o = gate(&r, Some(&bin), &["--stage", "schema"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(bin.calls().last().map(String::as_str), Some("run --locked -q -p contextful-spec -- lint"));
}

/// The evaluate stage runs every gate-tier ledger entry and the native case set in the deterministic tier, and reports the floor and baseline verdicts.
// spec: assurance.gate.evaluate-stage@316d20b1
#[test]
fn the_evaluate_stage_runs_the_native_case_set_and_reports_both_verdicts() {
    const MEASURED: &str = r#"
#[test]
fn native_set() {
    let dir = std::path::PathBuf::from(std::env::var_os("CONTEXTFUL_MEASURE_DIR").expect("a measure run"));
    let record = "{\"id\": \"native-golden-floor\", \"value\": 0.75, \"n\": 30, \"seed\": 7, \"run\": {\"processor\": \"t\", \"nproc\": 1, \"memory_limit\": null}}";
    std::fs::write(dir.join("native-golden-floor.json"), record).unwrap();
}
"#;
    let entry = "[entry.native-golden-floor]\nclause = \"assurance.baseline.native-gate\"\nmetric = \"retrieval.hybrid.r_precision\"\nkind = \"eval\"\ntier = \"gate\"\nmethod = { test = \"demo::measured::native_set\" }\ntarget = { op = \">=\", value = 0.6 }\nseed = 7\n";
    let r = Repo::init();
    r.write("spec/spec.lock.json", "{\"clauses\": [{\"id\": \"assurance.baseline.native-gate\"}]}\n");
    r.write("evals/cases/native.jsonl", "{}\n");
    r.write("evals/ledger.toml", "");
    r.write("crates/demo/tests/integration/main.rs", "mod double;\nmod measured;\n");
    r.write("crates/demo/tests/integration/measured.rs", MEASURED);
    r.commit("the native case set, run by no entry");
    let o = gate(&r, None, &["--stage", "evaluate"]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("the tree carries evals/cases/native.jsonl, and no gate-tier ledger entry"), "{}", stderr(&o));

    r.write("evals/ledger.toml", entry);
    r.commit("a gate-tier entry running it");
    let o = gate(&r, None, &["--stage", "evaluate"]);
    let err = stderr(&o);
    assert!(o.status.success(), "{err}");
    assert!(err.contains("measure: native-golden-floor = 0.75"), "{err}");
    assert!(err.contains("evaluate: floor verdict held"), "{err}");
    assert!(err.contains("evaluate: baseline verdict held for evals/cases/native.jsonl (native-golden-floor)"), "{err}");

    // The native set missing its floor turns the baseline verdict red.
    r.write("crates/demo/tests/integration/measured.rs", &MEASURED.replace("0.75", "0.5"));
    r.commit("the native set under its floor");
    let o = gate(&r, None, &["--stage", "evaluate"]);
    let err = stderr(&o);
    assert!(!o.status.success(), "{err}");
    assert!(err.contains("measure: native-golden-floor = 0.5"), "{err}");
    assert!(err.contains("evaluate: floor verdict red"), "{err}");
    assert!(err.contains("evaluate: baseline verdict red for evals/cases/native.jsonl (native-golden-floor)"), "{err}");

    // The native set's test failing turns it red, and its child's exit code propagates.
    r.write("crates/demo/tests/integration/measured.rs", &MEASURED.replace("let dir", "panic!(\"no native set\");\n    #[allow(unreachable_code)]\n    let dir"));
    r.commit("the native set's test fails");
    let o = gate(&r, None, &["--stage", "evaluate"]);
    let err = stderr(&o);
    assert_eq!(o.status.code(), Some(101), "{err}");
    assert!(err.contains("evaluate: baseline verdict red for evals/cases/native.jsonl"), "{err}");

    // An earlier gate-tier entry failing leaves the native set run and its own verdict read.
    let failing = "\n#[test]\nfn a_failing() {\n    panic!(\"an earlier entry\");\n}\n";
    r.write("crates/demo/tests/integration/measured.rs", &format!("{MEASURED}{failing}"));
    r.write("spec/spec.lock.json", "{\"clauses\": [{\"id\": \"assurance.baseline.native-gate\"}, {\"id\": \"run.journal.entry-key\"}]}\n");
    let earlier = "[entry.a-failing]\nclause = \"run.journal.entry-key\"\nmetric = \"effects_per_key.max\"\nkind = \"test\"\ntier = \"gate\"\nmethod = { test = \"demo::measured::a_failing\" }\ntarget = { op = \"<=\", value = 1 }\n\n";
    r.write("evals/ledger.toml", &format!("{earlier}{entry}"));
    r.commit("an earlier gate-tier entry that fails");
    let o = gate(&r, None, &["--stage", "evaluate"]);
    let err = stderr(&o);
    assert_eq!(o.status.code(), Some(101), "{err}");
    assert!(err.contains("measure: native-golden-floor = 0.75"), "the native set never ran: {err}");
    assert!(err.contains("evaluate: floor verdict red"), "{err}");
    assert!(err.contains("evaluate: baseline verdict held for evals/cases/native.jsonl (native-golden-floor)"), "{err}");
}

/// A Lean package tree the formal stage checks: the policy and protocol packages, each with
/// an inventory, and the toolchain record putting `bin` first on PATH.
fn formal_repo(bin: &Bin) -> Repo {
    let r = Repo::init();
    for f in ["formal/inventory.toml", "formal/protocol/inventory.toml", "formal/protocol/lakefile.toml"] {
        r.write(f, "");
    }
    r.commit("formal packages");
    r.write("target/gate/toolchain.env", &format!("PATH={}\nCONTEXTFUL_REQUIRE_LEAN=1\n", bin.path()));
    r
}

/// The formal stage runs {{assurance.audit-assumptions.check-command}}, {{assurance.differential-test.command}} and {{assurance.model.protocol-check}}; a non-zero exit from any reds the run.
// spec: assurance.gate.formal-stage@d308b6b6
#[test]
fn the_formal_stage_runs_the_audit_the_differential_and_the_protocol_check() {
    let bin = Bin::new();
    bin.fake("cargo", "echo \"cargo $*\" >> \"$CALLS\"\n");
    bin.fake("lake", "echo \"lake $* in $(basename \"$PWD\") with $CONTEXTFUL_REQUIRE_LEAN\" >> \"$CALLS\"\nexit \"${LAKE_EXIT:-0}\"\n");
    let r = formal_repo(&bin);
    // The stand-ins reach PATH through the toolchain record alone.
    let calls = bin.log().to_string_lossy().to_string();
    let o = gate_env(&r, None, &[("CALLS", &calls)], &["--stage", "formal"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let cli = "cargo run --locked -q -p contextful-cli --bin contextful -- formal";
    assert_eq!(
        bin.calls(),
        [
            format!("{cli} check --root formal"),
            format!("{cli} check --root formal/protocol"),
            format!("{cli} differential"),
            "lake exe protocol check in protocol with 1".to_string(),
        ]
    );

    // A non-zero exit from the protocol check reds the run with its own code.
    let o = gate_env(&r, None, &[("CALLS", &calls), ("LAKE_EXIT", "4")], &["--stage", "formal"]);
    assert_eq!(o.status.code(), Some(4), "{}", stderr(&o));
    assert!(stderr(&o).contains("`lake exe protocol check` exited 4"), "{}", stderr(&o));

    // So does a failing audit, before the later commands run.
    std::fs::remove_file(bin.log()).unwrap();
    bin.fake("cargo", "echo \"cargo $*\" >> \"$CALLS\"\nexit 1\n");
    let o = gate_env(&r, None, &[("CALLS", &calls)], &["--stage", "formal"]);
    assert!(!o.status.success());
    assert_eq!(bin.calls(), [format!("{cli} check --root formal")]);
}

/// A surface with `typecheck` and `build` scripts and no `test` script.
fn surface_repo() -> Repo {
    let r = Repo::init();
    r.write("apps/web/package.json", "{\"name\": \"web\", \"scripts\": {\"typecheck\": \"tsc\", \"build\": \"vite build\", \"dev\": \"vite\"}}\n");
    r.write("apps/web/pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
    r.commit("a surface");
    r
}

fn pnpm(bin: &Bin, failing: &str) {
    bin.fake("pnpm", &format!("echo \"pnpm $* in $(basename \"$PWD\")\" >> \"$CALLS\"\n[ \"$*\" = \"run {failing}\" ] && exit 2\nexit 0\n"));
}

/// The TypeScript surfaces run typecheck, unit tests and framework build in one stage, and a surface declaring no script for a check skips that check.
// spec: assurance.gate.typescript-surfaces@4d9c3bb2
#[test]
fn the_surfaces_stage_installs_then_runs_each_declared_check() {
    let r = surface_repo();
    let bin = Bin::new();
    pnpm(&bin, "none");
    let o = gate(&r, Some(&bin), &["--stage", "surfaces"]);
    let err = stderr(&o);
    assert!(o.status.success(), "{err}");
    assert_eq!(bin.calls(), ["pnpm install --frozen-lockfile in web", "pnpm run typecheck in web", "pnpm run build in web"]);
    assert!(err.contains("surfaces: apps/web declares no test script"), "{err}");
}

/// A surface whose typecheck, unit tests or framework build fails raises `SurfaceCheckFailed`, naming the surface and the script.
// spec: assurance.gate.surface-check-failed@e183417c
#[test]
fn a_failing_surface_check_is_refused_naming_the_surface_and_the_script() {
    for script in ["typecheck", "build"] {
        let r = surface_repo();
        let bin = Bin::new();
        pnpm(&bin, script);
        let o = gate(&r, Some(&bin), &["--stage", "surfaces"]);
        assert!(!o.status.success());
        assert!(stderr(&o).contains(&format!("SurfaceCheckFailed: surface apps/web: script `{script}` exited 2")), "{}", stderr(&o));
        assert_eq!(bin.calls().last().map(String::as_str), Some(format!("pnpm run {script} in web").as_str()));
    }
}

#[test]
fn the_surfaces_stage_runs_each_native_typescript_test_file_even_without_a_test_script() {
    let r = surface_repo();
    r.write("apps/web/test/pin.test.ts", "import { test } from 'node:test';\ntest('pinned refusal', () => { throw new Error('red'); });\n");
    r.commit("a native surface test");
    let bin = Bin::new();
    pnpm(&bin, "none");
    bin.fake("node", "echo \"node $* in $(basename \"$PWD\")\" >> \"$CALLS\"\nexit 5\n");
    let o = gate(&r, Some(&bin), &["--stage", "surfaces"]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("SurfaceCheckFailed: surface apps/web: test `test/pin.test.ts` exited 5"), "{}", stderr(&o));
    assert!(bin.calls().iter().any(|call| call == "node --experimental-strip-types --test test/pin.test.ts in web"), "{:?}", bin.calls());
}
