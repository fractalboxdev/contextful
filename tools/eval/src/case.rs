//! The case format (`assurance.evaluate.case-format`) and its one loader
//! (`assurance.baseline.golden-custody`): a JSON Lines file, one case per line, each naming
//! its corpus by a path relative to the file and its rows as `<table>#<key>`
//! (`assurance.evaluate.row-reference`).

use std::collections::{BTreeSet, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::metrics::RowRef;

/// The tag marking a regression case (`assurance.evaluate.regression-case`).
pub const REGRESSION_TAG: &str = "regression";

/// The separator between a row reference's table and key.
pub const ROW_SEPARATOR: char = '#';

/// The separator joining a composite primary key's values.
pub const KEY_SEPARATOR: &str = ",";

/// One evaluation case.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    /// The corpus directory, relative to the case file's directory.
    pub corpus: String,
    pub question: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// The table prefix the ranked call reads under; absent reads every table.
    #[serde(default)]
    pub prefix: Option<String>,
    /// The question's own embedding; absent, the stub embedder embeds the question.
    #[serde(default)]
    pub query_embedding: Option<Vec<f32>>,
    #[serde(default)]
    pub expected: Expected,
}

/// A case's truth and the constraints its return is held to.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expected {
    /// The reference answer the judged tier reads.
    #[serde(default)]
    pub answer: Option<String>,
    /// Rows the artifact retriever ranks, as `<table>#<key>`.
    #[serde(default)]
    pub artifacts: Vec<String>,
    /// Edges the edge retriever ranks.
    #[serde(default)]
    pub edges: Vec<String>,
    /// Entities the answer resolves to.
    #[serde(default)]
    pub entities: Vec<String>,
    /// Rows the answer cites.
    #[serde(default)]
    pub must_cite: Vec<String>,
    /// The case answers by refusing.
    #[serde(default)]
    pub must_abstain: bool,
    /// Rows no leg returns, as `<table>#<key>`.
    #[serde(default)]
    pub must_not_retrieve: Vec<String>,
    /// The instant the question is asked at (RFC 3339); absent, the corpus's landing instant.
    #[serde(default)]
    pub time_anchor: Option<String>,
    /// The question's lower bound on publication (RFC 3339); present, the case declares a
    /// recency bound.
    #[serde(default)]
    pub since: Option<String>,
}

impl Case {
    /// The corpus directory, resolved against the directory holding the case file.
    pub fn corpus_dir(&self, case_file: &Path) -> PathBuf {
        case_file.parent().unwrap_or(Path::new(".")).join(&self.corpus)
    }

    /// The relevant set of the artifact retriever.
    pub fn relevant(&self) -> HashSet<RowRef> {
        self.expected.artifacts.iter().filter_map(|r| row_ref(r)).collect()
    }

    /// The must-not-retrieve set.
    pub fn must_not(&self) -> HashSet<RowRef> {
        self.expected.must_not_retrieve.iter().filter_map(|r| row_ref(r)).collect()
    }

    pub fn regression(&self) -> bool {
        self.tags.iter().any(|t| t == REGRESSION_TAG)
    }

    pub fn recency_bound(&self) -> bool {
        self.expected.since.is_some()
    }
}

/// Parse `<table>#<key>`: the key follows the last `#`, and neither side is empty.
pub fn row_ref(s: &str) -> Option<RowRef> {
    let (table, key) = s.rsplit_once(ROW_SEPARATOR)?;
    (!table.is_empty() && !key.is_empty()).then(|| RowRef::new(table, key))
}

/// A row reference as a case writes it.
pub fn render(r: &RowRef) -> String {
    format!("{}{ROW_SEPARATOR}{}", r.table, r.key)
}

/// A case file that does not load: the 1-based line and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseError {
    pub line: usize,
    pub reason: String,
}

impl fmt::Display for CaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.reason)
    }
}

impl std::error::Error for CaseError {}

/// Load a case file: one case per non-blank line. A line that does not parse, a repeated
/// id, an empty question and a malformed row reference each refuse the whole file.
pub fn load(text: &str) -> Result<Vec<Case>, CaseError> {
    let mut cases = Vec::new();
    let mut ids = BTreeSet::new();
    for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let err = |reason: String| CaseError { line: i + 1, reason };
        let case: Case = serde_json::from_str(line).map_err(|e| err(e.to_string()))?;
        if case.id.is_empty() {
            return Err(err("the case carries an empty id".into()));
        }
        if !ids.insert(case.id.clone()) {
            return Err(err(format!("the id `{}` repeats an earlier case", case.id)));
        }
        if case.question.trim().is_empty() && case.query_embedding.is_none() {
            return Err(err(format!("`{}` asks no question", case.id)));
        }
        let e = &case.expected;
        if let Some(bad) = e.artifacts.iter().chain(&e.must_not_retrieve).find(|r| row_ref(r).is_none()) {
            return Err(err(format!("`{}` names the row `{bad}`; a row is `<table>#<key>`", case.id)));
        }
        cases.push(case);
    }
    if cases.is_empty() {
        return Err(CaseError { line: 0, reason: "the file holds no case".into() });
    }
    Ok(cases)
}
