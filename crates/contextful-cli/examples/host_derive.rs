//! An embedding binary: the `contextful` command line with two compiled derive tasks
//! registered before build. `word-split` takes a `documents` row as its unit and lands one
//! `words` row per word, one `stats` row per document, and a marker in `units`.
//! `WORD_SPLIT_VERSION` sets its version, `1` by default; `WORD_SPLIT_SKIP` names,
//! comma-separated, the content tables it returns no rows for; `WORD_SPLIT_FAIL` set ends
//! the process at its first unit, failing the fire. `count-tag` takes a row carrying
//! `word_count`, such as a `stats` row, and lands one `tags` row and a marker in `tag_units`.

use contextful_core::run::RunError;
use contextful_core::run::derive::task::{DeriveTask, Derived, HostUnit, Tasks};
use contextful_core::run::ports::Row;
use serde_json::json;
use std::sync::Arc;

struct WordSplit {
    version: String,
    skip: Vec<String>,
    fail: bool,
}

struct CountTag;

impl DeriveTask for CountTag {
    fn version(&self) -> &str {
        "1"
    }

    fn columns(&self) -> Vec<String> {
        vec!["word_count".into()]
    }

    fn marker_table(&self) -> String {
        "tag_units".into()
    }

    fn content_tables(&self) -> Vec<String> {
        vec!["tags".into()]
    }

    fn derive(&self, unit: &HostUnit) -> Result<Derived, RunError> {
        let mut out = Derived::new();
        if let Some(n) = unit.row.get("word_count").and_then(|v| v.as_i64()) {
            let tag = if n > 1 { "many" } else { "one" };
            out.insert("tags".into(), vec![row(json!({ "tag": tag }))]);
        }
        Ok(out)
    }
}

fn row(v: serde_json::Value) -> Row {
    v.as_object().cloned().unwrap_or_default()
}

impl DeriveTask for WordSplit {
    fn version(&self) -> &str {
        &self.version
    }

    fn columns(&self) -> Vec<String> {
        vec!["body".into()]
    }

    fn marker_table(&self) -> String {
        "units".into()
    }

    fn content_tables(&self) -> Vec<String> {
        vec!["words".into(), "stats".into()]
    }

    fn derive(&self, unit: &HostUnit) -> Result<Derived, RunError> {
        if self.fail {
            eprintln!("unit `{}`: `WORD_SPLIT_FAIL` ends the fire", unit.key);
            std::process::exit(3);
        }
        let body = unit
            .row
            .get("body")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RunError::Invalid(format!("unit `{}` holds no body", unit.key)))?;
        let words: Vec<&str> = body.split_whitespace().collect();
        let mut out = Derived::new();
        if words.is_empty() {
            return Ok(out);
        }
        out.insert(
            "words".into(),
            words
                .iter()
                .enumerate()
                .map(|(i, w)| {
                    row(json!({ "word_seq": i as i64, "word": w, "version_label": self.version }))
                })
                .collect(),
        );
        out.insert(
            "stats".into(),
            vec![row(json!({ "word_count": words.len() as i64 }))],
        );
        out.retain(|table, _| !self.skip.contains(table));
        Ok(out)
    }
}

fn main() {
    let version = std::env::var("WORD_SPLIT_VERSION").unwrap_or_else(|_| "1".into());
    let skip = std::env::var("WORD_SPLIT_SKIP")
        .unwrap_or_default()
        .split(',')
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();
    let fail = std::env::var_os("WORD_SPLIT_FAIL").is_some();
    let mut tasks = Tasks::default();
    let registered = tasks
        .register("word-split", Arc::new(WordSplit { version, skip, fail }))
        .and_then(|()| tasks.register("count-tag", Arc::new(CountTag)));
    if let Err(e) = registered {
        eprintln!("{e}");
        std::process::exit(1);
    }
    contextful_cli::main_with(tasks);
}
