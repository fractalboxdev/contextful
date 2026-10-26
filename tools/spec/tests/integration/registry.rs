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
    let findings = registry(&s);
    assert!(findings.is_empty(), "{findings:?}");
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

const GLOSS: &str = "A commit would expose snapshot data without its declared sidecars, or the reverse.";

fn drift(s: &Scratch) -> Vec<String> {
    codes(&s.lint("registry"), "SpecRegistryDrift")
}

/// The drift findings whose message holds `needle`.
fn mentions(s: &Scratch, needle: &str) -> Vec<String> {
    drift(s).into_iter().filter(|m| m.contains(needle)).collect()
}

/// A `StoreError` source file holding `variants`, each a `///` gloss line and a name.
fn store_enum(s: &Scratch, rel: &str, variants: &[(&str, &str, &str)]) {
    let mut text = String::from("#[derive(Debug, thiserror::Error)]\npub enum StoreError {\n");
    for (gloss, id, name) in variants {
        if !gloss.is_empty() {
            text.push_str(&format!("    /// {gloss}\n"));
        }
        text.push_str(&format!("    #[error(\"{id}: {{0}}\")]\n    {name}(String),\n"));
    }
    text.push_str("}\n");
    s.write(rel, &text);
}

/// Pin every refusal and limit clause of `store.fold` with a finished test.
fn pin_fold(s: &Scratch) {
    s.scaffold("store.fold", "crates/x");
    let rel = "crates/x/tests/integration/fold.rs";
    let text: Vec<String> =
        s.read(rel).lines().map(|l| if l.trim_start().starts_with("todo!") { "    assert_eq!(1 + 1, 2);".into() } else { l.to_string() }).collect();
    s.write(rel, &(text.join("\n") + "\n"));
}

// spec: corpus.registry.defining-site@f500d9fb
#[test]
fn a_variant_a_quoted_literal_or_a_lean_message_defines_an_error() {
    let s = Scratch::copy();
    pin_fold(&s);
    let found = drift(&s);
    assert!(found.iter().any(|m| m.contains("performed refusal `store.fold.partial-snapshot` raises `StorePartialSnapshot`, which no source defines")), "{found:?}");

    store_enum(&s, "crates/x/src/error.rs", &[(GLOSS, "StorePartialSnapshot", "StorePartialSnapshot")]);
    assert!(mentions(&s, "`StorePartialSnapshot`").is_empty(), "{:?}", drift(&s));

    std::fs::remove_file(s.root.join("crates/x/src/error.rs")).unwrap();
    s.write("apps/x/src/index.ts", "export const refusal = \"StorePartialSnapshot\";\n");
    assert!(mentions(&s, "`StorePartialSnapshot`").is_empty(), "{:?}", drift(&s));

    std::fs::remove_file(s.root.join("apps/x/src/index.ts")).unwrap();
    s.write("formal/x/Main.lean", "def main : IO Unit := IO.eprintln s!\"StorePartialSnapshot: {why}\"\n");
    assert!(mentions(&s, "`StorePartialSnapshot`").is_empty(), "{:?}", drift(&s));

    std::fs::remove_file(s.root.join("formal/x/Main.lean")).unwrap();
    s.write("crates/x/src/lib.rs", "// StorePartialSnapshot in a comment defines nothing\n");
    assert!(drift(&s).iter().any(|m| m.contains("`StorePartialSnapshot`, which no source defines")), "{:?}", drift(&s));
}

