//! `corpus.registry.term`: each fragment's `[term]` table, the homonym it refuses, the
//! glossary and Vale rule rendered from it, and the first-use ratchet over guides.

use crate::{codes, Scratch};

const STORE_TERMS: &str = "spec/terms/store.toml";
const GUIDE: &str = "spec/guide/store.md";

/// Insert `line` as the first entry of `rel`'s `[term]` table.
fn add_term(s: &Scratch, rel: &str, line: &str) {
    let text = s.read(rel);
    assert!(text.contains("\n[term]\n"), "{rel} holds no term table");
    s.write(rel, &text.replacen("\n[term]\n", &format!("\n[term]\n{line}\n"), 1));
}

/// Replace the term line starting `prefix` in `rel` by `f(line)`.
fn edit_term(s: &Scratch, rel: &str, prefix: &str, f: impl Fn(&str) -> String) {
    let text = s.read(rel);
    let line = text.lines().find(|l| l.starts_with(prefix)).unwrap_or_else(|| panic!("no `{prefix}` term")).to_string();
    s.write(rel, &text.replacen(&line, &f(&line), 1));
}

fn state(s: &Scratch) {
    let out = s.cmd(&["state"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

// spec: corpus.registry.term@e2168a51
#[test]
fn a_term_names_its_defining_clause_and_a_gloss_of_at_most_25_words() {
    let s = Scratch::copy();
    assert!(codes(&s.lint("registry"), "SpecTerm").is_empty());
    assert!(s.cmd(&["extract"]).status.success());
    let lock: serde_json::Value = serde_json::from_str(&s.read("spec/spec.lock.json")).unwrap();
    assert_eq!(lock["registry"]["fragments"]["store"]["term"]["writer lease"]["clause"], "store.lease.ttl");

    let long = vec!["word"; 26].join(" ");
    edit_term(&s, STORE_TERMS, "\"commit lock\"", |_| format!("\"commit lock\" = {{ clause = \"store.reserve.commit-lock-span\", gloss = \"{long}\" }}"));
    let found = codes(&s.lint("registry"), "SpecTerm");
    assert!(found.iter().any(|m| m.contains("term `commit lock` gloss holds 26 words, over 25")), "{found:?}");
}

// spec: corpus.registry.bad-term@e34e04d2
#[test]
fn a_homonym_a_foreign_clause_or_an_avoided_term_raises_spec_term() {
    let s = Scratch::copy();
    add_term(&s, "spec/terms/connector.toml", "\"writer lease\" = { clause = \"connector.lease.lease\", gloss = \"A second meaning.\" }");
    let found = codes(&s.lint("registry"), "SpecTerm");
    assert!(found.iter().any(|m| m.contains("term `writer lease` is registered in `connector` and `store`")), "{found:?}");

    let s = Scratch::copy();
    add_term(&s, STORE_TERMS, "\"ghost\" = { clause = \"store.lease.no-such-clause\", gloss = \"Nothing.\" }");
    add_term(&s, STORE_TERMS, "\"borrowed\" = { clause = \"connector.lease.lease\", gloss = \"Another contract's clause.\" }");
    add_term(&s, STORE_TERMS, "\"bare\" = { clause = \"store.lease.ttl\", gloss = \"\" }");
    add_term(&s, STORE_TERMS, "\"echo\" = { clause = \"store.lease.ttl\", gloss = \"Avoids a term.\", avoid = [\"commit lock\"] }");
    let found = codes(&s.lint("registry"), "SpecTerm");
    for want in [
        "term `ghost` names missing clause `store.lease.no-such-clause`",
        "term `borrowed` names clause `connector.lease.lease` of another contract",
        "term `bare` carries no gloss",
        "term `echo` avoids `commit lock`, itself a registered term",
    ] {
        assert!(found.iter().any(|m| m.contains(want)), "{want}: {found:?}");
    }
}

// spec: corpus.registry.glossary@b734d446
#[test]
fn state_renders_every_term_into_the_glossary_and_lint_holds_it_to_regeneration() {
    let s = Scratch::copy();
    state(&s);
    let page = s.read("spec/glossary.md");
    assert!(page.starts_with("# Glossary\n"), "{page}");
    let row = page.lines().find(|l| l.starts_with("| writer lease |")).expect("the writer lease row");
    assert!(row.ends_with("| {{store.lease.ttl}} |"), "{row}");
    let at = |term: &str| page.find(&format!("| {term} |")).unwrap_or_else(|| panic!("no `{term}` row"));
    assert!(at("audit chain") < at("commit fence") && at("commit fence") < at("writer lease"));

    s.write("spec/glossary.md", &page.replace("| writer lease |", "| writer-lease |"));
    let found = codes(&s.lint("render"), "SpecStaleRender");
    assert!(found.iter().any(|m| m.contains("spec/glossary.md")), "{found:?}");
}

// spec: corpus.registry.vale-terms@10ec7dec
#[test]
fn state_writes_each_avoided_spelling_into_the_vale_term_rule() {
    let s = Scratch::copy();
    state(&s);
    let rule = s.read(".vale/styles/WriteTechnically/Terms.yml");
    assert!(rule.contains("extends: substitution\n"), "{rule}");
    assert!(rule.contains("\n  fencing token: commit fence\n"), "{rule}");

    s.write(".vale/styles/WriteTechnically/Terms.yml", &rule.replace("fencing token", "fence token"));
    let found = codes(&s.lint("render"), "SpecStaleRender");
    assert!(found.iter().any(|m| m.contains("Terms.yml")), "{found:?}");
}

// spec: corpus.registry.term-first-use@b5ad7772
#[test]
fn a_guide_first_use_without_the_defining_pointer_raises_spec_term_first_use() {
    let s = Scratch::copy();
    add_term(&s, STORE_TERMS, "\"frobnicated part\" = { clause = \"store.fold.pass\", gloss = \"A test term.\" }");
    let guide = s.read(GUIDE);
    let line = guide.lines().count() + 2;
    s.write(GUIDE, &format!("{guide}\nFrobnicated parts wait in `frobnicated part` staging.\n\nA frobnicated part waits again.\n"));
    let found = codes(&s.lint("registry"), "SpecTermFirstUse");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("over its ceiling"), "{found:?}");
    assert!(found[0].contains(&format!("spec/guide/store.md:{line} `frobnicated part`")), "{found:?}");

    s.write(GUIDE, &format!("{guide}\nFrobnicated parts wait ({{{{store.fold.pass}}}}).\n\nA frobnicated part waits again.\n"));
    assert!(codes(&s.lint("registry"), "SpecTermFirstUse").is_empty());

    s.write(GUIDE, &format!("{guide}\nOnly `frobnicated part` in code.\n"));
    assert!(codes(&s.lint("registry"), "SpecTermFirstUse").is_empty());
}
