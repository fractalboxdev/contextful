//! `assurance.structure-tree`: one home per capability, duplicated-capability detection
//! across languages, adapters and mirror annotations.

use crate::{stderr, Repo};
use std::process::Output;

const REGISTRY: &str = r#"ports = ["crates/contextful-core/src"]

[[capability]]
name = "enforcement"
home = "crates/contextful-policy/src/enforce"
symbols = ["TablePolicy"]

[[capability]]
name = "control-plane state"
home = "crates/contextful-control/src"
symbols = ["ConfigDoc"]

[[capability]]
name = "credential resolution"
home = "crates/contextful-outbound/src/secrets.rs"
symbols = ["assemble_all"]

[[capability]]
name = "statement guarding"
home = "crates/contextful-core/src/read/guard.rs"
symbols = ["single_statement"]

[[capability]]
name = "visibility"
home = "crates/contextful-core/src/disclosure"
symbols = ["visibility_class"]
"#;

/// An engine tree whose registry resolves: each capability's home defines its symbol, and
/// the enforcement home carries the constant `MASK_CROWD_FLOOR`.
fn engine() -> Repo {
    let r = Repo::init();
    r.write("spec/capabilities.toml", REGISTRY);
    r.write(
        "crates/contextful-policy/src/enforce/mask.rs",
        "pub const MASK_CROWD_FLOOR: u64 = 1_000;\npub struct TablePolicy;\n",
    );
    r.write("crates/contextful-control/src/lib.rs", "pub struct ConfigDoc;\n");
    r.write("crates/contextful-outbound/src/secrets.rs", "pub fn assemble_all() {}\n");
    r.write(
        "crates/contextful-core/src/read/guard.rs",
        "pub fn single_statement(sql: &str) -> &str {\n    sql\n}\n",
    );
    r.write("crates/contextful-core/src/disclosure/sweep.rs", "pub fn visibility_class() {}\n");
    r.write(
        "crates/contextful-core/src/ports.rs",
        "pub trait StatementPort {\n    fn single_statement(&self, sql: &str) -> String;\n}\n",
    );
    r
}

fn lint(r: &Repo) -> Output {
    r.commit("fixture");
    r.run_ci(&["source-lint"])
}

fn refused(out: &Output, code: &str) -> String {
    let err = stderr(out);
    assert!(!out.status.success(), "source lint passed unexpectedly: {err}");
    assert!(err.contains(code), "{err}");
    err
}

fn passed(out: &Output) {
    assert!(out.status.success(), "{}", stderr(out));
}

/// A surface or crate carrying a second implementation of an engine capability, in any language, or a hand-written copy of an engine constant, raises `CapabilityDuplicated`, naming both sites.
// spec: assurance.structure-tree.duplicated-capability@498d38b2
#[test]
fn a_second_implementation_and_a_copied_constant_name_both_sites() {
    let r = engine();
    r.write(
        "apps/gateway/src/guard.ts",
        "import { x } from \"./x\";\n\nexport function singleStatement(sql: string): string {\n  return sql;\n}\n",
    );
    r.write("apps/console/src/limits.ts", "export const MASK_CROWD_FLOOR = 1000;\n");

    let err = refused(&lint(&r), "CapabilityDuplicated");
    assert!(err.contains("apps/gateway/src/guard.ts:3"), "{err}");
    assert!(err.contains("crates/contextful-core/src/read/guard.rs:1"), "{err}");
    assert!(err.contains("apps/console/src/limits.ts:1"), "{err}");
    assert!(err.contains("crates/contextful-policy/src/enforce/mask.rs:1"), "{err}");
}

/// The crate-graph stage matches Rust, TypeScript, JavaScript and Python functions, types and constants outside a home by name folded to lowercase without separators, against its symbols and its literal constants' names and values.
// spec: assurance.structure-tree.cross-language@9f1d90cf
#[test]
fn definitions_match_by_folded_name_in_every_scanned_language() {
    let r = engine();
    r.write("crates/demo/src/policy.rs", "pub struct TablePolicy {\n    pub rows: u32,\n}\n");
    r.write("tools/audit/check.py", "import os\n\ndef visibility_class(f):\n    return f\n");
    r.write("packages/client/src/config.mjs", "export class ConfigDoc {}\n");
    r.write("apps/gateway/src/secrets.js", "const assembleAll = (vars) => vars;\n");

    let err = refused(&lint(&r), "CapabilityDuplicated");
    for site in [
        "crates/demo/src/policy.rs:1",
        "tools/audit/check.py:3",
        "packages/client/src/config.mjs:1",
        "apps/gateway/src/secrets.js:1",
    ] {
        assert!(err.contains(site), "{site} missing: {err}");
    }
}

#[test]
fn a_constant_matches_on_name_and_value_together() {
    let r = engine();
    r.write("apps/console/src/limits.ts", "export const MASK_CROWD_FLOOR = 500;\nexport const CROWD = 1000;\n");
    r.write("tools/audit/limits.py", "MASK_CROWD_FLOOR: int = 2_000\n");
    passed(&lint(&r));

    r.write("tools/audit/limits.py", "MASK_CROWD_FLOOR = 1_000\n");
    let err = refused(&lint(&r), "CapabilityDuplicated");
    assert!(err.contains("tools/audit/limits.py:1"), "{err}");
}

#[test]
fn tests_type_declarations_and_calls_define_nothing() {
    let r = engine();
    r.write("apps/gateway/test/guard.test.ts", "export function singleStatement() {}\n");
    r.write("crates/demo/tests/integration/guard.rs", "fn single_statement() {}\n");
    r.write(
        "crates/demo/src/lib.rs",
        "pub fn double(x: i32) -> i32 {\n    x * 2\n}\n#[cfg(test)]\nmod tests {\n    fn single_statement() {}\n}\n",
    );
    r.write("apps/gateway/src/wire.ts", "export type TablePolicy = { rows: number };\nexport interface ConfigDoc { text: string }\n");
    passed(&lint(&r));
}

