//! Pin upkeep: `pins --migrate`, `retag`, and `slice --file` / `slice --diff`, which list
//! the clauses a test file or a change reaches.

use crate::{codes, Scratch};
use std::process::Command;

const CLAUSE: &str = "corpus.anatomy.statement-words";
const OTHER: &str = "corpus.anatomy.file-length";
const FILE: &str = "crates/x/tests/integration/words.rs";

/// `clause`'s statement in the scratch lock file.
fn statement(s: &Scratch, clause: &str) -> String {
    assert!(s.cmd(&["extract"]).status.success());
    let lock: serde_json::Value = serde_json::from_str(&s.read("spec/spec.lock.json")).unwrap();
    lock["clauses"].as_array().unwrap().iter().find(|c| c["id"] == clause).unwrap()["statement"].as_str().unwrap().to_string()
}

/// The `// spec:` tag scaffold wrote for `clause` into `crates/x/tests/integration/anatomy.rs`.
fn scaffolded_tag(s: &Scratch, clause: &str) -> String {
    s.scaffold("corpus.anatomy", "crates/x");
    let text = s.read("crates/x/tests/integration/anatomy.rs");
    s.write("crates/x/tests/integration/anatomy.rs", "");
    text.lines().find(|l| l.starts_with(&format!("// spec: {clause}@"))).unwrap().to_string()
}

fn ok(out: &std::process::Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// spec: corpus.state.migrate@70d6c6a0
#[test]
fn migrate_turns_each_test_entry_into_a_tag_above_its_test() {
    let s = Scratch::copy();
    s.write("crates/x/src/lib.rs", "pub struct Lede;\n\npub fn words_over_forty_refused() {}\n");
    s.write(FILE, "/// The statement copy.\n#[test]\nfn words_over_forty_refused() {\n    assert_eq!(1, 1);\n}\n");
    s.pin(CLAUSE, "test", "x::words::words_over_forty_refused");
    s.pin("corpus.anatomy.lede", "item", "x::Lede");
    ok(&s.cmd(&["pins", "--migrate"]));
    let pins = s.read("spec/pins.toml");
    assert!(!pins.contains("test ="), "{pins}");
    assert!(pins.contains("\"corpus.anatomy.lede\" = { item = \"x::Lede\" }"), "{pins}");
    let text = s.read(FILE);
    let tag = text.lines().nth(1).unwrap();
    assert!(tag.starts_with(&format!("// spec: {CLAUSE}@")), "{text}");
    assert_eq!(text.lines().nth(2), Some("#[test]"), "{text}");
    assert_eq!(s.read("crates/x/src/lib.rs"), "pub struct Lede;\n\npub fn words_over_forty_refused() {}\n");
    assert_eq!(s.verdict(CLAUSE).as_deref(), Some("performed"));
    ok(&s.cmd(&["pins", "--migrate"]));
    assert_eq!(s.read(FILE), text, "a second run changes nothing");
}

#[test]
fn migrate_writes_one_tag_line_per_clause_above_a_shared_test() {
    let s = Scratch::copy();
    s.write(FILE, "fn helper() {}\n\n/// Two clauses.\n#[test]\nfn shared() {\n    assert_eq!(1, 1);\n}\n");
    s.pin(CLAUSE, "test", "x::words::shared");
    s.pin(OTHER, "test", "x::words::shared");
    ok(&s.cmd(&["pins", "--migrate"]));
    let text = s.read(FILE);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[..3], ["fn helper() {}", "", "/// Two clauses."], "{text}");
    assert!(lines[3..5].iter().all(|l| l.starts_with("// spec: corpus.anatomy.")), "{text}");
    assert_eq!(lines[5..7], ["#[test]", "fn shared() {"], "{text}");
    assert_eq!(s.verdict(CLAUSE).as_deref(), Some("performed"));
    assert_eq!(s.verdict(OTHER).as_deref(), Some("performed"));
}

