//! Lean 4 artifacts as pins: theorems under `formal/`, `-- spec:` tags, `sorry` as
//! unfinished, one theorem beside one test, and `scaffold --lean`.

use crate::{codes, Scratch};

const LEAN: &str = "formal/Contextful/Corpus.lean";
const CLAUSE: &str = "corpus.anatomy.statement-words";

fn verdict(s: &Scratch, clause: &str) -> String {
    assert!(s.cmd(&["state"]).status.success());
    let status = s.read("spec/status.md");
    let row = status
        .lines()
        .find(|l| l.starts_with(&format!("| `{clause}` |")))
        .unwrap_or_else(|| panic!("no pin row for {clause} in\n{status}"));
    row.trim_end_matches(" |").rsplit("| ").next().unwrap().to_string()
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

#[test]
fn a_clause_carries_one_theorem_beside_one_test() {
    let s = Scratch::copy();
    scaffold_lean(&s);
    prove(&s);
    s.scaffold("corpus.anatomy", "crates/x");
    let module = "crates/x/tests/integration/anatomy.rs";
    let rust: String = s.read(module).lines().map(|l| if l.trim_start().starts_with("todo!") { "    assert!(true);" } else { l }).collect::<Vec<_>>().join("\n");
    s.write(module, &(rust + "\n"));
    assert!(codes(&s.lint("state"), "SpecBrokenPin").is_empty());
    assert_eq!(verdict(&s, CLAUSE), "performed");

    let text = s.read(LEAN);
    let tag = text.lines().find(|l| l.starts_with(&format!("-- spec: {CLAUSE}@"))).unwrap().to_string();
    s.write(LEAN, &format!("{text}\n{tag}\ntheorem statement_words_again : True := trivial\n"));
    let broken = codes(&s.lint("state"), "SpecBrokenPin");
    assert!(broken.iter().any(|m| m.contains(CLAUSE) && m.contains("two")), "{broken:?}");
}