/// Enforcement, control-plane state, credential resolution, statement guarding and visibility each resolve to one implementation in the engine; a surface reaches one through the engine route or the registered view compiling it in.
// spec: assurance.structure-tree.one-home@d347a98b
#[test]
fn a_surface_reaching_the_one_home_through_the_engine_passes() {
    let r = engine();
    r.write(
        "crates/demo/src/lib.rs",
        "use contextful_core::read::guard::single_statement;\npub fn guarded(sql: &str) -> &str {\n    single_statement(sql)\n}\n",
    );
    r.write("apps/gateway/src/guard.ts", "export async function guarded(sql: string) {\n  return fetch(`/v1/guard?sql=${sql}`);\n}\n");
    let out = lint(&r);
    passed(&out);
    assert!(String::from_utf8_lossy(&out.stdout).contains("capabilities: 5 homes"), "{}", String::from_utf8_lossy(&out.stdout));
}

/// `spec/capabilities.toml` names each capability of {{assurance.structure-tree.one-home}} with one home path and its defining symbols; an absent registry, a missing or doubled capability, an absent home or an undefined symbol raises `CapabilityHomeUnresolved`.
// spec: assurance.structure-tree.capability-registry@ffb5a6fc
#[test]
fn a_registry_resolving_to_no_home_is_refused() {
    let r = engine();
    let doubled = REGISTRY
        .replace("symbols = [\"single_statement\"]", "symbols = [\"single_statement\", \"guard_everything\"]")
        .replace("home = \"crates/contextful-control/src\"", "home = \"crates/contextful-gone/src\"")
        .replace("name = \"visibility\"", "name = \"enforcement\"");
    r.write("spec/capabilities.toml", &doubled);
    let err = refused(&lint(&r), "CapabilityHomeUnresolved");
    assert!(err.contains("guard_everything"), "{err}");
    assert!(err.contains("crates/contextful-gone/src"), "{err}");
    assert!(err.contains("`visibility` names no home"), "{err}");
    assert!(err.contains("`enforcement` names 2 homes"), "{err}");

    std::fs::remove_file(r.root.join("spec/capabilities.toml")).unwrap();
    passed(&lint(&r));
    r.write("spec/spec.lock.json", "{\"clauses\": []}\n");
    let err = refused(&lint(&r), "CapabilityHomeUnresolved");
    assert!(err.contains("spec/capabilities.toml is absent"), "{err}");
}

/// A surface with no engine process on its request path implements an adapter against the engine's ports.
// spec: assurance.structure-tree.adapter@998d98b4
#[test]
fn an_adapter_implementing_an_engine_port_passes_where_a_restatement_is_refused() {
    let r = engine();
    r.write(
        "crates/demo/src/remote.rs",
        "use contextful_core::ports::StatementPort;\npub struct Remote;\nimpl StatementPort for Remote {\n    fn single_statement(&self, sql: &str) -> String {\n        sql.to_string()\n    }\n}\n",
    );
    passed(&lint(&r));

    r.write(
        "crates/demo/src/remote.rs",
        "pub trait OwnPort {\n    fn single_statement(&self, sql: &str) -> String;\n}\npub struct Remote;\nimpl OwnPort for Remote {\n    fn single_statement(&self, sql: &str) -> String {\n        sql.to_string()\n    }\n}\n",
    );
    let err = refused(&lint(&r), "CapabilityDuplicated");
    assert!(err.contains("crates/demo/src/remote.rs:6"), "{err}");
}

/// A review-time restatement of a build-time rule is admitted where its site carries a `mirrors: <clause id>` comment naming the clause it restates; an unannotated restatement stays {{assurance.structure-tree.duplicated-capability}}.
// spec: assurance.structure-tree.mirror-exemption@25fe481e
#[test]
fn a_mirrored_restatement_is_admitted_and_an_unannotated_one_is_refused() {
    let r = engine();
    r.write(
        "apps/console/src/limits.ts",
        "// mirrors: assurance.structure-tree.one-home\n// The console previews the crowd floor before submit.\nexport const MASK_CROWD_FLOOR = 1000;\n",
    );
    r.write("tools/audit/guard.py", "def single_statement(sql):  # mirrors: assurance.structure-tree.one-home\n    return sql\n");
    passed(&lint(&r));

    r.write("apps/console/src/limits.ts", "// The console previews the crowd floor before submit.\nexport const MASK_CROWD_FLOOR = 1000;\n");
    let err = refused(&lint(&r), "CapabilityDuplicated");
    assert!(err.contains("apps/console/src/limits.ts:2"), "{err}");
    assert!(!err.contains("tools/audit/guard.py"), "{err}");
}

#[test]
fn the_crate_graph_stage_runs_the_capability_check() {
    let r = engine();
    r.write("apps/gateway/src/guard.ts", "export function singleStatement(sql: string) {\n  return sql;\n}\n");
    r.write("Cargo.toml", "[workspace]\nresolver = \"2\"\nmembers = [\"crates/demo\"]\n");
    r.lock();
    r.commit("fixture");
    let out = r.gate(&["--stage", "crate-graph"]);
    let err = refused(&out, "CapabilityDuplicated");
    assert!(err.contains("apps/gateway/src/guard.ts:1"), "{err}");
}

#[test]
fn the_repository_tree_holds_one_home_per_capability() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .arg("source-lint")
        .current_dir(&root)
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap();
    passed(&out);
    assert!(String::from_utf8_lossy(&out.stdout).contains("capabilities: 5 homes"), "{}", String::from_utf8_lossy(&out.stdout));
}
