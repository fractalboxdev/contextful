//! `corpus.state`: a `// spec: <id>@<rev>` tag above a test function pins its clause.

use crate::{codes, Scratch};

const MODULE: &str = "crates/x/tests/integration/anatomy.rs";

/// Scaffold `corpus.anatomy` into `crates/x`, then replace every placeholder body.
fn implemented(s: &Scratch) {
    s.scaffold("corpus.anatomy", "crates/x");
    let text: Vec<String> = s
        .read(MODULE)
        .lines()
        .map(|l| if l.trim_start().starts_with("todo!") { "    assert_eq!(1 + 1, 2);".to_string() } else { l.to_string() })
        .collect();
    s.write(MODULE, &(text.join("\n") + "\n"));
}

fn verdict(s: &Scratch, clause: &str) -> String {
    assert!(s.cmd(&["state"]).status.success());
    let status = s.read("spec/status.md");
    let row = status
        .lines()
        .find(|l| l.starts_with(&format!("| `{clause}` |")))
        .unwrap_or_else(|| panic!("no pin row for {clause} in\n{status}"));
    row.trim_end_matches(" |").rsplit("| ").next().unwrap().to_string()
}

// spec: corpus.state.tag-pin@9b4060f5
#[test]
fn a_tag_pin_resolves_to_performed() {
    let s = Scratch::copy();
    implemented(&s);
    let found = s.lint("state");
    assert!(codes(&found, "SpecBrokenPin").is_empty(), "{found:?}");
    assert!(codes(&found, "SpecStalePin").is_empty(), "{found:?}");
    assert_eq!(verdict(&s, "corpus.anatomy.statement-words"), "performed");
    assert_eq!(verdict(&s, "corpus.anatomy.bad-anatomy"), "performed");
}

#[test]
fn a_tag_pin_counts_toward_the_floor() {
    let s = Scratch::copy();
    implemented(&s);
    assert!(s.cmd(&["pins"]).status.success());
    assert!(s.read("spec/pins.toml").lines().any(|l| l == format!("corpus = {}", s.targets("corpus.anatomy"))), "{}", s.read("spec/pins.toml"));
}

// spec: corpus.state.stale-pin@254811c3
#[test]
fn a_tag_with_a_stale_rev_raises_spec_stale_pin() {
    let s = Scratch::copy();
    implemented(&s);
    let text = s.read(MODULE);
    let tag = text.lines().find(|l| l.starts_with("// spec: corpus.anatomy.statement-words@")).unwrap().to_string();
    s.write(MODULE, &text.replacen(&tag, "// spec: corpus.anatomy.statement-words@deadbeef", 1));
    let stale = codes(&s.lint("state"), "SpecStalePin");
    assert_eq!(stale.len(), 1, "{stale:?}");
    assert!(stale[0].contains("corpus.anatomy.statement-words"), "{stale:?}");
    assert_eq!(verdict(&s, "corpus.anatomy.statement-words"), "broken");
}

// spec: corpus.state.unfinished-test@349c95a5
#[test]
fn a_tag_on_a_todo_body_is_broken() {
    let s = Scratch::copy();
    s.scaffold("corpus.anatomy", "crates/x");
    let broken = codes(&s.lint("state"), "SpecBrokenPin");
    assert_eq!(broken.len(), s.targets("corpus.anatomy"), "{broken:?}");
    assert!(broken.iter().all(|m| m.contains("todo!")), "{broken:?}");
    assert_eq!(verdict(&s, "corpus.anatomy.file-length"), "broken");
}

#[test]
fn an_ignored_tagged_test_is_broken() {
    let s = Scratch::copy();
    implemented(&s);
    let text = s.read(MODULE);
    s.write(MODULE, &text.replacen("#[test]\nfn file_length()", "#[test]\n#[ignore = \"red\"]\nfn file_length()", 1));
    let broken = codes(&s.lint("state"), "SpecBrokenPin");
    assert_eq!(broken.len(), 1, "{broken:?}");
    assert!(broken[0].contains("ignore") && broken[0].contains("corpus.anatomy.file-length"), "{broken:?}");
    assert_eq!(verdict(&s, "corpus.anatomy.file-length"), "broken");
}

#[test]
fn a_tag_naming_no_clause_is_a_broken_pin() {
    let s = Scratch::copy();
    s.write(
        "crates/x/tests/integration/nothing.rs",
        "// spec: corpus.anatomy.no-such-clause@00000000\n#[test]\nfn nothing() {}\n",
    );
    let broken = codes(&s.lint("state"), "SpecBrokenPin");
    assert_eq!(broken.len(), 1, "{broken:?}");
    assert!(broken[0].contains("corpus.anatomy.no-such-clause"), "{broken:?}");
}

#[test]
fn a_clause_pinned_to_two_different_tests_is_a_broken_pin() {
    let s = Scratch::copy();
    implemented(&s);
    s.write("crates/x/src/lib.rs", "#[test]\nfn another_demonstration() {}\n");
    s.pin("corpus.anatomy.file-length", "test", "x::another_demonstration");
    let broken = codes(&s.lint("state"), "SpecBrokenPin");
    assert_eq!(broken.len(), 1, "{broken:?}");
    assert!(broken[0].contains("another_demonstration") && broken[0].contains("file_length"), "{broken:?}");
}

#[test]
fn one_test_pinned_both_ways_is_one_pin() {
    let s = Scratch::copy();
    implemented(&s);
    s.pin("corpus.anatomy.file-length", "test", "x::anatomy::file_length");
    let found = s.lint("state");
    assert!(codes(&found, "SpecBrokenPin").is_empty(), "{found:?}");
    assert_eq!(verdict(&s, "corpus.anatomy.file-length"), "performed");
}
