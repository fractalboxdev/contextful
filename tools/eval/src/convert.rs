//! Converters (`assurance.evaluate.converter`): each external ground-truth source converts
//! once into the case format through its own converter, and the runner reads the case
//! file alone — [`crate::case::load`] refuses a source's own records.

use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;

use crate::case::{self, Case, Expected};
use crate::metrics::RowRef;

/// A ground-truth source the converter could not read: its file, the 1-based line and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertError {
    pub source: &'static str,
    pub line: usize,
    pub reason: String,
}

impl fmt::Display for ConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} line {}: {}", self.source, self.line, self.reason)
    }
}

impl std::error::Error for ConvertError {}

/// Render cases as a case file: one JSON object per line, each line loading back through
/// [`case::load`] to the same case.
pub fn write(cases: &[Case]) -> String {
    cases.iter().map(|c| serde_json::to_string(c).expect("a case serializes") + "\n").collect()
}

/// The BEIR layout: a `queries.jsonl` of `{_id, text}` records and a `qrels` TSV of
/// `query-id`, `corpus-id` and `score` under a header row.
pub mod beir {
    use super::*;

    /// The tag every converted case carries, naming its source.
    pub const TAG: &str = "beir";

    #[derive(Deserialize)]
    struct Query {
        #[serde(rename = "_id")]
        id: String,
        text: String,
    }

    /// Where converted cases point: the corpus directory relative to the case file, and the
    /// table the corpus lands its documents in.
    #[derive(Debug, Clone)]
    pub struct Target<'a> {
        pub corpus: &'a str,
        pub table: &'a str,
    }

    /// Convert `queries` and `qrels` into cases, one per query holding at least one judged
    /// document of positive score, in query-file order. A judged document becomes the
    /// artifact `<table>#<corpus-id>`.
    pub fn convert(queries: &str, qrels: &str, target: &Target<'_>) -> Result<Vec<Case>, ConvertError> {
        let mut relevant: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (i, line) in qrels.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            let err = |reason: String| ConvertError { source: "qrels", line: i + 1, reason };
            let cols: Vec<&str> = line.split('\t').collect();
            let [query, doc, score] = cols.as_slice() else {
                return Err(err(format!("expected 3 tab-separated columns, found {}", cols.len())));
            };
            if i == 0 && score.parse::<f64>().is_err() {
                continue;
            }
            let score: f64 = score.trim().parse().map_err(|_| err(format!("score `{score}` is not a number")))?;
            if score > 0.0 {
                relevant.entry(query.trim().to_string()).or_default().push(case::render(&RowRef::new(target.table, doc.trim())));
            }
        }
        let mut cases = Vec::new();
        for (i, line) in queries.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            let q: Query = serde_json::from_str(line).map_err(|e| ConvertError { source: "queries", line: i + 1, reason: e.to_string() })?;
            let Some(artifacts) = relevant.remove(&q.id) else { continue };
            cases.push(Case {
                id: format!("{TAG}-{}", q.id),
                corpus: target.corpus.to_string(),
                question: q.text,
                tags: vec![TAG.to_string()],
                prefix: None,
                query_embedding: None,
                expected: Expected { artifacts, ..Expected::default() },
            });
        }
        Ok(cases)
    }
}
