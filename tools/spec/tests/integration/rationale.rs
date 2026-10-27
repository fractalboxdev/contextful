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

// spec: corpus.rationale.contract-adr@fdc8c562
#[test]
fn a_contract_adr_section_over_its_word_limit_is_a_record_finding() {
    let s = Scratch::copy();
    let prose = vec!["word"; 260].join(" ");
    append(&s, "spec/adr/A-store.md", &format!("## A long decision\n\n{prose}\n\n{OPTIONS}"));
    let found = codes(&s.lint("rationale"), "SpecRecord");
    assert!(found.iter().any(|m| m.contains("A long decision") && m.contains("250")), "{found:?}");
    assert!(found.iter().any(|m| m.contains("section heading `A long decision` is not `<slug> — <decision>`")), "{found:?}");

    let s = Scratch::copy();
    let adr = s.read("spec/adr/A-store.md");
    let slug = adr.lines().find_map(|l| l.strip_prefix("## ")).and_then(|h| h.split_once(" — ")).map(|(slug, _)| slug.to_string()).expect("a slugged section");
    append(&s, "spec/adr/A-store.md", &format!("## {slug} — A second decision under one slug\n\nText.\n\n{OPTIONS}"));
    let found = codes(&s.lint("rationale"), "SpecRecord");
    assert!(found.iter().any(|m| m.contains(&format!("slug `{slug}` also heads the section at line"))), "{found:?}");
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
    let why = store.lines().find(|l| l.starts_with("  *A-store#")).expect("a clause citing an A-store section").to_string();
    s.write("spec/10-store.md", &store.replacen(&format!("\n{why}\n"), "\n  *D07*\n", 1));
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

// spec: corpus.rationale.why-cell@378a7f6f
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

    // in an ADR every citation of which names a section, a section one clause of its own
    // contract rests on, naming no revisit, holds no decision
    let s = Scratch::copy();
    append(&s, "spec/adr/A-topology.md", &format!("## lonely — A decision one clause rests on\n\nText.\n\n{OPTIONS}"));
    let topology = s.read(TOPOLOGY);
    assert!(topology.matches("\n  *A-topology#build-profiles*\n").count() >= 3, "build-profiles holds three or more clauses");
    s.write(TOPOLOGY, &topology.replacen("\n  *A-topology#build-profiles*\n", "\n  *A-topology#lonely*\n", 1));
    let found = codes(&s.lint("rationale"), "SpecRecord");
    assert!(found.iter().any(|m| m.contains("section `A-topology#lonely` is cited by 1 clause of its own contract and names no `Revisit:`")), "{found:?}");
    append(&s, "spec/adr/A-topology.md", "Revisit: a second clause rests on it.\n");
    let found = codes(&s.lint("rationale"), "SpecRecord");
    assert_eq!(found.iter().filter(|m| m.contains("A-topology#lonely")).count(), 0, "{found:?}");
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

// spec: corpus.rationale.orphan-record@f99b4ce4
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

    let s = Scratch::copy();
    let store = s.read(STORE);
    s.write(STORE, &store.replacen("\n  *A-topology#commit-lock*\n", "\n  *A-topology#no-such-section*\n", 1));
    let found = codes(&s.lint("rationale"), "SpecRecord");
    assert!(found.iter().any(|m| m.contains("cites missing section A-topology#no-such-section")), "{found:?}");
}

const TOPOLOGY: &str = "spec/01-topology.md";

// spec: corpus.rationale.why-scoped@9a6d1fa1
#[test]
fn a_bare_adr_citation_past_the_ceiling_is_unscoped() {
    let s = Scratch::copy();
    assert!(codes(&s.lint("rationale"), "SpecWhyUnscoped").is_empty(), "{:?}", s.lint("rationale"));
    let topology = s.read(TOPOLOGY);
    s.write(TOPOLOGY, &topology.replacen("\n  *A-topology#build-profiles*\n", "\n  *A-topology*\n", 1));
    let found = codes(&s.lint("rationale"), "SpecWhyUnscoped");
    assert!(found.iter().any(|m| m.contains("cites A-topology without a section")), "{found:?}");
}

/// The Why line under the store item with subject `subject`.
fn why_of(text: &str, subject: &str) -> String {
    let at = text.find(&format!("\n- `{subject}` — ")).unwrap_or_else(|| panic!("no `{subject}` item"));
    let why = at + 1 + text[at + 1..].find('\n').unwrap() + 1;
    text[why..why + text[why..].find('\n').unwrap()].to_string()
}

