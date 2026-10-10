//! `corpus.render`: the generated files, the lock file's content, and the vocabulary no
//! authored file carries.

use crate::{codes, Scratch};

const GUIDE: &str = "spec/guide/store.md";
const STORE: &str = "spec/10-store.md";

/// The `code` findings of the `render` check after appending `line` to `rel`.
fn appended(rel: &str, line: &str, code: &str) -> Vec<String> {
    let s = Scratch::copy();
    let text = s.read(rel);
    s.write(rel, &format!("{text}\n{line}\n"));
    codes(&s.lint("render"), code)
}

// spec: corpus.render.generated@7d80b387
#[test]
fn a_committed_render_differing_from_regeneration_is_stale() {
    let s = Scratch::copy();
    assert!(s.cmd(&["state"]).status.success());
    assert!(s.cmd(&["extract"]).status.success());
    assert!(codes(&s.lint("render"), "SpecStaleRender").is_empty());
    for rel in ["spec/status.md", "spec/spec.lock.json", "spec/targets.md"] {
        let text = s.read(rel);
        s.write(rel, &format!("{text} "));
        let found = codes(&s.lint("render"), "SpecStaleRender");
        assert_eq!(found.len(), 1, "{rel}: {found:?}");
        assert!(found[0].contains(rel), "{found:?}");
        s.write(rel, &text);
    }
}

// spec: corpus.render.lock-file@b3f3193a
#[test]
fn the_lock_carries_clauses_ledes_owns_registry_and_pointers() {
    let s = Scratch::copy();
    assert!(s.cmd(&["extract"]).status.success());
    let lock: serde_json::Value = serde_json::from_str(&s.read("spec/spec.lock.json")).unwrap();
    let clause = lock["clauses"].as_array().unwrap().iter().find(|c| c["id"] == "store.fold.partial-snapshot").expect("the clause");
    for key in ["contract", "operation", "subject", "kind", "statement", "why", "file", "line"] {
        assert!(!clause[key].is_null(), "clause lacks `{key}`: {clause}");
    }
    assert_eq!(clause["kind"], "refusal");
    assert_eq!(clause["why"], "P4");
    assert_eq!(clause["file"], STORE);
    assert!(lock["ledes"]["store.fold"].as_str().is_some_and(|l| l.starts_with("Compaction")));
    assert!(lock["owns"][STORE].as_array().is_some_and(|o| o.iter().any(|op| op == "fold")), "{}", lock["owns"]);
    assert!(lock["registry"]["fragments"]["store"].to_string().contains("StorePartialSnapshot"));
    assert!(!lock["pointers"].as_array().unwrap().is_empty());
}

// spec: corpus.render.dated-prose@fe451fb3
#[test]
fn build_state_words_and_an_iso_date_are_dated_prose() {
    assert!(appended(GUIDE, "A fold reads every committed run.", "SpecDatedProse").is_empty());
    for line in ["A fold is currently single-threaded.", "The pass is implemented in the store.", "A TODO remains here.", "The format changed on 2026-01-01."] {
        assert_eq!(appended(GUIDE, line, "SpecDatedProse").len(), 1, "{line}");
    }
}

// spec: corpus.render.banned-vocabulary@964a7176
#[test]
fn a_banned_noun_a_bare_issue_number_or_a_pull_request_link_is_a_banned_word() {
    assert!(appended(GUIDE, "A fold reads every committed run.", "SpecBannedWord").is_empty());
    for line in ["The store is the seam between runs.", "The catalog is load-bearing.", "See #42 for the history.", "See the change at /pull/42 upstream."] {
        assert_eq!(appended(GUIDE, line, "SpecBannedWord").len(), 1, "{line}");
    }
}

// spec: corpus.render.local-path@372293e8
#[test]
fn a_machine_local_path_is_a_local_path_finding() {
    assert!(appended(STORE, "A store lives under `.contextful/`.", "SpecLocalPath").is_empty());
    for root in ["/Users", "/home", "~"] {
        let line = format!("A store lives under {root}/someone/project.");
        assert_eq!(appended(STORE, &line, "SpecLocalPath").len(), 1, "{line}");
    }
}
