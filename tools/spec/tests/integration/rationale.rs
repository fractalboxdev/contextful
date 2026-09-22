//! `corpus.rationale`: principle records and one ADR per contract.

use crate::{codes, Scratch};

const OPTIONS: &str = "| Option | Lost on | Cost |\n| --- | --- | --- |\n| One *(chosen)* | — | A cost. |\n| Two | Speed | Another cost. |\n";

fn append(s: &Scratch, rel: &str, text: &str) {
    let body = s.read(rel);
    s.write(rel, &format!("{body}\n{text}"));
}

#[test]
fn the_live_adrs_pass_their_anatomy() {
    let s = Scratch::copy();
    let found = codes(&s.lint("rationale"), "SpecRecord");
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn a_contract_adr_section_over_its_word_limit_is_a_record_finding() {
    let s = Scratch::copy();
    let prose = vec!["word"; 260].join(" ");
    append(&s, "spec/adr/A-store.md", &format!("## A long decision\n\n{prose}\n\n{OPTIONS}"));
    let found = codes(&s.lint("rationale"), "SpecRecord");
    assert!(found.iter().any(|m| m.contains("A long decision") && m.contains("250")), "{found:?}");
}

#[test]
fn a_contract_adr_section_without_an_options_table_is_a_record_finding() {
    let s = Scratch::copy();
    append(&s, "spec/adr/A-store.md", "## A bare decision\n\nThe store decides.\n");
    let found = codes(&s.lint("rationale"), "SpecRecord");
    assert!(found.iter().any(|m| m.contains("A bare decision") && m.contains("Options")), "{found:?}");
}

#[test]
fn a_contract_adr_naming_no_contract_is_a_record_finding() {
    let s = Scratch::copy();
    s.write("spec/adr/A-nowhere.md", &format!("# A-nowhere — Nothing decisions\n\n**Status:** accepted\n\n## A decision\n\nText.\n\n{OPTIONS}"));
    let found = codes(&s.lint("rationale"), "SpecRecord");
    assert!(found.iter().any(|m| m.contains("A-nowhere") && m.contains("contract")), "{found:?}");
}

#[test]
fn a_why_naming_a_numbered_decision_record_is_a_record_finding() {
    let s = Scratch::copy();
    let store = s.read("spec/10-store.md");
    assert!(store.contains("\n  *A-store*\n"), "a clause citing A-store");
    s.write("spec/10-store.md", &store.replacen("\n  *A-store*\n", "\n  *D07*\n", 1));
    let found = codes(&s.lint("rationale"), "SpecRecord");
    assert!(found.iter().any(|m| m.contains("D07")), "{found:?}");
}
