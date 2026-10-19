//! The parts a long stage splits into: the remaining CLI tests in hash-partitioned shards,
//! the features stage one part per featured package, and the evaluate stage one part per
//! package group of its gate-tier entries.

use crate::{manifest, stderr, Repo};

fn listed(r: &Repo) -> Vec<String> {
    let out = r.run_ci(&["stages", "--parts"]);
    assert!(out.status.success(), "{}", stderr(&out));
    String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
}

fn position(parts: &[String], part: &str) -> usize {
    parts.iter().position(|p| p == part).unwrap_or_else(|| panic!("no `{part}` part: {parts:?}"))
}

/// A test leaving `<name>.ran` in the package directory, refusing a second run.
fn marking(name: &str) -> String {
    format!("#[test]\nfn {name}() {{ std::fs::OpenOptions::new().write(true).create_new(true).open(\"{name}.ran\").expect(\"{name} ran twice\"); }}\n")
}

const ORDINARY: usize = 12;

fn cli_repo() -> Repo {
    let r = Repo::init();
    r.write("crates/contextful-cli/Cargo.toml", &(manifest("contextful-cli", "") + "\n[features]\nextra = []\n"));
    let lib = "/// ```\n/// std::fs::OpenOptions::new().write(true).create_new(true).open(\"doctest.ran\").expect(\"the doctest ran twice\");\n/// ```\npub fn documented() {}\n\n#[cfg(test)]\nmod unit {\n    #[test]\n    fn unit() { std::fs::OpenOptions::new().write(true).create_new(true).open(\"unit.ran\").expect(\"the unit test ran twice\"); }\n}\n";
    r.write("crates/contextful-cli/src/lib.rs", lib);
    let mut suite: String = (0..ORDINARY).map(|i| marking(&format!("ordinary_{i:02}"))).collect();
    suite.push_str("mod differential {\n    #[test]\n    fn reference() { std::fs::write(\"reference.ran\", \"yes\").unwrap(); }\n}\n");
    suite.push_str("mod protocol_differential {\n    #[test]\n    fn seeded() { std::fs::write(\"protocol.ran\", \"yes\").unwrap(); }\n}\n");
    r.write("crates/contextful-cli/tests/integration/main.rs", &suite);
    r.lock();
    r.commit("a CLI suite of twelve ordinary tests, a unit test and a doctest");
    r
}

fn ran(dir: &std::path::Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".ran")).map(str::to_string))
        .collect();
    out.sort();
    out
}

#[test]
fn the_remaining_cli_tests_run_once_across_four_hash_shards_and_the_first_runs_doctests() {
    let r = cli_repo();
    let parts = listed(&r);
    let shards: Vec<String> = (1..=4).map(|k| format!("workspace.cli-{k}")).collect();
    assert!(parts.contains(&"workspace.compile".to_string()), "{parts:?}");
    assert!(!parts.iter().any(|p| p == "workspace.cli"), "the CLI suite still runs as one part: {parts:?}");
    assert!(position(&parts, "workspace.compile") < position(&parts, &shards[0]), "{parts:?}");
    for pair in shards.windows(2) {
        assert!(position(&parts, &pair[0]) < position(&parts, &pair[1]), "{parts:?}");
    }
    assert!(position(&parts, &shards[3]) < position(&parts, "workspace.cli-formal"), "{parts:?}");

    let package = r.root.join("crates/contextful-cli");
    let mut seen = Vec::new();
    for (k, shard) in shards.iter().enumerate() {
        let out = r.gate(&["--stage", shard]);
        assert!(out.status.success(), "{shard}: {}{}", String::from_utf8_lossy(&out.stdout), stderr(&out));
        let now = ran(&package);
        let fresh: Vec<String> = now.iter().filter(|n| !seen.contains(*n)).cloned().collect();
        let ordinary = fresh.iter().filter(|n| n.starts_with("ordinary_")).count();
        assert!(ordinary < ORDINARY, "{shard} ran every ordinary test: {fresh:?}");
        if k == 0 {
            assert!(fresh.iter().any(|n| n == "doctest") && fresh.iter().any(|n| n == "unit"), "the first shard skipped the doctests or unit tests: {fresh:?}");
        }
        seen = now;
    }
    let expected: Vec<String> = (0..ORDINARY).map(|i| format!("ordinary_{i:02}")).chain(["doctest".into(), "unit".into()]).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    assert_eq!(seen, expected, "the shards together ran another set than the suite");
    assert!(!package.join("reference.ran").exists(), "a shard ran the differential partition");
    assert!(!package.join("protocol.ran").exists(), "a shard ran a test the differential filter reaches");

    // A failure reds exactly the one shard holding the failing test.
    for name in ran(&package) {
        std::fs::remove_file(package.join(format!("{name}.ran"))).unwrap();
    }
    let suite = std::fs::read_to_string(package.join("tests/integration/main.rs")).unwrap();
    r.write("crates/contextful-cli/tests/integration/main.rs", &suite.replace("fn ordinary_05() {", "fn ordinary_05() { panic!(\"shard regression\");"));
    r.commit("one failing ordinary test");
    let mut red = 0;
    for shard in &shards {
        let out = r.gate(&["--stage", shard]);
        if !out.status.success() {
            red += 1;
            assert!(String::from_utf8_lossy(&out.stdout).contains("shard regression"), "{shard}: {}", stderr(&out));
        }
        for name in ran(&package) {
            std::fs::remove_file(package.join(format!("{name}.ran"))).unwrap();
        }
    }
    assert_eq!(red, 1, "one failing test reds exactly one shard");
}

