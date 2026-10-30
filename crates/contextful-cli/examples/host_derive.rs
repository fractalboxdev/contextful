//! An embedding binary: the `contextful` command line with compiled `word-split`,
//! `word-copy` and `doc-join` derive tasks. `word-split` lands `words`, `stats` and `units`
//! from a `documents` row; `word-copy` lands `copies` and `units` from a `words` row;
//! `doc-join` lands `digests` and `units` from a `documents` row and the `notes` and `tags`
//! rows sharing its `doc_id`.
//! `WORD_SPLIT_VERSION` sets the task's version, `1` by default; `WORD_SPLIT_SKIP` names,
//! comma-separated, the content tables the task returns no rows for. `WORD_SPLIT_FAIL`
//! exits the parent process with a failed run outcome in the chained-failure fixture.
//! `WORD_SPLIT_REACH` makes the pure `word-split` ask its reach for a network client, or,
//! set to `infer`, for a model call.
//! `DOC_JOIN_NOTES` names the table `doc-join` reads as `notes`, `notes` by default, and
//! `DOC_JOIN_NOTES_ON` and `DOC_JOIN_NOTE` its join and note columns, `doc_id` and `note` by default.
//! `judge` declares `pure` false and lands `scores` and `units` from a `documents` row, one
//! model call scoring its `body`; `JUDGE_SEND` names an address it requests first.

use contextful_core::connector::infer::{DataItem, Provenance};
use contextful_core::run::derive::task::{DeriveTask, Derived, HostUnit, Join, ModelCall, Reach, Request, Tasks};
use contextful_core::run::ports::Row;
use contextful_core::run::RunError;
use serde_json::json;
use std::sync::Arc;

struct WordSplit {
    version: String,
    skip: Vec<String>,
    fail: bool,
    reach: Option<String>,
}

/// One model call over a unit's body, its value fenced as third-party data.
fn score_call(unit: &HostUnit) -> ModelCall {
    let body = unit.row.get("body").and_then(|v| v.as_str()).unwrap_or_default();
    ModelCall {
        template: "Score the passage from 0 to 1.".into(),
        items: vec![(Provenance::ThirdParty, DataItem::new(format!("documents#{}", unit.key), body))],
        cap: 4000,
        rules: "Answer with one number.".into(),
    }
}

struct Judge {
    send: Option<String>,
}

impl DeriveTask for Judge {
    fn version(&self) -> &str {
        "1"
    }
    fn pure(&self) -> bool {
        false
    }
    fn columns(&self) -> Vec<String> {
        vec!["body".into()]
    }
    fn marker_table(&self) -> String {
        "units".into()
    }
    fn content_tables(&self) -> Vec<String> {
        vec!["scores".into()]
    }

    fn derive(&self, unit: &HostUnit, reach: &Reach<'_>) -> Result<Derived, RunError> {
        if let Some(url) = &self.send {
            reach.client()?.send(&Request { method: "GET".into(), url: url.clone(), ..Request::default() })?;
        }
        let inferred = reach.infer(&score_call(unit))?;
        let mut out = Derived::new();
        out.insert(
            "scores".into(),
            vec![row(json!({ "score": inferred.record.content, "provenance": inferred.taint.as_str() }))],
        );
        Ok(out)
    }
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

    fn derive(&self, unit: &HostUnit, _reach: &Reach<'_>) -> Result<Derived, RunError> {
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

struct DocJoin;

fn notes_table() -> String {
    env_or("DOC_JOIN_NOTES", "notes")
}

fn env_or(var: &str, default: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| default.into())
}

impl DeriveTask for DocJoin {
    fn version(&self) -> &str {
        "1"
    }
    fn columns(&self) -> Vec<String> {
        vec!["body".into()]
    }
    fn joins(&self) -> Vec<Join> {
        vec![
            Join { table: notes_table(), key_column: env_or("DOC_JOIN_NOTES_ON", "doc_id"), columns: vec![env_or("DOC_JOIN_NOTE", "note")] },
            Join { table: "tags".into(), key_column: "doc_id".into(), columns: vec!["tag".into()] },
        ]
    }
    fn marker_table(&self) -> String {
        "units".into()
    }
    fn content_tables(&self) -> Vec<String> {
        vec!["digests".into()]
    }

    fn derive(&self, unit: &HostUnit, _reach: &Reach<'_>) -> Result<Derived, RunError> {
        let texts = |table: &str, column: &str| {
            let mut v: Vec<String> = unit.joined.get(table).into_iter().flatten().filter_map(|r| r.get(column)?.as_str().map(str::to_string)).collect();
            v.sort();
            v.join(",")
        };
        let mut out = Derived::new();
        out.insert(
            "digests".into(),
            vec![row(json!({ "notes": texts(&notes_table(), &env_or("DOC_JOIN_NOTE", "note")), "tags": texts("tags", "tag") }))],
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

    fn derive(&self, unit: &HostUnit, reach: &Reach<'_>) -> Result<Derived, RunError> {
        match self.reach.as_deref() {
            Some("infer") => {
                reach.infer(&score_call(unit))?;
            }
            Some(_) => {
                reach.client()?;
            }
            None => {}
        }
        if self.fail {
            eprintln!("word-split failed for this run");
            std::process::exit(1);
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
    let reach = std::env::var("WORD_SPLIT_REACH").ok();
    let mut tasks = Tasks::default();
    if let Err(e) = tasks.register(
        "word-split",
        Arc::new(WordSplit {
            version,
            skip,
            fail,
            reach,
        }),
    ) {
        eprintln!("{e}");
        std::process::exit(1);
    }
    if let Err(e) = tasks.register("word-copy", Arc::new(WordCopy)) {
        eprintln!("{e}");
        std::process::exit(1);
    }
    if let Err(e) = tasks.register("doc-join", Arc::new(DocJoin)) {
        eprintln!("{e}");
        std::process::exit(1);
    }
    if let Err(e) = tasks.register("judge", Arc::new(Judge { send: std::env::var("JUDGE_SEND").ok() })) {
        eprintln!("{e}");
        std::process::exit(1);
    }
    contextful_cli::main_with(tasks);
}
