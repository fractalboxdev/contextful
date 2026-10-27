//! `corpus.reference`: a `{{type:<path>}}` pointer resolves to a struct or enum, the lock
//! file and the card carry its members, and a clause listing a type's keys draws an advisory.

use crate::{codes, Scratch};

const STORE: &str = "spec/10-store.md";

const SHAPE: &str = r#"use serde::{Deserialize, Serialize};

/// A shape block.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shape {
    /// The shape's name.
    pub name: String,
    /// Columns the shape sorts by,
    /// first to last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_columns: Option<Vec<String>>,
    #[serde(rename = "max-rows")]
    pub max_rows: Option<u32>,
    pub fill_mode: Option<Fill>,
    #[serde(skip)]
    pub cached: bool,
}

/// How a shape fills a gap.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fill {
    /// Carry the last value forward.
    CarryForward,
    Zero,
}

/// A helper no declaration reads.
pub struct Plain {
    pub left: u8,
    pub right: u8,
    pub middle: u8,
}
"#;

/// A scratch corpus beside a `contextful-demo` package whose `shape` module defines `Shape`.
fn with_shape() -> Scratch {
    let s = Scratch::copy();
    s.write("crates/demo/Cargo.toml", "[package]\nname = \"contextful-demo\"\nversion = \"0.0.0\"\n");
    s.write("crates/demo/src/lib.rs", "pub mod shape;\n");
    s.write("crates/demo/src/shape.rs", SHAPE);
    s
}

/// Append clause `subject` to the fold operation's clause list.
fn add_clause(s: &Scratch, subject: &str, statement: &str) {
    let body = s.read(STORE);
    let at = body.find("## fold\n").expect("fold section");
    let list = at + body[at..].find("\n- `").expect("a clause list") + 1;
    s.write(STORE, &format!("{}- `{subject}` — {statement}\n{}", &body[..list], &body[list..]));
}

// spec: corpus.reference.type-pointer@29024c4b
#[test]
fn a_type_pointer_records_each_member_with_its_spelling_and_doc() {
    let s = with_shape();
    add_clause(&s, "shape-keys", "A shape block takes {{type:contextful_demo::shape::Shape}}.");
    add_clause(&s, "fill-modes", "A shape fills gaps by {{type:contextful_demo::shape::Fill}}.");
    let out = s.cmd(&["state"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let lock: serde_json::Value = serde_json::from_str(&s.read("spec/spec.lock.json")).unwrap();
    let shape = &lock["types"]["contextful_demo::shape::Shape"];
    assert_eq!(shape["file"], "crates/demo/src/shape.rs", "{shape}");
    assert!(shape["line"].is_null(), "a type records its file, not a line: {shape}");
    assert_eq!(shape["kind"], "struct");
    assert_eq!(shape["clauses"], serde_json::json!(["store.fold.shape-keys"]));
    let members: Vec<(String, String)> = shape["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["name"].as_str().unwrap().to_string(), m["doc"].as_str().unwrap_or("").to_string()))
        .collect();
    let names: Vec<&str> = members.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["name", "sort_columns", "max-rows", "fill_mode"], "a skipped field is no key; a rename is the spelling");
    assert_eq!(members[1].1, "Columns the shape sorts by, first to last.");
    let fill: Vec<&str> = lock["types"]["contextful_demo::shape::Fill"]["members"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap()).collect();
    assert_eq!(fill, ["carry_forward", "zero"]);
    let pointers = lock["pointers"].as_array().unwrap();
    assert!(pointers.iter().any(|p| p[1].as_str().is_some_and(|t| t.starts_with("store."))), "the lock keeps its clause edges");
    assert!(!pointers.iter().any(|p| p[1].as_str().is_some_and(|t| t.starts_with("type:"))), "a type pointer is no clause edge");

    let card = s.read("spec/cards/store.md");
    assert!(card.contains("## Types"), "{card}");
    assert!(card.contains("`contextful_demo::shape::Shape`"), "{card}");
    assert!(card.contains("The struct at `crates/demo/src/shape.rs`, named by"), "{card}");
    for line in ["| `sort_columns` | Columns the shape sorts by, first to last. |", "| `max-rows` |  |", "| `carry_forward` | Carry the last value forward. |"] {
        assert!(card.contains(line), "{line}\n{card}");
    }
}

// spec: corpus.reference.type-unresolved@a99521a6
#[test]
fn a_type_pointer_to_no_struct_or_enum_in_its_module_is_unresolved() {
    let s = with_shape();
    add_clause(&s, "shape-keys", "A shape block takes {{type:contextful_demo::shape::Shape}}.");
    let found = s.lint("reference");
    assert!(codes(&found, "SpecTypeUnresolved").is_empty(), "{found:?}");
    assert!(codes(&found, "SpecDanglingReference").is_empty(), "a type pointer is no clause pointer: {found:?}");
    for path in ["contextful_demo::shape::Missing", "contextful_demo::other::Shape", "contextful_absent::shape::Shape"] {
        let s = with_shape();
        add_clause(&s, "shape-keys", &format!("A shape block takes {{{{type:{path}}}}}."));
        let found = codes(&s.lint("reference"), "SpecTypeUnresolved");
        assert_eq!(found.len(), 1, "{path}: {found:?}");
        assert!(found[0].contains(path), "{found:?}");
        let out = s.cmd(&["state"]);
        assert!(out.status.success(), "an unresolved pointer leaves the render to lint: {}", String::from_utf8_lossy(&out.stderr));
    }
}

// spec: corpus.reference.restates-code@1d42408a
#[test]
fn a_clause_backticking_three_keys_of_one_type_draws_an_advisory() {
    let advisories = |s: &Scratch| -> Vec<String> {
        let out = s.cmd(&["state"]);
        assert!(out.status.success(), "the advisory refuses nothing: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stderr).lines().filter(|l| l.contains("SpecRestatesCode")).map(str::to_string).collect()
    };
    let s = with_shape();
    add_clause(&s, "shape-keys", "A shape block declares `name`, `sort_columns` and `max-rows`.");
    add_clause(&s, "two-keys", "A shape block declares `name` and `sort_columns`.");
    add_clause(&s, "plain-keys", "A helper holds `left`, `right` and `middle`.");
    let all = advisories(&s);
    let found: Vec<&String> = all.iter().filter(|l| l.contains("`store.fold.")).collect();
    assert_eq!(found.len(), 1, "two keys, or a type no declaration reads, draw none: {found:?}");
    assert!(found[0].contains("store.fold.shape-keys") && found[0].contains("contextful_demo::shape::Shape"), "{found:?}");
    let count = format!("Clauses restating a code type: {}.", all.len());
    assert!(s.read("spec/status.md").contains(&count), "{count}\n{}", s.read("spec/status.md"));
    assert!(codes(&s.lint("reference"), "SpecRestatesCode").is_empty());

    let s = with_shape();
    add_clause(&s, "shape-keys", "A shape block takes {{type:contextful_demo::shape::Shape}}; `name`, `sort_columns` and `max-rows` are required.");
    let pointed = advisories(&s);
    assert!(!pointed.is_empty(), "the live corpus's own advisories remain");
    assert!(!pointed.iter().any(|l| l.contains("`store.fold.")), "{pointed:?}");
}