// spec: corpus.state.retag@cf44e302
#[test]
fn retag_rewrites_every_tag_of_a_reworded_clause_and_a_close_statement_copy() {
    let s = Scratch::copy();
    let old = statement(&s, CLAUSE);
    let tag = scaffolded_tag(&s, CLAUSE);
    s.write(FILE, &format!("/// {old}\n{tag}\n#[test]\nfn copied() {{}}\n\n/// Something else entirely.\n{tag}\n#[test]\nfn described() {{}}\n"));
    let corpus = s.read("spec/00-corpus.md");
    s.write("spec/00-corpus.md", &corpus.replacen(&old, &old.replacen("A clause statement", "A clause's statement", 1), 1));
    let new = statement(&s, CLAUSE);
    assert_ne!(new, old);
    assert_eq!(codes(&s.lint("state"), "SpecStalePin").len(), 2);
    let printed = ok(&s.cmd(&["retag", CLAUSE]));
    assert!(printed.contains(&format!("{FILE}::copied")) && printed.contains(&format!("{FILE}::described")), "{printed}");
    assert!(codes(&s.lint("state"), "SpecStalePin").is_empty(), "{:?}", s.lint("state"));
    let text = s.read(FILE);
    assert!(text.starts_with(&format!("/// {new}\n")), "{text}");
    assert!(text.contains("/// Something else entirely.\n"), "{text}");
}

#[test]
fn retag_refuses_while_lint_reports_a_finding_retag_does_not_repair() {
    let s = Scratch::copy();
    let old = statement(&s, CLAUSE);
    let corpus = s.read("spec/00-corpus.md");
    let long = format!("{old} {}", "word ".repeat(40).trim_end());
    s.write("spec/00-corpus.md", &corpus.replacen(&old, &long, 1));
    let out = s.cmd(&["retag", CLAUSE]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("SpecAnatomy"), "{}", String::from_utf8_lossy(&out.stderr));
}

// spec: corpus.state.slice-file@b0214fdd
#[test]
fn slice_file_lists_each_clause_the_file_tags_with_its_current_statement() {
    let s = Scratch::copy();
    let (a, b) = (scaffolded_tag(&s, CLAUSE), scaffolded_tag(&s, OTHER));
    s.write(FILE, &format!("{a}\n#[test]\nfn words() {{}}\n\n{b}\n#[test]\nfn length() {{}}\n"));
    let v: serde_json::Value = serde_json::from_str(&ok(&s.cmd(&["slice", "--file", FILE, "--json"]))).unwrap();
    let ids: Vec<&str> = v.as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(ids, [OTHER, CLAUSE]);
    assert_eq!(v[1]["statement"].as_str().unwrap(), statement(&s, CLAUSE));
    assert_eq!(v[1]["sites"][0], format!("{FILE}::words"));
}

fn git(s: &Scratch, args: &[&str]) {
    let out = Command::new("git").args(args).current_dir(&s.root).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

// spec: corpus.state.slice-diff@f46eee8e
#[test]
fn slice_diff_lists_the_clauses_whose_tagged_tests_the_change_edits() {
    let s = Scratch::copy();
    let (a, b) = (scaffolded_tag(&s, CLAUSE), scaffolded_tag(&s, OTHER));
    s.write(FILE, &format!("{a}\n#[test]\nfn words() {{\n    assert_eq!(1, 1);\n}}\n\n{b}\n#[test]\nfn length() {{\n    assert_eq!(2, 2);\n}}\n"));
    git(&s, &["init", "-q", "-b", "main"]);
    git(&s, &["-c", "user.name=t", "-c", "user.email=t@example.com", "add", "-A"]);
    git(&s, &["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "base"]);
    s.write(FILE, &s.read(FILE).replace("assert_eq!(1, 1);", "assert_eq!(1, 1);\n    assert_ne!(1, 2);"));
    let printed = ok(&s.cmd(&["slice", "--diff", "HEAD"]));
    assert!(printed.contains(CLAUSE) && printed.contains(&statement(&s, CLAUSE)), "{printed}");
    assert!(!printed.contains(OTHER), "{printed}");
}