// spec: corpus.registry.variant-shape@9632e31a
#[test]
fn a_second_variant_a_foreign_name_or_a_mixed_enum_is_drift() {
    let s = Scratch::copy();
    store_enum(&s, "crates/x/src/a.rs", &[("", "StorePartialSnapshot", "StorePartialSnapshot")]);
    store_enum(&s, "crates/y/src/b.rs", &[("", "StorePartialSnapshot", "StorePartialSnapshot")]);
    let found = drift(&s);
    assert!(found.iter().any(|m| m.contains("`StorePartialSnapshot` is defined by two variants")), "{found:?}");

    let s = Scratch::copy();
    store_enum(&s, "crates/x/src/a.rs", &[("", "StorePartialSnapshot", "PartialSnapshot")]);
    assert!(mentions(&s, "PartialSnapshot`; name it").is_empty(), "{:?}", drift(&s));
    store_enum(&s, "crates/x/src/a.rs", &[("", "StorePartialSnapshot", "Torn")]);
    let found = drift(&s);
    assert!(found.iter().any(|m| m.contains("variant `StoreError::Torn` defines `StorePartialSnapshot`; name it `StorePartialSnapshot` or `PartialSnapshot`")), "{found:?}");

    let s = Scratch::copy();
    store_enum(&s, "crates/x/src/a.rs", &[("", "StorePartialSnapshot", "StorePartialSnapshot"), ("", "StoreKeyWidened", "StoreKeyWidened"), ("", "ReadPartUnavailable", "ReadPartUnavailable")]);
    let found = drift(&s);
    assert!(found.iter().any(|m| m.contains("enum `StoreError` holds `store` errors and `ReadPartUnavailable` of `read`")), "{found:?}");
}

// spec: corpus.registry.variant-gloss@7d9522c5
#[test]
fn a_variant_gloss_differing_from_the_registry_is_drift() {
    let s = Scratch::copy();
    pin_fold(&s);
    let pointer = format!("{GLOSS} (`store.fold.partial-snapshot`) The message names the table.");
    store_enum(&s, "crates/x/src/a.rs", &[(&pointer, "StorePartialSnapshot", "StorePartialSnapshot")]);
    assert!(mentions(&s, "StorePartialSnapshot").is_empty(), "{:?}", drift(&s));

    store_enum(&s, "crates/x/src/a.rs", &[("A torn commit. (`store.fold.partial-snapshot`)", "StorePartialSnapshot", "StorePartialSnapshot")]);
    let found: Vec<String> = drift(&s).into_iter().filter(|m| m.contains("StorePartialSnapshot")).collect();
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("`StorePartialSnapshot` variant gloss `A torn commit.` differs from its registry gloss"), "{found:?}");
}

// spec: corpus.registry.drift@c7ca5fc8
#[test]
fn a_committed_refusal_whose_error_has_a_defining_site_is_drift() {
    let s = Scratch::copy();
    store_enum(&s, "crates/x/src/a.rs", &[(GLOSS, "StorePartialSnapshot", "StorePartialSnapshot")]);
    let found = drift(&s);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("committed refusal `store.fold.partial-snapshot` raises `StorePartialSnapshot`, which crates/x/src/a.rs defines; pin the clause"), "{found:?}");
    pin_fold(&s);
    assert!(mentions(&s, "`StorePartialSnapshot`").is_empty(), "{:?}", drift(&s));
}

/// Set the `registry-committed-sites` ceiling in the scratch fragment.
fn set_ceiling(s: &Scratch, value: usize) {
    edit_corpus(s, "registry-committed-sites ", &format!("registry-committed-sites = {value}"));
}

fn edit_corpus(s: &Scratch, prefix: &str, line: &str) {
    let text = s.read("spec/terms/corpus.toml");
    let old = text.lines().find(|l| l.starts_with(prefix)).unwrap_or_else(|| panic!("no `{prefix}` line")).to_string();
    s.write("spec/terms/corpus.toml", &text.replacen(&old, line, 1));
}

// spec: corpus.registry.ceiling@f33dc59d
#[test]
fn a_count_off_its_ceiling_is_a_finding_until_ceilings_lowers_it() {
    let s = Scratch::copy();
    store_enum(&s, "crates/x/src/a.rs", &[(GLOSS, "StorePartialSnapshot", "StorePartialSnapshot")]);
    set_ceiling(&s, 1);
    assert!(drift(&s).is_empty(), "{:?}", drift(&s));

    set_ceiling(&s, 2);
    let found = drift(&s);
    assert_eq!(found, ["ratchet `registry-committed-sites` counts 1, under its ceiling 2; run `contextful-spec ceilings`"]);
    assert!(s.cmd(&["ceilings"]).status.success());
    assert!(s.read("spec/terms/corpus.toml").lines().any(|l| l == "registry-committed-sites = 1"));
    assert!(drift(&s).is_empty(), "{:?}", drift(&s));

    set_ceiling(&s, 0);
    let out = s.cmd(&["ceilings"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("a ceiling never rises"), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(s.read("spec/terms/corpus.toml").lines().any(|l| l == "registry-committed-sites = 0"));
}
