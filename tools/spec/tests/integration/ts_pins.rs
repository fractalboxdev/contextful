//! TypeScript surface tests carry clause pins only when the surface gate executes them.

use crate::{codes, Scratch};

const CLAUSE: &str = "corpus.anatomy.statement-words";
const REV: &str = "11227773";
const FILE: &str = "apps/console/test/pin.test.ts";

fn fixture() -> Scratch {
    let s = Scratch::copy();
    s.write("apps/console/package.json", r#"{"name":"@contextful/console","type":"module","scripts":{"test":"node --experimental-strip-types --test test/*.test.ts"}}"#);
    s
}

fn test_source(rev: &str, call: &str, body: &str) -> String {
    format!("import {{ test }} from 'node:test';\nimport assert from 'node:assert/strict';\n// spec: {CLAUSE}@{rev}\n{call}('words over forty refused', () => {{ {body} }});\n")
}

fn verdict(s: &Scratch) -> String {
    assert!(s.cmd(&["state"]).status.success());
    s.read("spec/status.md").lines().find(|l| l.starts_with(&format!("| `{CLAUSE}` |")))
        .unwrap().trim_end_matches(" |").rsplit("| ").next().unwrap().to_string()
}

#[test]
fn a_running_surface_test_tag_performs_its_clause() {
    let s = fixture();
    s.write(FILE, &test_source(REV, "test", "assert.equal(1 + 1, 2);"));
    assert!(codes(&s.lint("state"), "SpecBrokenPin").is_empty());
    assert_eq!(verdict(&s), "performed");
}

#[test]
fn a_skipped_or_todo_surface_test_is_broken() {
    for call in ["test.skip", "test.todo"] {
        let s = fixture();
        s.write(FILE, &test_source(REV, call, "assert.ok(true);"));
        let broken = codes(&s.lint("state"), "SpecBrokenPin");
        assert_eq!(broken.len(), 1, "{call}: {broken:?}");
        assert_eq!(verdict(&s), "broken");
    }
}

#[test]
fn an_empty_or_placeholder_surface_test_is_broken() {
    for body in ["", "// TODO\n", "return;", "throw new Error('not implemented');"] {
        let s = fixture();
        s.write(FILE, &test_source(REV, "test", body));
        assert_eq!(codes(&s.lint("state"), "SpecBrokenPin").len(), 1, "{body}");
    }
}

#[test]
fn a_stale_surface_tag_raises_spec_stale_pin() {
    let s = fixture();
    s.write(FILE, &test_source("deadbeef", "test", "assert.ok(true);"));
    assert_eq!(codes(&s.lint("state"), "SpecStalePin").len(), 1);
    assert_eq!(verdict(&s), "broken");
}

#[test]
fn a_surface_tag_without_an_attached_test_is_broken() {
    let s = fixture();
    s.write(FILE, &format!("// spec: {CLAUSE}@{REV}\nexport const value = 1;\n"));
    assert_eq!(codes(&s.lint("state"), "SpecBrokenPin").len(), 1);
}

#[test]
fn a_surface_tag_outside_a_runnable_package_is_broken() {
    let s = Scratch::copy();
    s.write(FILE, &test_source(REV, "test", "assert.ok(true);"));
    assert_eq!(codes(&s.lint("state"), "SpecBrokenPin").len(), 1);
}

#[test]
fn a_surface_test_in_source_is_not_run_by_the_gate() {
    let s = fixture();
    s.write("apps/console/src/pin.test.ts", &test_source(REV, "test", "assert.equal(1, 1);"));
    assert_eq!(codes(&s.lint("state"), "SpecBrokenPin").len(), 1);
}

#[test]
fn a_native_surface_test_needs_no_package_test_script() {
    let s = Scratch::copy();
    s.write("apps/console/package.json", r#"{"name":"@contextful/console","type":"module"}"#);
    s.write(FILE, &test_source(REV, "test", "assert.equal(1 + 1, 2);"));
    assert_eq!(verdict(&s), "performed");
}

#[test]
fn an_assertion_spelled_only_inside_a_string_does_not_perform() {
    let s = fixture();
    s.write(FILE, &test_source(REV, "test", "const note = 'assert.equal(1, 1)';"));
    assert_eq!(codes(&s.lint("state"), "SpecBrokenPin").len(), 1);
}
