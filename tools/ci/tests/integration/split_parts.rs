//! The parts a long stage splits into: the remaining CLI tests in duration-balanced shards,
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
    r.write("tools/ci/cli-shards.toml", "[test]\n");
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

/// The `workspace.cli-<k>` parts in run order.
fn cli_shards(parts: &[String]) -> Vec<String> {
    parts.iter().filter(|p| p.strip_prefix("workspace.cli-").is_some_and(|k| k.parse::<usize>().is_ok())).cloned().collect()
}

/// A map line assigning `test` to `shard` at `seconds`.
fn mapped(test: &str, shard: usize, seconds: f64) -> String {
    format!("\"{test}\" = {{ shard = {shard}, seconds = {seconds:.1} }}\n")
}

/// `tools/ci/cli-shards.toml` assigns each remaining CLI test, a nested-build group whole, one shard by recorded duration within 9 min per shard; an unmapped test, reported as drift, joins its group or the least-loaded shard, which adds unit tests and doctests.
// spec: assurance.gate.cli-shard-map@1648cae3
#[test]
fn the_shard_map_runs_each_cli_test_once_and_a_test_it_lacks_in_the_least_loaded_shard() {
    let r = cli_repo();
    let parts = listed(&r);
    let shards = cli_shards(&parts);
    let n = shards.len();
    assert!(n >= 4, "{parts:?}");
    assert!(position(&parts, "workspace.compile") < position(&parts, &shards[0]), "{parts:?}");
    for pair in shards.windows(2) {
        assert!(position(&parts, &pair[0]) < position(&parts, &pair[1]), "{parts:?}");
    }
    assert!(position(&parts, &shards[n - 1]) < position(&parts, "suite.cli"), "the differential suite's check follows the shards: {parts:?}");

    // The map lacks `ordinary_10` and `ordinary_11` and names `gone::away`, which is no test.
    // `ordinary_10` shares a group with `ordinary_09`, so it runs in that test's shard;
    // `ordinary_11` runs in the shard of least recorded time.
    let home = |i: usize| i % n + 1;
    let seconds = |k: usize| if k == 3 { 10.0 } else { 100.0 };
    let mut map = String::from("[group]\nnested-build = [\"ordinary_09\", \"ordinary_10\"]\n\n[test]\n");
    let mut load = vec![0.0; n];
    for i in 0..ORDINARY - 2 {
        map.push_str(&mapped(&format!("ordinary_{i:02}"), home(i), seconds(home(i))));
        load[home(i) - 1] += seconds(home(i));
    }
    map.push_str(&mapped("gone::away", 3, 1.0));
    let fallback = (1..=n).min_by(|a, b| f64::total_cmp(&load[a - 1], &load[b - 1])).unwrap();
    assert_ne!(fallback, home(9), "the fixture separates the group's shard from the fallback");
    r.write("tools/ci/cli-shards.toml", &map);
    r.commit("a shard map lacking two tests and naming a removed one");

    let package = r.root.join("crates/contextful-cli");
    let mut seen: Vec<String> = Vec::new();
    for (k, shard) in shards.iter().enumerate().map(|(i, s)| (i + 1, s)) {
        let out = r.gate(&["--stage", shard]);
        let err = stderr(&out);
        assert!(out.status.success(), "{shard}: {}{err}", String::from_utf8_lossy(&out.stdout));
        let now = ran(&package);
        let fresh: Vec<String> = now.iter().filter(|n| !seen.contains(*n)).cloned().collect();
        let mut expected: Vec<String> = (0..ORDINARY - 1).filter(|i| home(if *i == 10 { 9 } else { *i }) == k).map(|i| format!("ordinary_{i:02}")).collect();
        if k == fallback {
            expected.extend(["ordinary_11".into(), "doctest".into(), "unit".into()]);
        }
        assert!(err.contains("drift") && err.contains("ordinary_10") && err.contains("ordinary_11") && err.contains("gone::away"), "{shard} reported no drift: {err}");
        expected.sort();
        assert_eq!(fresh, expected, "{shard} ran another set than the map assigns");
        seen = now;
    }
    assert!(!package.join("reference.ran").exists(), "a shard ran the differential partition");
    assert!(!package.join("protocol.ran").exists(), "a shard ran a test the differential filter reaches");

    // A failure reds exactly the one shard holding the failing test.
    for name in ran(&package) {
        std::fs::remove_file(package.join(format!("{name}.ran"))).unwrap();
    }
    let suite = std::fs::read_to_string(package.join("tests/integration/main.rs")).unwrap();
    r.write("crates/contextful-cli/tests/integration/main.rs", &suite.replace("fn ordinary_05() {", "fn ordinary_05() { panic!(\"shard regression\");"));
    r.commit("one failing ordinary test");
    let mut red = Vec::new();
    for shard in &shards {
        let out = r.gate(&["--stage", shard]);
        if !out.status.success() {
            red.push(shard.clone());
            assert!(String::from_utf8_lossy(&out.stdout).contains("shard regression"), "{shard}: {}", stderr(&out));
        }
        for name in ran(&package) {
            std::fs::remove_file(package.join(format!("{name}.ran"))).unwrap();
        }
    }
    assert_eq!(red, [format!("workspace.cli-{}", home(5))], "one failing test reds exactly its mapped shard");
}

