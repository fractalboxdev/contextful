//! `corpus.state.scaffold`: one failing, tagged test per refusal and limit clause.

use crate::Scratch;
use std::process::Command;

const MODULE: &str = "crates/demo/tests/integration/anatomy.rs";

fn module(s: &Scratch) -> String {
    s.read(MODULE)
}

/// The text of `fn <name>` through its closing brace at column zero.
fn function<'a>(text: &'a str, name: &str) -> &'a str {
    let start = text.find(&format!("fn {name}()")).unwrap_or_else(|| panic!("no fn {name} in\n{text}"));
    let end = text[start..].find("\n}").map(|e| start + e + 2).unwrap();
    &text[start..end]
}

/// The comment and attribute lines directly above `fn <name>`.
fn preamble(text: &str, name: &str) -> Vec<String> {
    let start = text.find(&format!("fn {name}()")).unwrap();
    text[..start]
        .lines()
        .rev()
        .take_while(|l| l.starts_with("//") || l.starts_with("#["))
        .map(str::to_string)
        .collect()
}

// spec: corpus.state.scaffold@2f87c427
#[test]
fn scaffold_writes_one_failing_tagged_test_per_refusal_and_limit() {
    let s = Scratch::copy();
    s.scaffold("corpus.anatomy", "crates/demo");
    let text = module(&s);

    for name in ["bad_anatomy", "statement_words", "file_length"] {
        function(&text, name);
    }
    assert!(!text.contains("fn clause_table()"), "a behavior clause is scaffolded:\n{text}");
    assert!(
        function(&text, "bad_anatomy").contains(r#"todo!("corpus.anatomy.bad-anatomy: assert SpecAnatomy")"#),
        "{text}"
    );
    assert!(
        function(&text, "statement_words").contains(r#"todo!("corpus.anatomy.statement-words: hold 40 words")"#),
        "{text}"
    );
    assert!(
        function(&text, "file_length").contains(r#"todo!("corpus.anatomy.file-length: hold 900 lines")"#),
        "{text}"
    );

    let above = preamble(&text, "statement_words");
    assert!(above.iter().any(|l| l == "#[test]"), "{above:?}");
    assert!(
        above.iter().any(|l| l.starts_with("/// A clause statement holds at most 40 words")),
        "the doc comment quotes the statement: {above:?}"
    );
    let tag = above.iter().find(|l| l.starts_with("// spec: ")).expect("a tag line");
    let rev = tag.strip_prefix("// spec: corpus.anatomy.statement-words@").expect("the tag names the clause");
    assert!(rev.len() == 8 && rev.chars().all(|c| c.is_ascii_hexdigit()), "{tag}");

    assert!(s.read("crates/demo/tests/integration/main.rs").lines().any(|l| l == "mod anatomy;"));

    // The scaffold compiles, and every test it writes fails.
    s.write(
        "crates/demo/Cargo.toml",
        "[package]\nname = \"demo\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\n\n[[test]]\nname = \"integration\"\npath = \"tests/integration/main.rs\"\n",
    );
    s.write("crates/demo/src/lib.rs", "");
    let target = tempfile::tempdir().unwrap();
    let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["test", "--offline", "--manifest-path"])
        .arg(s.root.join("crates/demo/Cargo.toml"))
        .env("CARGO_TARGET_DIR", target.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stdout}\n{stderr}");
    let want = format!("test result: FAILED. 0 passed; {} failed", s.targets("corpus.anatomy"));
    assert!(stdout.contains(&want), "{stdout}\n{stderr}");
    assert!(stdout.contains("corpus.anatomy.bad-anatomy: assert SpecAnatomy"), "{stdout}");
}

#[test]
fn scaffold_keeps_an_existing_test_function_and_mod_line() {
    let s = Scratch::copy();
    s.write("crates/demo/tests/integration/main.rs", "//! Suites.\n\nmod other;\n");
    s.write(
        MODULE,
        "#[test]\nfn statement_words() {\n    assert_eq!(1 + 1, 2);\n}\n",
    );
    s.scaffold("corpus.anatomy", "crates/demo");
    let text = module(&s);
    assert_eq!(text.matches("fn statement_words()").count(), 1, "{text}");
    assert!(function(&text, "statement_words").contains("assert_eq!(1 + 1, 2);"), "{text}");
    function(&text, "bad_anatomy");
    function(&text, "file_length");
    let main = s.read("crates/demo/tests/integration/main.rs");
    assert_eq!(main, "//! Suites.\n\nmod anatomy;\nmod other;\n");

    s.scaffold("corpus.anatomy", "crates/demo");
    assert_eq!(module(&s), text, "a second run changes nothing");
    assert_eq!(s.read("crates/demo/tests/integration/main.rs"), main);
}

#[test]
fn the_tag_rev_is_the_first_8_hex_of_the_statement_digest() {
    let s = Scratch::copy();
    let corpus = s.read("spec/00-corpus.md");
    let row = corpus.lines().find(|l| l.starts_with("| `corpus.anatomy.statement-words` |")).unwrap().to_string();
    let fixed = "| `corpus.anatomy.statement-words` | A clause statement holds at most 40 words. | because one row states one obligation |";
    s.write("spec/00-corpus.md", &corpus.replacen(&row, fixed, 1));
    s.scaffold("corpus.anatomy", "crates/demo");
    let tags: Vec<String> = module(&s).lines().filter(|l| l.starts_with("// spec: ")).map(str::to_string).collect();
    assert!(tags.iter().any(|t| t == "// spec: corpus.anatomy.statement-words@60ce64cb"), "{tags:?}");
}

#[test]
fn a_hyphenated_operation_scaffolds_into_a_snake_case_module() {
    let s = Scratch::copy();
    let lock: serde_json::Value = serde_json::from_str(&s.read("spec/spec.lock.json")).unwrap();
    let wanted: Vec<String> = lock["clauses"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["contract"] == "store" && c["operation"] == "lay-out" && c["kind"] != "behavior")
        .map(|c| c["subject"].as_str().unwrap().replace('-', "_"))
        .collect();
    assert!(!wanted.is_empty(), "store.lay-out holds a refusal or limit");
    s.scaffold("store.lay-out", "crates/demo");
    let text = s.read("crates/demo/tests/integration/lay_out.rs");
    for name in &wanted {
        function(&text, name);
    }
    assert!(s.read("crates/demo/tests/integration/main.rs").lines().any(|l| l == "mod lay_out;"));
}

#[test]
fn scaffold_refuses_an_unknown_operation() {
    let s = Scratch::copy();
    let out = s.cmd(&["scaffold", "corpus.no-such-operation", "--package", "crates/demo"]);
    assert!(!out.status.success());
    assert!(!s.root.join("crates/demo").exists());
}
