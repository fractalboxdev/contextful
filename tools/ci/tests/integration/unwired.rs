//! `corpus.state.unwired`: a public item only a performed pin's test calls.

use crate::{stderr, Repo};
use std::process::{Command, Output};

const LOCK: &str = r#"{"clauses": [], "pins": {
  "store.replicate.periodic": [{"kind": "test", "path": "crates/demo/tests/integration/replicate.rs", "name": "replicates", "line": 1, "verdict": "performed", "proved": false}],
  "store.replicate.broken": [{"kind": "test", "path": "crates/demo/tests/integration/replicate.rs", "name": "broken", "line": 6, "verdict": "broken", "proved": false}]
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

// spec: corpus.state.unwired@79b919c7
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
fn a_contract_filter_reports_only_that_contracts_clauses() {
    let r = fixture();
    let o = unwired(&r, &["--contract", "disclosure"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(!unwired(&r, &["--contract", "store"]).status.success());
}