/// One libtest JSON event per test with its execution time.
fn timed(name: &str, seconds: f64) -> String {
    format!("{{ \"type\": \"test\", \"name\": \"{name}\", \"event\": \"ok\", \"exec_time\": {seconds} }}\n")
}

/// The balancer writes a map holding every timed test once, the differential ones excepted, keeps
/// each group on one shard, and refuses a report leaving any shard past 9 min.
#[test]
fn the_balancer_spreads_a_timing_report_within_the_shard_budget() {
    let r = Repo::init();
    r.lock();
    let parts = listed(&r);
    let n = cli_shards(&parts).len();
    let mut report = String::from("{ \"type\": \"suite\", \"event\": \"started\", \"test_count\": 3 }\n");
    let names: Vec<String> = (0..4 * n).map(|i| format!("module_{}::case_{i:03}", i % 5)).collect();
    for (i, name) in names.iter().enumerate() {
        report.push_str(&timed(name, 100.0 + i as f64));
    }
    report.push_str(&timed("differential::reference", 5000.0));
    r.write("timings.json", &report);
    r.write("tools/ci/cli-shards.toml", "[group]\ntogether = [\"module_0::\"]\n");
    let out = r.run_ci(&["cli-shards", "--report", "timings.json", "--scale", "2"]);
    assert!(out.status.success(), "{}", stderr(&out));

    let text = std::fs::read_to_string(r.root.join("tools/ci/cli-shards.toml")).unwrap();
    let map: toml::Table = toml::from_str(&text).unwrap();
    let tests = map["test"].as_table().unwrap();
    let mut keys: Vec<&String> = tests.keys().collect();
    keys.sort();
    let mut want: Vec<&String> = names.iter().collect();
    want.sort();
    assert_eq!(keys, want, "the map holds another set than the timed ordinary tests");
    assert!(map["group"]["together"].as_array().is_some_and(|g| g.len() == 1), "the rewrite dropped the curated groups: {text}");
    let shard_of = |name: &str| tests[name]["shard"].as_integer().unwrap() as usize;
    let together: Vec<usize> = names.iter().filter(|n| n.starts_with("module_0::")).map(|n| shard_of(n)).collect();
    assert!(together.len() > 1, "the group holds fewer than two tests: {together:?}");
    assert!(together.windows(2).all(|w| w[0] == w[1]), "the group spans shards: {together:?}");
    let mut load = vec![0.0; n];
    for name in &names {
        let shard = shard_of(name);
        assert!((1..=n).contains(&shard), "{name} on shard {shard}");
        let i: f64 = name.rsplit('_').next().unwrap().parse().unwrap();
        assert!((tests[name]["seconds"].as_float().unwrap() - 2.0 * (100.0 + i)).abs() < 0.1, "{name}: the recorded time is the scaled report time");
        load[shard - 1] += tests[name]["seconds"].as_float().unwrap();
    }
    assert!(load.iter().all(|l| *l > 0.0), "a shard holds no test: {load:?}");

    // The committed map's check prints each shard's estimate and holds it within budget.
    let out = r.run_ci(&["cli-shards"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let printed = String::from_utf8_lossy(&out.stdout);
    assert_eq!(printed.lines().filter(|l| l.starts_with("cli-")).count(), n, "{printed}");

    // Four threads at 540 s each hold 2160 test-seconds a shard: one more test-second refuses.
    let mut heavy = String::new();
    for i in 0..4 * n {
        heavy.push_str(&timed(&format!("heavy::case_{i:03}"), 540.0));
    }
    heavy.push_str(&timed("heavy::straw", 1.0));
    r.write("heavy.json", &heavy);
    let out = r.run_ci(&["cli-shards", "--report", "heavy.json"]);
    assert!(!out.status.success(), "a report beyond the budget wrote a map");
    assert!(stderr(&out).contains("9 min"), "{}", stderr(&out));
    assert_eq!(std::fs::read_to_string(r.root.join("tools/ci/cli-shards.toml")).unwrap(), text, "a refused report rewrote the map");

    // One test past the budget holds a shard alone and still exceeds it: no shard count splits it.
    r.write("giant.json", &(timed("lone::giant", 600.0) + &timed("lone::small", 1.0)));
    let out = r.run_ci(&["cli-shards", "--report", "giant.json"]);
    assert!(!out.status.success(), "a test past the budget wrote a map");
    assert!(stderr(&out).contains("lone::giant") && stderr(&out).contains("9 min"), "{}", stderr(&out));
    assert_eq!(std::fs::read_to_string(r.root.join("tools/ci/cli-shards.toml")).unwrap(), text, "a refused report rewrote the map");
}

/// Runs `contextful-ci cli-shards` against `map` as the committed shard map.
fn check_map(map: Option<&str>) -> std::process::Output {
    let r = Repo::init();
    r.lock();
    if let Some(map) = map {
        r.write("tools/ci/cli-shards.toml", map);
    }
    r.run_ci(&["cli-shards"])
}

/// A group split across shards builds its nested example once per shard, so the map refuses it.
#[test]
fn the_shard_check_refuses_a_group_spanning_two_shards() {
    let map = format!("[group]\nnested = [\"nested::\"]\n\n[test]\n{}{}", mapped("nested::a", 1, 10.0), mapped("nested::b", 2, 10.0));
    let out = check_map(Some(&map));
    assert!(!out.status.success(), "a group spanning two shards passed");
    assert!(stderr(&out).contains("`nested` spans shards 1 and 2"), "{}", stderr(&out));
}

/// A shard's estimate never falls below its longest test: a 600 s test beside a 1 s one exceeds
/// 9 min, though their 601 s over four threads does not.
#[test]
fn the_shard_check_holds_a_shard_to_its_longest_test() {
    let map = format!("[test]\n{}{}", mapped("long::case", 1, 600.0), mapped("short::case", 1, 1.0));
    let out = check_map(Some(&map));
    assert!(!out.status.success(), "a shard holding a 600 s test passed");
    assert!(stderr(&out).contains("`long::case` 600 s"), "{}", stderr(&out));
}

/// Without the map every test would pile onto one shard, so the check and every shard refuse.
#[test]
fn the_shard_check_refuses_a_missing_map() {
    let out = check_map(None);
    assert!(!out.status.success(), "a repository without a shard map passed");
    assert!(stderr(&out).contains("tools/ci/cli-shards.toml"), "{}", stderr(&out));
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
    let out = r.run_ci(&["stages", "--parts", "--tier", "nightly"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let parts: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect();
    assert!(parts.contains(&"formal".to_string()), "{parts:?}");
    assert!(!parts.iter().any(|p| p == "features.packages"), "featured packages still share one part: {parts:?}");
    // Parts follow package-name order: `beta` sorts before `contextful-alpha`.
    assert!(position(&parts, "features.beta") < position(&parts, "features.alpha"), "{parts:?}");
    assert!(position(&parts, "features.alpha") < position(&parts, "formal"), "{parts:?}");

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
    // A workspace run measures nothing; the evaluate stage sets the directory.
    let Some(dir) = std::env::var_os("CONTEXTFUL_MEASURE_DIR").map(std::path::PathBuf::from) else { return };
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
fn the_evaluate_stage_splits_one_part_per_package_group_each_riding_in_its_suite_and_holding_its_own_floor() {
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

    // Each package group's evaluate part rides in that group's suite check, after its
    // workspace part.
    let parts = listed(&r);
    let suites: Vec<String> = ["cli", "foundation", "runtime", "read", "ci"].iter().map(|g| format!("suite.{g}")).collect();
    assert!(parts.contains(&"acceptance".to_string()), "{parts:?}");
    assert!(!parts.iter().any(|p| p == "evaluate" || p.starts_with("evaluate.")), "an evaluate part runs as its own check: {parts:?}");
    for pair in suites.windows(2) {
        assert!(position(&parts, &pair[0]) < position(&parts, &pair[1]), "{parts:?}");
    }
    assert!(position(&parts, &suites[4]) < position(&parts, "acceptance"), "{parts:?}");

    let out = r.gate(&["--stage", "suite.runtime"]);
    let err = stderr(&out);
    assert!(out.status.success(), "{err}");
    assert!(err.contains("--- stage workspace") && err.contains("--- stage evaluate"), "{err}");
    assert!(err.contains("measure: engine-figure = 4") && !err.contains("demo-figure"), "the runtime suite measured beyond its group: {err}");
    let out = r.gate(&["--stage", "suite.ci"]);
    assert!(out.status.success(), "the ci suite with no entry fails: {}", stderr(&out));
    assert!(!stderr(&out).contains("-figure"), "{}", stderr(&out));

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

/// A fold runs each member after an earlier member fails, skips a member reading a failed member's output, and fails when any member fails.
// spec: assurance.gate.fold-verdicts@22eb26ea
#[test]
fn a_fold_reports_every_member_past_a_failing_one() {
    let r = Repo::init();
    r.write("spec/spec.lock.json", "{\"clauses\": [{\"id\": \"run.journal.entry-key\"}]}\n");
    r.write("crates/engine/Cargo.toml", &manifest("contextful-engine", ""));
    r.write("crates/engine/src/lib.rs", "");
    r.write("crates/engine/tests/integration/main.rs", "mod measured;\n");
    // `holds` fails the workspace part; the evaluate part runs the ledger's test alone.
    r.write(
        "crates/engine/tests/integration/measured.rs",
        &format!("{MEASURED}\n#[test]\nfn holds() {{\n    assert!(std::env::var_os(\"CONTEXTFUL_MEASURE_DIR\").is_some(), \"a workspace defect\");\n}}\n"),
    );
    r.write("evals/ledger.toml", &entry("engine-figure", "contextful_engine::measured::measured", "=="));
    r.lock();
    r.commit("a runtime package whose workspace suite fails");

    let out = r.gate(&["--stage", "suite.runtime"]);
    let err = stderr(&out);
    assert!(!out.status.success(), "a fold with a failing member passes: {err}");
    assert!(err.contains("report: stage `workspace` failed"), "{err}");
    assert!(err.contains("--- stage evaluate") && err.contains("measure: engine-figure = 4"), "the failing workspace part hid the evaluate verdict: {err}");

    // A member reading a failed member's output is skipped; the rest still run.
    r.write("formal/protocol/lean-toolchain", "leanprover/lean4:stable\n");
    r.commit("an inexact Lean pin fails the pins stage");
    let out = r.gate(&["--predecessors", "--base", "HEAD", "--stage", "preflight"]);
    let err = stderr(&out);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("report: stage `pins` failed"), "{err}");
    assert!(err.contains("stage `toolchain` skipped: it reads target/gate/pins.json, which failed stage `pins` writes"), "{err}");
    assert!(err.contains("--- stage schema") && err.contains("--- stage test-first"), "the pins failure hid the later members: {err}");
}
