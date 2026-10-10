//! `corpus.anatomy`: the section order of a contract file, what follows a clause list,
//! the computed kind, and the statement and file bounds.

use crate::{codes, Scratch};

const STORE: &str = "spec/10-store.md";

fn lock(s: &Scratch) -> serde_json::Value {
    assert!(s.cmd(&["extract"]).status.success());
    serde_json::from_str(&s.read("spec/spec.lock.json")).unwrap()
}

fn kind(lock: &serde_json::Value, id: &str) -> String {
    lock["clauses"].as_array().unwrap().iter().find(|c| c["id"] == id).map(|c| c["kind"].as_str().unwrap().to_string()).unwrap_or_default()
}

/// Insert `text` after the clause list of `## <operation>` in the store file.
fn after_list(s: &Scratch, operation: &str, text: &str) {
    let body = s.read(STORE);
    let at = body.find(&format!("## {operation}\n")).expect("operation section");
    let list = at + body[at..].find("\n- `").expect("a clause list");
    let end = list + body[list..].find("\n\n").unwrap();
    s.write(STORE, &format!("{}\n\n{text}{}", &body[..end], &body[end..]));
}

// spec: corpus.anatomy.file-headings@dbcd05f2
#[test]
fn sections_out_of_owns_order_or_a_second_title_are_anatomy_findings() {
    let s = Scratch::copy();
    assert!(codes(&s.lint("anatomy"), "SpecAnatomy").is_empty());
    let body = s.read(STORE);
    s.write(STORE, &body.replacen("  - init\n  - declare\n", "  - declare\n  - init\n", 1));
    let found = codes(&s.lint("anatomy"), "SpecAnatomy");
    assert!(found.iter().any(|m| m.contains("differ from owns order")), "{found:?}");

    s.write(STORE, &format!("{body}\n# A second title\n"));
    let found = codes(&s.lint("anatomy"), "SpecAnatomy");
    assert!(found.iter().any(|m| m.contains("2 `# ` titles, want 1")), "{found:?}");
}

// spec: corpus.anatomy.after-list@34efdd0c
#[test]
fn prose_tables_and_diagrams_after_the_list_state_no_obligation() {
    let s = Scratch::copy();
    let before = s.clauses_of("store.fold");
    assert!(!before.is_empty());
    after_list(
        &s,
        "fold",
        "A pass reads every run since the last one.\n\n| Trigger | Fires |\n| --- | --- |\n| runs | a pass |\n\n```mermaid\nflowchart LR\n  R[\"run\"] -->|\"folds into\"| S[(\"snapshot store\")]\n```\n",
    );
    assert!(codes(&s.lint("anatomy"), "SpecAnatomy").is_empty(), "{:?}", s.lint("anatomy"));
    assert!(s.cmd(&["extract"]).status.success());
    assert_eq!(s.clauses_of("store.fold"), before);
}

// spec: corpus.anatomy.kind-is-computed@4e9056dc
#[test]
fn the_lock_carries_the_kind_the_fragment_assigns() {
    let s = Scratch::copy();
    let lock = lock(&s);
    assert_eq!(kind(&lock, "store.fold.partial-snapshot"), "refusal");
    assert_eq!(kind(&lock, "store.fold.triggers"), "limit");
    assert_eq!(kind(&lock, "store.fold.includes-runs"), "behavior");

    let fragment = s.read("spec/terms/store.toml");
    let entry = fragment.lines().find(|l| l.starts_with("StorePartialSnapshot ")).expect("the error entry").to_string();
    s.write("spec/terms/store.toml", &fragment.replacen(&entry, &entry.replace("store.fold.partial-snapshot", "store.fold.includes-runs"), 1));
    let lock = self::lock(&s);
    assert_eq!(kind(&lock, "store.fold.partial-snapshot"), "behavior");
}

/// Replace the statement of the `includes-runs` item with `words` copies of `word`.
fn statement_of(s: &Scratch, words: usize) {
    let body = s.read(STORE);
    let row = body.lines().find(|l| l.starts_with("- `includes-runs` — ")).unwrap().to_string();
    s.write(STORE, &body.replacen(&row, &format!("- `includes-runs` — {}.", vec!["word"; words].join(" ")), 1));
}

// spec: corpus.anatomy.statement-words@11227773
#[test]
fn a_statement_over_forty_words_is_an_anatomy_finding_and_forty_is_admitted() {
    let s = Scratch::copy();
    statement_of(&s, 40);
    assert!(codes(&s.lint("anatomy"), "SpecAnatomy").is_empty());
    statement_of(&s, 41);
    let found = codes(&s.lint("anatomy"), "SpecAnatomy");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("`store.fold.includes-runs` statement is 41 words, over 40"), "{found:?}");
}

/// Pad the store file with trailing prose lines until it holds `lines` lines.
fn padded(s: &Scratch, lines: usize) {
    let body = s.read(STORE);
    let have = body.lines().count();
    assert!(have < lines, "the store file holds {have} lines");
    s.write(STORE, &format!("{body}{}", "Filler prose.\n".repeat(lines - have)));
}

// spec: corpus.anatomy.file-length@792693d3
#[test]
fn a_contract_file_over_nine_hundred_lines_is_an_anatomy_finding() {
    let s = Scratch::copy();
    let body = s.read(STORE);
    padded(&s, 900);
    assert!(codes(&s.lint("anatomy"), "SpecAnatomy").is_empty());
    s.write(STORE, &body);
    padded(&s, 901);
    let found = codes(&s.lint("anatomy"), "SpecAnatomy");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("901 lines exceed 900"), "{found:?}");
}
