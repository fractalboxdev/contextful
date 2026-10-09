//! `corpus.registry`: the fragment each contract owns, the one clause an error or a bound
//! belongs to, and the findings an unregistered spelling raises.

use crate::{codes, Scratch};

const STORE: &str = "spec/10-store.md";
const FRAGMENT: &str = "spec/terms/store.toml";

fn registry(s: &Scratch) -> Vec<String> {
    codes(&s.lint("registry"), "SpecRegistry")
}

/// Append `text` to the statement of the store item with subject `subject`.
fn extend(s: &Scratch, subject: &str, text: &str) {
    let body = s.read(STORE);
    let row = body.lines().find(|l| l.starts_with(&format!("- `{subject}` — "))).expect("the clause item").to_string();
    s.write(STORE, &body.replacen(&row, &format!("{row} {text}"), 1));
}

/// Replace the fragment line starting `prefix` by `f(line)`.
fn edit_entry(s: &Scratch, prefix: &str, f: impl Fn(&str) -> String) {
    let text = s.read(FRAGMENT);
    let line = text.lines().find(|l| l.starts_with(prefix)).unwrap_or_else(|| panic!("no `{prefix}` entry")).to_string();
    s.write(FRAGMENT, &text.replacen(&line, &f(&line), 1));
}

// spec: corpus.registry.fragment@53fd0f64
#[test]
fn each_contract_registers_its_operations_errors_and_bounds_in_its_own_fragment() {
    let s = Scratch::copy();
    assert!(registry(&s).is_empty());
    assert!(s.cmd(&["extract"]).status.success());
    let lock: serde_json::Value = serde_json::from_str(&s.read("spec/spec.lock.json")).unwrap();
    let store = &lock["registry"]["fragments"]["store"];
    assert!(store.to_string().contains("StorePartialSnapshot"), "{store}");
    assert!(store.to_string().contains("store-compaction-run-count"), "{store}");

    std::fs::remove_file(s.root.join(FRAGMENT)).unwrap();
    let found = registry(&s);
    assert!(found.iter().any(|m| m.contains("owned operation `store.fold` is not registered")), "{found:?}");
}

// spec: corpus.registry.one-error-one-clause@a39d00b8
#[test]
fn an_error_named_by_a_second_clause_or_dropped_by_its_own_is_a_registry_finding() {
    let s = Scratch::copy();
    extend(&s, "includes-runs", "A torn read raises `StorePartialSnapshot`.");
    let found = registry(&s);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("`store.fold.includes-runs` names error `StorePartialSnapshot` owned by `store.fold.partial-snapshot`"), "{found:?}");

    let s = Scratch::copy();
    edit_entry(&s, "StorePartialSnapshot ", |l| l.replace("store.fold.partial-snapshot", "store.fold.includes-runs"));
    let found = registry(&s);
    assert!(found.iter().any(|m| m.contains("`store.fold.includes-runs` does not name its error `StorePartialSnapshot`")), "{found:?}");
}

// spec: corpus.registry.bound-entry@34ea55a8
#[test]
fn a_bound_off_its_unit_basis_or_statement_value_is_a_registry_finding() {
    let s = Scratch::copy();
    edit_entry(&s, "store-compaction-run-count ", |l| l.replace("unit = \"runs\"", "unit = \"furlongs\""));
    let found = registry(&s);
    assert!(found.iter().any(|m| m.contains("`store-compaction-run-count` unit `furlongs` is not a canonical unit")), "{found:?}");

    let s = Scratch::copy();
    edit_entry(&s, "store-compaction-run-count ", |l| l.replace("basis = \"chosen\"", "basis = \"guessed\""));
    let found = registry(&s);
    assert!(found.iter().any(|m| m.contains("basis `guessed` is not chosen")), "{found:?}");

    let s = Scratch::copy();
    edit_entry(&s, "store-compaction-run-count ", |l| l.replace("value = 50", "value = 51"));
    let found = registry(&s);
    assert!(found.iter().any(|m| m.contains("`store.fold.triggers` does not carry its bound `store-compaction-run-count` as `51 runs`")), "{found:?}");
}

// spec: corpus.registry.unregistered@c38f02e6
#[test]
fn an_unregistered_error_or_an_entry_naming_a_missing_clause_is_a_registry_finding() {
    let s = Scratch::copy();
    assert!(registry(&s).is_empty());
    extend(&s, "includes-runs", "A torn read raises `StoreNoSuchError`.");
    let found = registry(&s);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("raises unregistered error `StoreNoSuchError`"), "{found:?}");

    let s = Scratch::copy();
    edit_entry(&s, "StorePartialSnapshot ", |l| l.replace("store.fold.partial-snapshot", "store.fold.no-such-clause"));
    let found = registry(&s);
    assert!(found.iter().any(|m| m.contains("error `StorePartialSnapshot` names missing clause `store.fold.no-such-clause`")), "{found:?}");
}
