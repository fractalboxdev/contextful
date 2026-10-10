//! `assurance.structure-tree.mirror-unresolved`.

use crate::{stderr, Repo};
use std::process::{Command, Output};

const LOCK: &str = "{\"clauses\": [{\"id\": \"assurance.gate.schema-stage\"}]}\n";

fn mirrors(r: &Repo) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .arg("mirrors")
        .current_dir(&r.root)
        .output()
        .unwrap()
}

#[test]
fn an_annotation_naming_no_clause_is_refused() {
    let r = Repo::init();
    r.write("spec/spec.lock.json", LOCK);
    r.write("apps/web/src/check.ts", "export const x = 1;\n// mirrors: assurance.gate.nowhere\nexport const y = 2;\n");
    r.write("tools/lint/check.sh", "#!/bin/sh\n# mirrors:\ntrue\n");
    r.commit("mirrors");
    let o = mirrors(&r);
    assert!(!o.status.success());
    let err = stderr(&o);
    assert!(err.contains("MirrorUnresolved"), "{err}");
    assert!(err.contains("apps/web/src/check.ts:2"), "{err}");
    assert!(err.contains("tools/lint/check.sh:2"), "{err}");
}

// spec: topology.bound-application.restated-case@16b746a9
#[test]
fn an_annotation_naming_a_clause_passes() {
    let r = Repo::init();
    r.write("spec/spec.lock.json", LOCK);
    r.write("apps/web/src/check.ts", "// mirrors: assurance.gate.schema-stage\nexport const x = 1;\n");
    r.write("crates/demo/src/extra.rs", "fn f() {} // mirrors: not.an.annotation\n");
    r.commit("mirrors");
    let o = mirrors(&r);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(String::from_utf8_lossy(&o.stdout).contains("1 annotation(s)"), "{}", String::from_utf8_lossy(&o.stdout));
}

#[test]
fn the_schema_stage_runs_the_mirror_check() {
    let r = Repo::init();
    r.write("spec/spec.lock.json", LOCK);
    r.write("crates/demo/src/extra.rs", "// mirrors: assurance.gate.nowhere\n");
    r.commit("mirrors");
    let o = r.gate(&["--stage", "schema"]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("MirrorUnresolved"), "{}", stderr(&o));
}
