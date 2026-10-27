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
    // The inserted block counts toward the file bound, which `corpus.anatomy` owns separately.
    let lines = s.read(STORE).lines().count();
    let bound: Vec<String> = if lines > 900 { vec![format!("{lines} lines exceed 900")] } else { vec![] };
    assert_eq!(codes(&s.lint("anatomy"), "SpecAnatomy"), bound, "{:?}", s.lint("anatomy"));
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

/// Put a `### <group>` heading and `lede` before the item of `## <operation>` with subject `subject`.
fn group_before(s: &Scratch, subject: &str, group: &str, lede: &str) {
    let body = s.read(STORE);
    let row = format!("\n- `{subject}` — ");
    let at = body.find(&row).unwrap_or_else(|| panic!("no `{subject}` item"));
    s.write(STORE, &format!("{}\n\n### {group}\n\n{lede}\n{}", &body[..at], &body[at..]));
}

fn lock_row(lock: &serde_json::Value, id: &str) -> serde_json::Value {
    lock["clauses"].as_array().unwrap().iter().find(|c| c["id"] == id).cloned().unwrap_or_else(|| panic!("no clause `{id}`"))
}

// spec: corpus.anatomy.group@a39ff2b4
#[test]
fn a_group_heading_splits_a_list_without_changing_an_id() {
    let s = Scratch::copy();
    let before = s.clauses_of("store.fold");
    group_before(&s, "lost-pointer", "collection", "A pass that loses its pointer, and the staging it leaves behind.");
    assert!(codes(&s.lint("anatomy"), "SpecAnatomy").is_empty(), "{:?}", s.lint("anatomy"));
    let lock = lock(&s);
    assert_eq!(s.clauses_of("store.fold"), before);
    assert_eq!(lock_row(&lock, "store.fold.lost-pointer")["group"], "collection");
    assert_eq!(lock_row(&lock, "store.fold.supersedes")["group"], "collection");
    assert!(lock_row(&lock, "store.fold.triggers").get("group").is_none());
    assert_eq!(lock["groups"]["store.fold.collection"], "A pass that loses its pointer, and the staging it leaves behind.");
}

#[test]
fn a_group_without_a_one_sentence_lede_or_after_other_content_is_an_anatomy_finding() {
    let s = Scratch::copy();
    group_before(&s, "lost-pointer", "collection", "A pass that loses its pointer. Staging it leaves behind.");
    let found = codes(&s.lint("anatomy"), "SpecAnatomy");
    assert!(found.iter().any(|m| m.contains("group `fold.collection` lede is 2 sentences, want 1")), "{found:?}");

    let s = Scratch::copy();
    group_before(&s, "lost-pointer", "collection", "");
    let found = codes(&s.lint("anatomy"), "SpecAnatomy");
    assert!(found.iter().any(|m| m.contains("group `fold.collection` opens without a lede")), "{found:?}");

    let s = Scratch::copy();
    after_list(&s, "fold", "A pass reads every run since the last one.\n\n### late\n\nClauses after prose.\n\n- `late-rule` — A late rule holds.\n");
    let found = codes(&s.lint("anatomy"), "SpecAnatomy");
    assert!(found.iter().any(|m| m.contains("group `fold.late` follows content after a clause list")), "{found:?}");
}

/// Set the corpus fragment line starting `prefix` to `line`.
fn corpus_line(s: &Scratch, prefix: &str, line: &str) {
    let text = s.read("spec/terms/corpus.toml");
    let old = text.lines().find(|l| l.starts_with(prefix)).unwrap_or_else(|| panic!("no `{prefix}` line")).to_string();
    s.write("spec/terms/corpus.toml", &text.replacen(&old, line, 1));
}

fn long_lists(s: &Scratch) -> usize {
    let found = codes(&s.lint("anatomy"), "SpecAnatomy");
    let m = found.iter().find_map(|m| m.strip_prefix("ratchet `anatomy-long-lists` counts ")).unwrap_or_else(|| panic!("{found:?}"));
    m.split(',').next().unwrap().parse().unwrap()
}

// spec: corpus.anatomy.group-clauses@ee765807
#[test]
fn each_list_over_its_bound_counts_toward_the_long_list_ceiling() {
    let s = Scratch::copy();
    corpus_line(&s, "anatomy-long-lists ", "anatomy-long-lists = 100000");
    let live = long_lists(&s);
    corpus_line(&s, "corpus-group-clauses ", "corpus-group-clauses = { clause = \"corpus.anatomy.group-clauses\", value = 20, unit = \"clauses\", basis = \"chosen\", gloss = \"Longest clause list.\" }");
    let tighter = long_lists(&s);
    assert!(tighter > live, "{tighter} > {live}");
    group_before(&s, "lost-pointer", "collection", "A pass that loses its pointer, and the staging it leaves behind.");
    assert_eq!(long_lists(&s), tighter - 1, "store.fold's 24 items split into 20 and 4");
    corpus_line(&s, "anatomy-long-lists ", &format!("anatomy-long-lists = {}", tighter - 1));
    assert!(codes(&s.lint("anatomy"), "SpecAnatomy").is_empty(), "{:?}", s.lint("anatomy"));
}
