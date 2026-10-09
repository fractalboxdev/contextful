//! `corpus.address`: the three segments of a clause id, where each comes from, and the
//! findings a malformed or duplicated id raises.

use crate::{codes, Scratch};

const STORE: &str = "spec/10-store.md";
const ITEM: &str = "- `includes-runs` — ";

fn lock(s: &Scratch) -> serde_json::Value {
    assert!(s.cmd(&["extract"]).status.success());
    serde_json::from_str(&s.read("spec/spec.lock.json")).unwrap()
}

fn ids(s: &Scratch) -> Vec<String> {
    let mut ids: Vec<String> = lock(s)["clauses"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap().to_string()).collect();
    ids.sort();
    ids
}

/// Rename the subject of the `includes-runs` item in the store file.
fn rename_subject(s: &Scratch, subject: &str) {
    let body = s.read(STORE);
    assert!(body.contains(ITEM), "the store file holds `includes-runs`");
    s.write(STORE, &body.replacen(ITEM, &format!("- `{subject}` — "), 1));
}

// spec: corpus.address.contract-segment@a2354d86
#[test]
fn the_first_segment_is_the_front_matter_contract_its_registry_entry_lists() {
    let s = Scratch::copy();
    let clause = lock(&s)["clauses"].as_array().unwrap().iter().find(|c| c["id"] == "store.fold.includes-runs").cloned().expect("the clause");
    assert_eq!(clause["contract"], "store");
    assert_eq!(clause["file"], STORE);
    assert!(codes(&s.lint("anatomy"), "SpecAnatomy").is_empty());

    let body = s.read(STORE);
    s.write(STORE, &body.replacen("contract: store\n", "contract: run\n", 1));
    let found = codes(&s.lint("anatomy"), "SpecAnatomy");
    assert!(found.iter().any(|m| m.contains("front matter says `run`") && m.contains("`store`")), "{found:?}");
}

// spec: corpus.address.operation-segment@bc5cd98b
#[test]
fn the_second_segment_is_an_operation_registered_in_the_fragment_and_owned_once() {
    let s = Scratch::copy();
    let clause = lock(&s)["clauses"].as_array().unwrap().iter().find(|c| c["id"] == "store.fold.includes-runs").cloned().expect("the clause");
    assert_eq!(clause["operation"], "fold");
    assert!(codes(&s.lint("registry"), "SpecRegistry").is_empty());

    let fragment = s.read("spec/terms/store.toml");
    assert!(fragment.contains("fold       = {}\n"));
    s.write("spec/terms/store.toml", &fragment.replacen("fold       = {}\n", "", 1));
    let found = codes(&s.lint("registry"), "SpecRegistry");
    assert!(found.iter().any(|m| m.contains("`store.fold` is not registered")), "{found:?}");
}

// spec: corpus.address.subject-segment@11f18596
#[test]
fn a_subject_over_forty_chars_is_malformed_and_forty_is_admitted() {
    let s = Scratch::copy();
    rename_subject(&s, &"a".repeat(40));
    assert!(codes(&s.lint("address"), "SpecMalformedId").is_empty());

    let s = Scratch::copy();
    rename_subject(&s, &"a".repeat(41));
    let found = codes(&s.lint("address"), "SpecMalformedId");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("subject exceeds 40 chars"), "{found:?}");
}

// spec: corpus.address.ids-are-stable@3d26e3f4
#[test]
fn moving_an_operation_between_files_of_one_contract_rewrites_no_id() {
    let s = Scratch::copy();
    let before = ids(&s);
    assert!(before.contains(&"run.test-engine.unsupported-driver".to_string()));

    let derive = s.read("spec/32-derive.md");
    let start = derive.find("## test-engine\n").expect("the test-engine section");
    let end = start + derive[start..].find("## Shapes\n").expect("the Shapes section");
    let section = derive[start..end].to_string();
    s.write("spec/32-derive.md", &format!("{}{}", &derive[..start], &derive[end..]).replacen("  - test-engine\n", "", 1));

    let pipeline = s.read("spec/31-pipeline.md");
    let shapes = pipeline.find("## Shapes\n").expect("the Shapes section");
    let pipeline = format!("{}{section}{}", &pipeline[..shapes], &pipeline[shapes..]);
    s.write("spec/31-pipeline.md", &pipeline.replacen("  - publish\n", "  - publish\n  - test-engine\n", 1));

    assert_eq!(ids(&s), before);
    assert!(s.lint("address").is_empty(), "{:?}", s.lint("address"));
    assert!(codes(&s.lint("anatomy"), "SpecAnatomy").is_empty());
}

// spec: corpus.address.malformed-id@8a2a7849
#[test]
fn a_clause_under_a_section_its_file_does_not_own_is_a_malformed_id() {
    let s = Scratch::copy();
    assert!(codes(&s.lint("address"), "SpecMalformedId").is_empty());
    let body = s.read(STORE);
    let shapes = body.find("## Shapes\n").unwrap_or(body.len());
    let stray = "## stray\n\nA section no front matter owns.\n\n- `orphan` — A clause item in a section the file does not own.\n\n";
    s.write(STORE, &format!("{}{stray}{}", &body[..shapes], &body[shapes..]));
    let found = codes(&s.lint("address"), "SpecMalformedId");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("`store.stray.orphan`") && found[0].contains("does not own"), "{found:?}");
}

// spec: corpus.address.duplicate-id@1790d7da
#[test]
fn a_clause_id_in_two_items_is_a_duplicate_naming_both_locations() {
    let s = Scratch::copy();
    assert!(codes(&s.lint("address"), "SpecDuplicateId").is_empty());
    let body = s.read(STORE);
    let row = body.lines().find(|l| l.starts_with(ITEM)).unwrap().to_string();
    let line = body.lines().position(|l| l == row).unwrap() + 1;
    s.write(STORE, &body.replacen(&row, &format!("{row}\n{row}"), 1));
    let found = codes(&s.lint("address"), "SpecDuplicateId");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("`store.fold.includes-runs`") && found[0].contains(&format!("{STORE}:{line}")), "{found:?}");
}
