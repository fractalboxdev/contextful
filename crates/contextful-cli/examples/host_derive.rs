//! An embedding binary: the `contextful` command line with one compiled derive task,
//! `word-split`, registered before build. Each unit is a `documents` row; the task lands one
//! `words` row per word, one `stats` row per document, and a marker in `units`.
//! `WORD_SPLIT_VERSION` sets the task's version, `1` by default.

use contextful_core::run::RunError;
use contextful_core::run::derive::task::{DeriveTask, Derived, HostUnit, Tasks};
use contextful_core::run::ports::Row;
use serde_json::json;
use std::sync::Arc;

struct WordSplit {
    version: String,
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
        Ok(out)
    }
}

fn main() {
    let version = std::env::var("WORD_SPLIT_VERSION").unwrap_or_else(|_| "1".into());
    let mut tasks = Tasks::default();
    if let Err(e) = tasks.register("word-split", Arc::new(WordSplit { version })) {
        eprintln!("{e}");
        std::process::exit(1);
    }
    contextful_cli::main_with(tasks);
}