#[test]
fn the_features_stage_dispatches_one_part_per_featured_package() {
    let r = Repo::init();
    for (dir, name) in [("alpha", "contextful-alpha"), ("beta", "beta")] {
        r.write(&format!("crates/{dir}/Cargo.toml"), &(manifest(name, "") + "\n[features]\nextra = []\n"));
        r.write(&format!("crates/{dir}/src/lib.rs"), "");
        r.write(&format!("crates/{dir}/tests/integration/main.rs"), "#[test]\nfn builds() {}\n");
    }
    r.lock();
    r.commit("two featured packages");
    let parts = listed(&r);
    assert!(parts.contains(&"crate-graph".to_string()), "{parts:?}");
    assert!(!parts.iter().any(|p| p == "features.packages"), "featured packages still share one part: {parts:?}");
    // Parts follow package-name order: `beta` sorts before `contextful-alpha`.
    assert!(position(&parts, "features.beta") < position(&parts, "features.alpha"), "{parts:?}");
    assert!(position(&parts, "features.alpha") < position(&parts, "crate-graph"), "{parts:?}");

    let out = r.gate(&["--stage", "features.alpha"]);
    let err = stderr(&out);
    assert!(out.status.success(), "{err}");
    assert!(err.contains("features: contextful-alpha --all-features"), "{err}");
    assert!(!err.contains("features: beta"), "the part ran another package: {err}");

    r.write("crates/beta/tests/integration/main.rs", "#[test]\nfn builds() { assert!(!cfg!(feature = \"extra\"), \"extra alone\"); }\n");
    r.commit("a defect in beta under extra");
    assert!(r.gate(&["--stage", "features.alpha"]).status.success(), "beta's defect reds alpha's part");
    assert!(!r.gate(&["--stage", "features.beta"]).status.success(), "beta's part hid its defect");
}

const MEASURED: &str = r#"
#[test]
fn measured() {
    let dir = std::path::PathBuf::from(std::env::var_os("CONTEXTFUL_MEASURE_DIR").expect("a measure run"));
    let id = env!("CARGO_PKG_NAME").trim_start_matches("contextful-").to_string() + "-figure";
    let record = format!("{{\"id\": \"{id}\", \"value\": 4, \"n\": 1, \"seed\": 7, \"run\": {{\"processor\": \"t\", \"nproc\": 1, \"memory_limit\": null}}}}");
    std::fs::write(dir.join(format!("{id}.json")), record).unwrap();
}
"#;

fn entry(id: &str, test: &str, op: &str) -> String {
    format!("[entry.{id}]\nclause = \"run.journal.entry-key\"\nmetric = \"figure.value\"\nkind = \"test\"\ntier = \"gate\"\nmethod = {{ test = \"{test}\" }}\ntarget = {{ op = \"{op}\", value = 4 }}\n\n")
}

const NATIVE: &str = r#"
#[test]
fn native_set() {
    let dir = std::path::PathBuf::from(std::env::var_os("CONTEXTFUL_MEASURE_DIR").expect("a measure run"));
    let record = "{\"id\": \"native-golden-floor\", \"value\": 0.75, \"n\": 30, \"seed\": 7, \"run\": {\"processor\": \"t\", \"nproc\": 1, \"memory_limit\": null}}";
    std::fs::write(dir.join("native-golden-floor.json"), record).unwrap();
}
"#;

const NATIVE_ENTRY: &str = "[entry.native-golden-floor]\nclause = \"assurance.baseline.native-gate\"\nmetric = \"retrieval.hybrid.r_precision\"\nkind = \"eval\"\ntier = \"gate\"\nmethod = { test = \"demo::measured::native_set\" }\ntarget = { op = \">=\", value = 0.6 }\nseed = 7\n\n";

