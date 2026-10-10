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

const STORE: &str = "spec/10-store.md";
const P9: &str = "spec/adr/P9-a-test-principle.md";

fn p9(decision: &str, options: &str) -> String {
    format!("# P9 — A test principle\n\n**Status:** accepted\n\n## Decision\n\n{decision}\n\n## Options\n\n{options}\n## Consequences\n\n- One consequence.\n")
}

/// A principle `P9` with the given Decision and Options, cited by one store clause.
fn principle(s: &Scratch, decision: &str, options: &str) {
    s.write(P9, &p9(decision, options));
    let store = s.read(STORE);
    if !store.contains("P9*") {
        assert!(store.contains("\n  *P4*\n"), "a clause citing P4");
        s.write(STORE, &store.replacen("\n  *P4*\n", "\n  *P4, P9*\n", 1));
    }
}

/// Replace the first `because` Why of the store file with `because` and `words` words.
fn because_of(s: &Scratch, store: &str, words: usize) {
    let why = store.lines().find(|l| l.starts_with("  *because ")).expect("a because Why").to_string();
    s.write(STORE, &store.replacen(&why, &format!("  *because {}*", vec!["word"; words].join(" ")), 1));
}

// spec: corpus.rationale.why-cell@f56dd62e
#[test]
fn a_because_over_thirty_words_or_a_refusal_without_a_why_is_a_record_finding() {
    let s = Scratch::copy();
    let store = s.read(STORE);
    because_of(&s, &store, 30);
    assert!(codes(&s.lint("rationale"), "SpecRecord").is_empty());
    because_of(&s, &store, 31);
    let found = codes(&s.lint("rationale"), "SpecRecord");
    assert!(found.iter().any(|m| m.contains("`because` cell is 31 words, over 30")), "{found:?}");

    let at = store.find("- `partial-snapshot` — ").expect("the refusal");
    let why = at + store[at..].find("\n  *").unwrap();
    let end = why + 1 + store[why + 1..].find('\n').unwrap();
    s.write(STORE, &format!("{}{}", &store[..why], &store[end..]));
    let found = codes(&s.lint("rationale"), "SpecRecord");
    assert!(found.iter().any(|m| m.contains("refusal `store.fold.partial-snapshot` carries no Why")), "{found:?}");
}

// spec: corpus.rationale.record-threshold@ce755e7e
#[test]
fn a_decision_one_clause_rests_on_lives_in_its_because_cell() {
    let s = Scratch::copy();
    assert!(codes(&s.lint("rationale"), "SpecRecord").is_empty());
    let store = s.read(STORE);
    assert!(store.matches("\n  *P4*\n").count() >= 2, "P4 holds two or more clauses");
    s.write(STORE, &store.replacen("\n  *P4*\n", "\n  *because a reader observes the snapshot and its sidecars together or neither*\n", 1));
    assert!(codes(&s.lint("rationale"), "SpecRecord").is_empty(), "{:?}", s.lint("rationale"));

    // a record no clause rests on holds no decision
    s.write(P9, &p9("The rule.", OPTIONS));
    let found = codes(&s.lint("rationale"), "SpecRecord");
    assert!(found.iter().any(|m| m.contains("record P9 is cited by no clause")), "{found:?}");
}

// spec: corpus.rationale.record-anatomy@4dba1869
#[test]
fn a_principle_over_four_hundred_words_or_off_its_sections_is_a_record_finding() {
    let s = Scratch::copy();
    principle(&s, "The rule.", OPTIONS);
    assert!(codes(&s.lint("rationale"), "SpecRecord").is_empty(), "{:?}", s.lint("rationale"));

    principle(&s, &vec!["word"; 400].join(" "), OPTIONS);
    let found = codes(&s.lint("rationale"), "SpecRecord");
    assert!(found.iter().any(|m| m.contains("over 400")), "{found:?}");

    s.write(P9, &p9("The rule.", OPTIONS).replace("## Decision\n", "## Reasoning\n"));
    let found = codes(&s.lint("rationale"), "SpecRecord");
    assert!(found.iter().any(|m| m.contains("are not Context?, Decision, Options, Consequences, Revisit?")), "{found:?}");
}

