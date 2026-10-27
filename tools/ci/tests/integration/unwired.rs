//! `corpus.state.unwired`: a public item only a performed pin's test calls.

use crate::{stderr, Repo};
use std::process::{Command, Output};

const LOCK: &str = r#"{"clauses": [], "pins": {
  "store.replicate.periodic": [{"kind": "test", "path": "crates/demo/tests/integration/replicate.rs", "name": "replicates", "verdict": "performed", "proved": false}],
  "store.replicate.broken": [{"kind": "test", "path": "crates/demo/tests/integration/replicate.rs", "name": "broken", "verdict": "broken", "proved": false}]
}}
"#;

const LIB: &str = "mod replica;\npub use replica::{spawn, INTERVAL};\n";
const REPLICA: &str = "pub const INTERVAL: u64 = 600;\n\npub fn spawn(every: u64) -> u64 {\n    every\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn spawns() {\n        assert_eq!(super::spawn(super::INTERVAL), 600);\n    }\n}\n";
const TEST: &str = "#[test]\nfn replicates() {\n    assert_eq!(demo::replica_spawn(demo::INTERVAL), 600);\n}\n\n#[test]\nfn broken() {\n    demo::spawn(1);\n}\n";

fn unwired(r: &Repo, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful-ci")).arg("unwired").args(args).current_dir(&r.root).output().unwrap()
}

fn fixture() -> Repo {
    let r = Repo::init();
    r.write("spec/spec.lock.json", LOCK);
    r.write("crates/demo/src/lib.rs", LIB);
    r.write("crates/demo/src/replica.rs", &REPLICA.replace("pub fn spawn", "pub fn replica_spawn"));
    r.write("crates/demo/tests/integration/replicate.rs", TEST);
    r
}

// spec: corpus.state.unwired@9ed1dfd5
#[test]
fn an_item_only_a_performed_test_calls_is_unwired_until_a_binary_calls_it_or_marks_its_entry() {
    let r = fixture();
    let o = unwired(&r, &[]);
    assert!(!o.status.success());
    let err = stderr(&o);
    assert!(err.contains("SpecUnwired"), "{err}");
    assert!(err.contains("store.replicate.periodic") && err.contains("replica_spawn") && err.contains("INTERVAL"), "{err}");
    assert!(!err.contains("store.replicate.broken"), "a broken pin claims nothing: {err}");

    r.write("crates/demo/src/main.rs", "fn main() {\n    demo::replica_spawn(1);\n}\n");
    let err = stderr(&unwired(&r, &[]));
    assert!(!err.contains("replica_spawn"), "{err}");
    assert!(err.contains("INTERVAL"), "{err}");

    r.write("crates/demo/src/main.rs", "fn main() {\n    // entry: store.replicate.periodic\n    demo::replica_spawn(1);\n}\n");
    let o = unwired(&r, &[]);
    assert!(o.status.success(), "{}", stderr(&o));
}

#[test]
fn only_cfg_test_and_cfg_all_test_gate_an_item_to_tests() {
    for gate in ["#[cfg(not(test))]", "#[cfg(any(test, feature = \"cli\"))]"] {
        let r = fixture();
        r.write("crates/demo/src/main.rs", &format!("{gate}\nfn main() {{\n    demo::replica_spawn(1);\n}}\n"));
        let err = stderr(&unwired(&r, &[]));
        assert!(!err.contains("replica_spawn"), "{gate} gates a production item: {err}");
    }
    for gate in ["#[cfg(test)]", "#[cfg(all(test, feature = \"cli\"))]"] {
        let r = fixture();
        r.write("crates/demo/src/main.rs", &format!("fn main() {{}}\n\n{gate}\nfn probe() {{\n    demo::replica_spawn(1);\n}}\n"));
        let err = stderr(&unwired(&r, &[]));
        assert!(err.contains("replica_spawn"), "{gate} gates a test item: {err}");
    }
}

#[test]
fn a_contract_filter_reports_only_that_contracts_clauses() {
    let r = fixture();
    let o = unwired(&r, &["--contract", "disclosure"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(!unwired(&r, &["--contract", "store"]).status.success());
}

// spec: corpus.state.unwired-ceiling@57f2aa28
#[test]
fn the_count_holds_under_a_ceiling_that_only_falls() {
    let r = fixture();
    r.write("tools/ci/unwired.toml", "ceiling = 2\n");
    let o = unwired(&r, &[]);
    assert!(o.status.success(), "two unwired items sit at a ceiling of 2: {}", stderr(&o));
    r.write("tools/ci/unwired.toml", "ceiling = 1\n");
    let o = unwired(&r, &[]);
    assert!(!o.status.success() && stderr(&o).contains("SpecUnwired"), "{}", stderr(&o));
    r.write("tools/ci/unwired.toml", "ceiling = 5\n");
    assert!(unwired(&r, &[]).status.success());
    assert!(unwired(&r, &["--lower"]).status.success());
    assert_eq!(std::fs::read_to_string(r.root.join("tools/ci/unwired.toml")).unwrap(), "ceiling = 2\n");
    r.commit("an unwired fixture under its ceiling");
    let o = r.gate(&["--stage", "unwired"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stderr(&o).contains("--- stage unwired"), "{}", stderr(&o));
}

#[test]
fn an_item_of_the_acceptance_harness_is_test_support_and_never_unwired() {
    let r = fixture();
    r.write("crates/demo/src/main.rs", "fn main() {\n    // entry: store.replicate.periodic\n    demo::replica_spawn(1);\n}\n");
    r.write("crates/acceptance/src/lib.rs", "pub fn harness_repo() -> u8 {\n    1\n}\n");
    r.write("crates/demo/tests/integration/replicate.rs", &TEST.replace("#[test]\nfn broken", "#[test]\nfn harness() {\n    contextful_acceptance::harness_repo();\n}\n\n#[test]\nfn broken"));
    let lock = LOCK.replace("\"store.replicate.broken\"", "\"store.replicate.harness\": [{\"kind\": \"test\", \"path\": \"crates/demo/tests/integration/replicate.rs\", \"name\": \"harness\", \"line\": 6, \"verdict\": \"performed\", \"proved\": false}],\n  \"store.replicate.broken\"");
    r.write("spec/spec.lock.json", &lock);
    let o = unwired(&r, &[]);
    assert!(o.status.success(), "{}", stderr(&o));
}
