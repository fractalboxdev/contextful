//! The lighter clause grammar: rationale words, modals, shared phrases and unregistered
//! identifiers are admitted; scenarios attach examples to a clause.

use crate::{codes, Scratch};

const STORE: &str = "spec/10-store.md";

fn all_codes(s: &Scratch) -> Vec<(String, String)> {
    ["registry", "reference", "rationale", "anatomy"].iter().flat_map(|c| s.lint(c)).collect()
}

/// Insert `text` after the clause list of `## <operation>` in the store file.
fn after_list(s: &Scratch, operation: &str, text: &str) {
    let body = s.read(STORE);
    let head = format!("## {operation}\n");
    let at = body.find(&head).expect("operation section");
    let list = body[at..].find("\n- `").map(|i| at + i).expect("a clause list");
    let list_end = body[list..].find("\n\n").map(|i| list + i).unwrap();
    s.write(STORE, &format!("{}\n\n{text}{}", &body[..list_end], &body[list_end..]));
}

#[test]
fn prose_may_carry_reasons_and_modals() {
    let s = Scratch::copy();
    after_list(&s, "fold", "A pass must finish at least once a day, because a reader always expects a folded snapshot.");
    let found = all_codes(&s);
    assert!(codes(&found, "SpecRationaleLeak").is_empty(), "{found:?}");
    assert!(codes(&found, "SpecStrayModal").is_empty(), "{found:?}");
}

#[test]
fn two_statements_may_share_a_phrase() {
    let s = Scratch::copy();
    let body = s.read(STORE);
    let row = body.lines().find(|l| l.starts_with("- `includes-runs` — ")).unwrap().to_string();
    let copy = row.replace("`includes-runs`", "`includes-runs-again`");
    s.write(STORE, &body.replacen(&row, &format!("{row}\n{copy}"), 1));
    assert!(codes(&all_codes(&s), "SpecRestatement").is_empty());
}

/// Append `text` to the statement of the first clause item starting with `prefix`.
fn extend_statement(s: &Scratch, file: &str, prefix: &str, text: &str) {
    let body = s.read(file);
    let item = body.lines().find(|l| l.starts_with(prefix)).expect("a clause item").to_string();
    s.write(file, &body.replacen(&item, &format!("{item} {text}"), 1));
}

#[test]
fn an_identifier_shared_across_contracts_needs_no_registration() {
    let s = Scratch::copy();
    extend_statement(&s, STORE, "- `includes-runs` — ", "It reads `unregistered_shared_token`.");
    extend_statement(&s, "spec/30-run.md", "- `", "It reads `unregistered_shared_token`.");
    let found = codes(&all_codes(&s), "SpecRegistry");
    assert!(found.iter().all(|m| !m.contains("unregistered_shared_token")), "{found:?}");
}

#[test]
fn a_scenario_attaches_to_its_clause_in_the_lock() {
    let s = Scratch::copy();
    after_list(
        &s,
        "fold",
        "#### Scenarios\n\n- `store.fold.triggers`: WHEN a table reaches 50 committed runs since its last pass, THEN a fold pass starts for that table.",
    );
    assert!(codes(&all_codes(&s), "SpecScenario").is_empty());
    assert!(s.cmd(&["extract"]).status.success());
    let lock: serde_json::Value = serde_json::from_str(&s.read("spec/spec.lock.json")).unwrap();
    let clause = lock["clauses"].as_array().unwrap().iter().find(|c| c["id"] == "store.fold.triggers").unwrap();
    let scen = clause["scenarios"].as_array().expect("scenarios on the clause");
    assert_eq!(scen.len(), 1);
    assert!(scen[0].as_str().unwrap().starts_with("WHEN a table reaches 50"));
}

#[test]
fn a_scenario_may_point_at_a_fixture_table() {
    let s = Scratch::copy();
    after_list(&s, "fold", "#### Scenarios\n\n- `store.fold.triggers`: `tests/fixtures/store.fold/triggers.toml`");
    assert!(codes(&all_codes(&s), "SpecScenario").is_empty());
}

#[test]
fn a_scenario_naming_another_operations_clause_is_a_finding() {
    let s = Scratch::copy();
    after_list(&s, "fold", "#### Scenarios\n\n- `store.lay-out.root`: WHEN anything happens, THEN something follows.");
    assert_eq!(codes(&all_codes(&s), "SpecScenario").len(), 1);
}

#[test]
fn a_scenario_without_when_then_or_a_fixture_is_a_finding() {
    let s = Scratch::copy();
    after_list(&s, "fold", "#### Scenarios\n\n- `store.fold.triggers`: a pass fires eventually.");
    assert_eq!(codes(&all_codes(&s), "SpecScenario").len(), 1);
}
