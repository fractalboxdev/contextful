//! `corpus.state`: a `// spec: <id>@<rev>` tag above a test function pins its clause.

use crate::{codes, Scratch};

const MODULE: &str = "crates/x/tests/integration/anatomy.rs";

/// Scaffold `corpus.anatomy` into `crates/x`, then replace every placeholder body.
fn implemented(s: &Scratch) {
    s.scaffold("corpus.anatomy", "crates/x");
    s.implement(MODULE);
}

fn verdict(s: &Scratch, clause: &str) -> String {
    s.verdict(clause).unwrap_or_else(|| panic!("no pin entry for {clause} in the lock file"))
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

// spec: corpus.state.several-tests@d4b76042
#[test]
fn a_clause_pinned_to_two_tests_performs_when_both_perform() {
    let s = Scratch::copy();
    implemented(&s);
    s.write("crates/x/src/lib.rs", "#[test]\nfn another_demonstration() {\n    assert!(true);\n}\n");
    s.pin("corpus.anatomy.file-length", "test", "x::another_demonstration");
    assert!(codes(&s.lint("state"), "SpecBrokenPin").is_empty(), "{:?}", s.lint("state"));
    assert_eq!(verdict(&s, "corpus.anatomy.file-length"), "performed");
    s.write("crates/x/src/lib.rs", "#[test]\n#[ignore]\nfn another_demonstration() {}\n");
    assert_eq!(verdict(&s, "corpus.anatomy.file-length"), "broken");
}

// spec: corpus.state.unexercised@8dcfa6a3
#[test]
fn a_refusal_pin_whose_test_names_none_of_its_errors_is_unexercised() {
    let s = Scratch::copy();
    implemented(&s);
    assert_eq!(verdict(&s, "corpus.anatomy.bad-anatomy"), "performed");
    let text = s.read(MODULE);
    let named = text.lines().find(|l| l.contains("corpus.anatomy.bad-anatomy: assert SpecAnatomy")).unwrap().to_string();
    s.write(MODULE, &text.replacen(&named, "    assert_eq!(1 + 1, 2);", 1));
    let broken = codes(&s.lint("state"), "SpecBrokenPin");
    assert_eq!(broken.len(), 1, "{broken:?}");
    assert!(broken[0].contains("corpus.anatomy.bad-anatomy") && broken[0].contains("SpecAnatomy"), "{broken:?}");
    assert_eq!(verdict(&s, "corpus.anatomy.bad-anatomy"), "broken");
    s.write(MODULE, &text.replacen(&named, "    assert!(matches!(found, Error::Anatomy));", 1));
    assert_eq!(verdict(&s, "corpus.anatomy.bad-anatomy"), "broken", "a variant path names the whole id or a prefix-free suffix");
    s.write(MODULE, &text.replacen(&named, "    assert!(matches!(found, CorpusError::SpecAnatomy));", 1));
    assert_eq!(verdict(&s, "corpus.anatomy.bad-anatomy"), "performed");
}

#[test]
fn a_brace_inside_a_string_char_or_comment_does_not_end_the_test_body() {
    let s = Scratch::copy();
    implemented(&s);
    let text = s.read(MODULE);
    let named = text.lines().find(|l| l.contains("corpus.anatomy.bad-anatomy: assert SpecAnatomy")).unwrap().to_string();
    let braces = "    let _ = (\"]}\", r#\"}\"#, '}'); // }\n    /* } */\n";
    s.write(MODULE, &text.replacen(&named, &format!("{braces}{named}"), 1));
    assert_eq!(verdict(&s, "corpus.anatomy.bad-anatomy"), "performed");
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

// spec: corpus.state.limit-const@e888dd76
#[test]
fn a_limit_pin_performs_only_while_non_test_source_uses_its_registered_const() {
    let s = Scratch::copy();
    implemented(&s);
    let clause = "corpus.anatomy.statement-words";
    assert_eq!(verdict(&s, clause), "performed");
    let terms = s.read("spec/terms/corpus.toml");
    let entry = terms.lines().find(|l| l.starts_with("corpus-statement-words ")).unwrap().to_string();
    s.write("spec/terms/corpus.toml", &terms.replacen(&entry, &entry.replacen(" }", ", const = \"crates/x/src/lib.rs::MAX_WORDS\" }", 1), 1));
    s.write("crates/x/src/lib.rs", "pub const MAX_WORDS: usize = 40;\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn bound() {\n        assert_eq!(super::MAX_WORDS, 40);\n    }\n}\n");
    let broken = codes(&s.lint("state"), "SpecBrokenPin");
    assert_eq!(broken.len(), 1, "{broken:?}");
    assert!(broken[0].contains(clause) && broken[0].contains("MAX_WORDS"), "{broken:?}");
    assert_eq!(verdict(&s, clause), "broken");
    s.write("crates/x/src/lib.rs", "pub const MAX_WORDS: usize = 40;\n\npub fn admitted(words: usize) -> bool {\n    words <= MAX_WORDS\n}\n");
    assert_eq!(verdict(&s, clause), "performed");
    s.write("crates/x/src/lib.rs", "pub const OTHER: usize = 40;\n");
    assert_eq!(verdict(&s, clause), "broken", "a const path resolving to nothing exercises nothing");
}

#[test]
fn a_limit_const_named_only_in_comments_strings_or_a_const_assert_is_unread() {
    let s = Scratch::copy();
    implemented(&s);
    let clause = "corpus.anatomy.statement-words";
    let terms = s.read("spec/terms/corpus.toml");
    let entry = terms.lines().find(|l| l.starts_with("corpus-statement-words ")).unwrap().to_string();
    s.write("spec/terms/corpus.toml", &terms.replacen(&entry, &entry.replacen(" }", ", const = \"crates/x/src/lib.rs::MAX_WORDS\" }", 1), 1));
    s.write("crates/x/src/lib.rs", "pub const MAX_WORDS: usize = 40;\n");
    for unread in [
        "// MAX_WORDS bounds a statement.\n",
        "/// Refuses past MAX_WORDS.\npub fn admitted() {}\n",
        "/* MAX_WORDS */\n",
        "pub fn named() -> &'static str {\n    \"MAX_WORDS\"\n}\n",
        "pub fn raw() -> &'static str {\n    r#\"MAX_WORDS\"#\n}\n",
        "const _: () = assert!(MAX_WORDS == 40, \"forty words\");\n",
    ] {
        s.write("crates/x/src/other.rs", unread);
        assert_eq!(verdict(&s, clause), "broken", "{unread}");
    }
    s.write("crates/x/src/other.rs", "// MAX_WORDS\npub fn admitted(words: usize) -> bool {\n    words <= crate::MAX_WORDS // the bound\n}\n");
    assert_eq!(verdict(&s, clause), "performed");
}
