//! Lean 4 artifacts as pins: theorems under `formal/`, `-- spec:` tags, `sorry` as
//! unfinished, one theorem beside one test, and `scaffold --lean`.

use crate::{codes, Scratch};

const LEAN: &str = "formal/Contextful/Corpus.lean";
const CLAUSE: &str = "corpus.anatomy.statement-words";

fn verdict(s: &Scratch, clause: &str) -> String {
    s.verdict(clause).unwrap_or_else(|| panic!("no pin entry for {clause} in the lock file"))
}

fn scaffold_lean(s: &Scratch) {
    let out = s.cmd(&["scaffold", "corpus.anatomy", "--lean", LEAN]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

/// Replace every `sorry` with a closed proof and a closed proposition.
fn prove(s: &Scratch) {
    let text = s.read(LEAN).replace("(sorry : Prop)", "True").replace("sorry", "trivial");
    s.write(LEAN, &text);
}

#[test]
fn a_theorem_pin_resolves_under_formal() {
    let s = Scratch::copy();
    s.write(LEAN, "namespace Contextful.Corpus\n\ntheorem statement_words : True := trivial\n\nend Contextful.Corpus\n");
    s.pin(CLAUSE, "theorem", "Contextful.Corpus.statement_words");
    assert!(codes(&s.lint("state"), "SpecBrokenPin").is_empty());
    assert_eq!(verdict(&s, CLAUSE), "performed");
}

// spec: corpus.state.scaffold-lean@8064bb28
#[test]
fn scaffold_lean_writes_one_tagged_sorry_theorem_per_clause() {
    let s = Scratch::copy();
    scaffold_lean(&s);
    let text = s.read(LEAN);
    let clauses = s.clauses_of("corpus.anatomy");
    for id in &clauses {
        assert!(text.contains(&format!("-- spec: {id}@")), "no tag for {id}:\n{text}");
    }
    assert_eq!(text.matches("theorem ").count(), clauses.len(), "{text}");
    assert!(text.contains("/-- A clause statement holds at most 40 words"), "{text}");
    scaffold_lean(&s);
    assert_eq!(s.read(LEAN), text, "a second run changes nothing");
}

// spec: corpus.state.lean-tag@f0b03491
#[test]
fn a_tagged_theorem_holding_sorry_is_broken_until_proved() {
    let s = Scratch::copy();
    scaffold_lean(&s);
    let broken = codes(&s.lint("state"), "SpecBrokenPin");
    assert!(broken.iter().any(|m| m.contains(CLAUSE) && m.contains("sorry")), "{broken:?}");
    assert_eq!(verdict(&s, CLAUSE), "broken");
    prove(&s);
    assert!(codes(&s.lint("state"), "SpecBrokenPin").is_empty());
    assert_eq!(verdict(&s, CLAUSE), "performed");
}

#[test]
fn a_stale_lean_tag_raises_spec_stale_pin() {
    let s = Scratch::copy();
    scaffold_lean(&s);
    prove(&s);
    let text = s.read(LEAN);
    let tag = text.lines().find(|l| l.starts_with(&format!("-- spec: {CLAUSE}@"))).unwrap().to_string();
    s.write(LEAN, &text.replacen(&tag, &format!("-- spec: {CLAUSE}@deadbeef"), 1));
    let stale = codes(&s.lint("state"), "SpecStalePin");
    assert_eq!(stale.len(), 1, "{stale:?}");
}

// spec: corpus.state.theorem-beside-test@7f5680a1
#[test]
fn a_clause_carries_one_theorem_beside_one_test() {
    let s = Scratch::copy();
    scaffold_lean(&s);
    prove(&s);
    s.scaffold("corpus.anatomy", "crates/x");
    let module = "crates/x/tests/integration/anatomy.rs";
    s.implement(module);
    assert!(codes(&s.lint("state"), "SpecBrokenPin").is_empty());
    assert_eq!(verdict(&s, CLAUSE), "performed");

    let text = s.read(LEAN);
    let tag = text.lines().find(|l| l.starts_with(&format!("-- spec: {CLAUSE}@"))).unwrap().to_string();
    s.write(LEAN, &format!("{text}\n{tag}\ntheorem statement_words_again : True := trivial\n"));
    let broken = codes(&s.lint("state"), "SpecBrokenPin");
    assert!(broken.iter().any(|m| m.contains(CLAUSE) && m.contains("two")), "{broken:?}");
}

/// A proved Lean theorem and an implemented Rust test, both tagged for every
/// `corpus.anatomy` clause.
fn theorem_beside_test(s: &Scratch) {
    scaffold_lean(s);
    prove(s);
    s.scaffold("corpus.anatomy", "crates/x");
    s.implement("crates/x/tests/integration/anatomy.rs");
}

const BOUND: &str = "[constant.\"Corpus.statement_words\"]\nmodule = \"Contextful.Corpus\"\nbinding = \"crates/x/src/lib.rs::words; test crates/x/tests/integration/anatomy.rs::statement_words\"\n";

// spec: corpus.state.bound-theorem@72da97a9
// spec: corpus.state.proved@fb95daa7
#[test]
fn a_theorem_beside_a_test_counts_only_when_its_inventory_row_binds_that_test() {
    let s = Scratch::copy();
    theorem_beside_test(&s);
    assert!(codes(&s.lint("state"), "SpecBrokenPin").is_empty(), "{:?}", s.lint("state"));
    let row = s.pin_row(CLAUSE);
    assert_eq!((row["verdict"].as_str(), row["proved"].as_bool()), (Some("performed"), Some(false)), "{row}");
    s.write("formal/inventory.toml", &BOUND.replace("::statement_words\"", "::file_length\""));
    assert_eq!(s.pin_row(CLAUSE)["proved"], false, "a binding naming another test leaves the theorem uncounted");
    s.write("formal/inventory.toml", BOUND);
    let row = s.pin_row(CLAUSE);
    assert_eq!((row["verdict"].as_str(), row["proved"].as_bool()), (Some("performed"), Some(true)), "{row}");
    let status = s.read("spec/status.md");
    assert!(status.contains("| Performed | Proved | Broken |"), "{status}");
    assert!(status.lines().any(|l| l.starts_with("| `corpus` |") && l.split(" | ").nth(8) == Some("1")), "{status}");
    let card = s.read("spec/cards/corpus.md");
    assert!(card.contains("| Operation | Covers | Clauses | Proved |"), "{card}");
    assert!(card.lines().any(|l| l.starts_with("| `anatomy` |") && l.ends_with("| 1 |")), "{card}");
}

// spec: corpus.state.proof-obligation@b41611fc
#[test]
fn an_obligation_listing_a_clause_that_is_not_proved_is_refused() {
    let s = Scratch::copy();
    theorem_beside_test(&s);
    s.write("formal/obligations.toml", &format!("clauses = [\"{CLAUSE}\"]\n"));
    let found = codes(&s.lint("state"), "SpecProofObligationUnpinned");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains(CLAUSE), "{found:?}");
    s.write("formal/inventory.toml", BOUND);
    assert!(codes(&s.lint("state"), "SpecProofObligationUnpinned").is_empty());
    s.write("formal/obligations.toml", "clauses = [\"corpus.anatomy.no-such-clause\"]\n");
    assert_eq!(codes(&s.lint("state"), "SpecProofObligationUnpinned").len(), 1);
}