/// Make the store item with subject `subject` cite `A-store` without a section.
fn cite_bare(s: &Scratch, subject: &str) {
    let store = s.read(STORE);
    let item = store.lines().find(|l| l.starts_with(&format!("- `{subject}` — "))).unwrap_or_else(|| panic!("no `{subject}` item")).to_string();
    let next = store[store.find(&item).unwrap() + item.len() + 1..].lines().next().unwrap().to_string();
    let cited = if next.starts_with("  *") {
        store.replacen(&format!("{item}\n{next}\n"), &format!("{item}\n  *A-store*\n"), 1)
    } else {
        store.replacen(&format!("{item}\n"), &format!("{item}\n  *A-store*\n"), 1)
    };
    s.write(STORE, &cited);
}

// spec: corpus.rationale.why-migrate@f83ec897
#[test]
fn why_migrate_rewrites_a_citation_one_section_names_and_prints_the_rest() {
    let s = Scratch::copy();
    cite_bare(&s, "non-blocking");
    cite_bare(&s, "supersedes");
    append(&s, "spec/adr/A-store.md", &format!("## non-blocking-pass — `store.fold.non-blocking` keeps reads off the pass\n\nText.\n\n{OPTIONS}"));
    // Two sections naming one clause leave its citation ambiguous, whatever the live record says.
    append(&s, "spec/adr/A-store.md", &format!("## supersedes-one — `store.fold.supersedes` first reading\n\nText.\n\n{OPTIONS}"));
    append(&s, "spec/adr/A-store.md", &format!("## supersedes-two — `store.fold.supersedes` second reading\n\nText.\n\n{OPTIONS}"));
    let out = s.cmd(&["why-migrate", "A-store"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(why_of(&s.read(STORE), "non-blocking"), "  *A-store#non-blocking-pass*");
    let printed = String::from_utf8_lossy(&out.stdout);
    assert!(printed.starts_with("rewrote "), "{printed}");
    assert!(printed.lines().any(|l| l == "store: 1 unresolved"), "{printed}");
    assert_eq!(why_of(&s.read(STORE), "supersedes"), "  *A-store*");
    assert!(!s.cmd(&["why-migrate", "A-nowhere"]).status.success());
}

#[test]
fn why_migrate_rewrites_every_named_adr_a_why_cites_on_one_line() {
    let s = Scratch::copy();
    cite_bare(&s, "non-blocking");
    s.write(STORE, &s.read(STORE).replacen("\n  *A-store*\n", "\n  *A-store, A-run*\n", 1));
    append(&s, "spec/adr/A-store.md", &format!("## non-blocking-pass — `store.fold.non-blocking` keeps reads off the pass\n\nText.\n\n{OPTIONS}"));
    append(&s, "spec/adr/A-run.md", &format!("## non-blocking-run — `store.fold.non-blocking` keeps runs off the pass\n\nText.\n\n{OPTIONS}"));
    let out = s.cmd(&["why-migrate", "A-store", "A-run"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(why_of(&s.read(STORE), "non-blocking"), "  *A-store#non-blocking-pass, A-run#non-blocking-run*");
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("rewrote 2 citations"), "{}", String::from_utf8_lossy(&out.stdout));
}

// spec: corpus.rationale.cited-by@9aea4abe
#[test]
fn each_adr_section_lists_the_clauses_citing_it_in_the_card_and_the_lock() {
    let s = Scratch::copy();
    assert!(s.cmd(&["state"]).status.success());
    assert!(s.cmd(&["extract"]).status.success());
    let card = s.read("spec/cards/topology.md");
    let row = card.lines().find(|l| l.starts_with("| `A-topology#commit-lock` |")).unwrap_or_else(|| panic!("{card}"));
    assert!(row.contains("`store.reserve.commit-lock-span`"), "{row}");
    let lock: serde_json::Value = serde_json::from_str(&s.read("spec/spec.lock.json")).unwrap();
    let cited: Vec<&str> = lock["sections"]["A-topology#commit-lock"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert!(cited.contains(&"store.reserve.commit-lock-span"), "{cited:?}");
}

#[test]
fn a_slice_inlines_the_cited_section_alone() {
    let s = Scratch::copy();
    let out = s.cmd(&["slice", "store.reserve", "--json"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let record = v["records"].as_array().unwrap().iter().find(|r| r["id"] == "A-topology#commit-lock").expect("the cited section");
    let text = record["text"].as_str().unwrap();
    assert!(text.starts_with("## commit-lock — "), "{text}");
    assert_eq!(text.matches("\n## ").count(), 0, "one section: {text}");
}
