//! An embedding binary: the `contextful` command line with compiled `word-split` and
//! `word-copy` derive tasks. `word-split` lands `words`, `stats` and `units` from a
//! `documents` row; `word-copy` lands `copies` and `units` from a `words` row.
//! `WORD_SPLIT_VERSION` sets the task's version, `1` by default; `WORD_SPLIT_SKIP` names,
//! comma-separated, the content tables the task returns no rows for. `WORD_SPLIT_FAIL`
//! makes the parent task panic in the chained-failure fixture.

use contextful_core::run::derive::task::{DeriveTask, Derived, HostUnit, Tasks};
use contextful_core::run::ports::Row;
use contextful_core::run::RunError;
use serde_json::json;
use std::sync::Arc;

struct WordSplit {
    version: String,
    skip: Vec<String>,
    fail: bool,
}

struct WordCopy;

impl DeriveTask for WordCopy {
    fn version(&self) -> &str {
        "1"
    }
    fn columns(&self) -> Vec<String> {
        vec!["word".into()]
    }
    fn marker_table(&self) -> String {
        "units".into()
    }
    fn content_tables(&self) -> Vec<String> {
        vec!["copies".into()]
    }

    fn derive(&self, unit: &HostUnit) -> Result<Derived, RunError> {
        let word = unit
            .row
            .get("word")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RunError::Invalid(format!("unit `{}` holds no word", unit.key)))?;
        let mut out = Derived::new();
        out.insert(
            "copies".into(),
            vec![row(json!({ "cue_seq": 0, "copy": word }))],
        );
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
            panic!("word-split failed for this run");
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
    if let Err(e) = tasks.register(
        "word-split",
        Arc::new(WordSplit {
            version,
            skip,
            fail,
        }),
    ) {
        eprintln!("{e}");
        std::process::exit(1);
    }
    if let Err(e) = tasks.register("word-copy", Arc::new(WordCopy)) {
        eprintln!("{e}");
        std::process::exit(1);
    }
    contextful_cli::main_with(tasks);
}