/// The evaluate stage runs every gate-tier ledger entry and the native case set in the deterministic tier, one part per package group plus a probe part, each reporting the floor and baseline verdicts over its entries.
// spec: assurance.gate.evaluate-stage@94fe67c3
#[test]
fn the_evaluate_stage_dispatches_one_part_per_package_group_each_holding_its_own_floor() {
    let r = Repo::init();
    r.write("spec/spec.lock.json", "{\"clauses\": [{\"id\": \"run.journal.entry-key\"}, {\"id\": \"assurance.baseline.native-gate\"}]}\n");
    r.write("evals/cases/native.jsonl", "{}\n");
    r.write("crates/demo/tests/integration/main.rs", "mod double;\nmod measured;\n");
    r.write("crates/demo/tests/integration/measured.rs", &format!("{MEASURED}{NATIVE}"));
    r.write("crates/engine/Cargo.toml", &manifest("contextful-engine", ""));
    r.write("crates/engine/src/lib.rs", "");
    r.write("crates/engine/tests/integration/main.rs", "mod measured;\n");
    r.write("crates/engine/tests/integration/measured.rs", MEASURED);
    r.write("evals/ledger.toml", &(entry("demo-figure", "demo::measured::measured", "==") + &entry("engine-figure", "contextful_engine::measured::measured", "==") + NATIVE_ENTRY));
    r.lock();
    r.commit("a foundation entry and a runtime entry");

    let parts = listed(&r);
    let groups: Vec<String> = ["foundation", "runtime", "read", "cli", "ci", "probe"].iter().map(|g| format!("evaluate.{g}")).collect();
    assert!(parts.contains(&"acceptance".to_string()), "{parts:?}");
    assert!(!parts.iter().any(|p| p == "evaluate"), "the evaluate stage still runs as one part: {parts:?}");
    assert!(position(&parts, "acceptance") < position(&parts, &groups[0]), "{parts:?}");
    for pair in groups.windows(2) {
        assert!(position(&parts, &pair[0]) < position(&parts, &pair[1]), "{parts:?}");
    }

    let out = r.gate(&["--stage", "evaluate.runtime"]);
    let err = stderr(&out);
    assert!(out.status.success(), "{err}");
    assert!(err.contains("measure: engine-figure = 4") && !err.contains("demo-figure"), "the runtime part measured beyond its group: {err}");
    assert!(err.contains("evaluate: floor verdict held"), "{err}");
    assert!(!err.contains("baseline verdict"), "the runtime part reported the native set's verdict: {err}");

    // The native case set's part reports its baseline verdict beside the floor.
    let out = r.gate(&["--stage", "evaluate.foundation"]);
    let err = stderr(&out);
    assert!(out.status.success(), "{err}");
    assert!(err.contains("measure: native-golden-floor = 0.75") && !err.contains("engine-figure"), "{err}");
    assert!(err.contains("evaluate: baseline verdict held for evals/cases/native.jsonl (native-golden-floor)"), "{err}");

    let out = r.gate(&["--stage", "evaluate.probe"]);
    assert!(out.status.success(), "the probe part with no probe entry fails: {}", stderr(&out));
    assert!(!stderr(&out).contains("-figure") && !stderr(&out).contains("native-golden-floor ="), "{}", stderr(&out));

    let out = r.gate(&["--stage", "evaluate.read"]);
    assert!(out.status.success(), "a part with no entry fails: {}", stderr(&out));
    assert!(!stderr(&out).contains("-figure"), "{}", stderr(&out));

    // A missed target reds its group's part alone, and the whole stage with it.
    r.write("evals/ledger.toml", &(entry("demo-figure", "demo::measured::measured", ">") + &entry("engine-figure", "contextful_engine::measured::measured", "==") + NATIVE_ENTRY));
    r.commit("the foundation figure misses its target");
    let out = r.gate(&["--stage", "evaluate.foundation"]);
    let err = stderr(&out);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("demo-figure = 4 against > 4") && err.contains("evaluate: floor verdict red"), "{err}");
    assert!(err.contains("evaluate: baseline verdict held for evals/cases/native.jsonl"), "a missed figure beside the native set reds its baseline: {err}");
    assert!(r.gate(&["--stage", "evaluate.runtime"]).status.success(), "the foundation miss reds the runtime part");
    assert!(!r.gate(&["--stage", "evaluate"]).status.success(), "the whole stage holds over a red part");
}