// spec: corpus.rationale.options-table@a542ca1c
#[test]
fn an_options_table_off_its_header_rows_or_chosen_mark_is_a_record_finding() {
    let s = Scratch::copy();
    principle(&s, "The rule.", OPTIONS);
    assert!(codes(&s.lint("rationale"), "SpecRecord").is_empty());

    principle(&s, "The rule.", "| Choice | Lost on | Cost |\n| --- | --- | --- |\n| One *(chosen)* | — | A cost. |\n| Two | Speed | Another cost. |\n");
    let found = codes(&s.lint("rationale"), "SpecRecord");
    assert!(found.iter().any(|m| m.contains("not headed Option | Lost on | Cost")), "{found:?}");

    principle(&s, "The rule.", "| Option | Lost on | Cost |\n| --- | --- | --- |\n| One *(chosen)* | Speed | A cost. |\n");
    let found = codes(&s.lint("rationale"), "SpecRecord");
    assert!(found.iter().any(|m| m.contains("Options holds 1 rows, want 2 to 5")), "{found:?}");
    assert!(found.iter().any(|m| m.contains("the chosen row's Lost on is not `—`")), "{found:?}");

    principle(&s, "The rule.", "| Option | Lost on | Cost |\n| --- | --- | --- |\n| One | — | A cost. |\n| Two |  | Another cost. |\n");
    let found = codes(&s.lint("rationale"), "SpecRecord");
    assert!(found.iter().any(|m| m.contains("0 rows marked *(chosen)*, want 1")), "{found:?}");
    assert!(found.iter().any(|m| m.contains("a rejected option names no criterion it lost on")), "{found:?}");
}

// spec: corpus.rationale.unsettled-line@b095255a
#[test]
fn an_unsettled_line_off_its_form_or_an_appendix_heading_is_an_unsettled_finding() {
    let s = Scratch::copy();
    assert!(codes(&s.lint("rationale"), "SpecUnsettled").is_empty());
    let store = s.read(STORE);
    let line = store.lines().find(|l| l.starts_with("unsettled: ")).expect("an unsettled line").to_string();
    s.write(STORE, &store.replacen(&line, "unsettled: the reader decides later. owner: store affects: store.lay-out", 1));
    let found = codes(&s.lint("rationale"), "SpecUnsettled");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("is not `unsettled:"), "{found:?}");

    s.write(STORE, &store.replacen(&line, &line.replace("affects: store.lay-out", "affects: store.no-such-operation"), 1));
    let found = codes(&s.lint("rationale"), "SpecUnsettled");
    assert!(found.iter().any(|m| m.contains("`store.no-such-operation` names no operation")), "{found:?}");

    s.write(STORE, &format!("{store}\n### See also\n"));
    let found = codes(&s.lint("rationale"), "SpecUnsettled");
    assert!(found.iter().any(|m| m.contains("appendix heading")), "{found:?}");
}

// spec: corpus.rationale.orphan-record@9a132a4a
#[test]
fn a_record_no_why_cites_or_a_why_naming_no_record_is_a_record_finding() {
    let s = Scratch::copy();
    s.write(P9, &p9("The rule.", OPTIONS));
    let found = codes(&s.lint("rationale"), "SpecRecord");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("record P9 is cited by no clause"), "{found:?}");

    let s = Scratch::copy();
    let store = s.read(STORE);
    s.write(STORE, &store.replacen("\n  *P4*\n", "\n  *P4, P98*\n", 1));
    let found = codes(&s.lint("rationale"), "SpecRecord");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("Why cites missing record P98"), "{found:?}");
}
