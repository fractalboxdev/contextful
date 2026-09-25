//! `read.rank`: the lexical index and its BM25 leg, min-max normalization, fusion with
//! the cosine leg, the question's timeframe, the ordering, and the retrieval block.
//! `read.retrieve.candidate-window` sizes the recency slice the legs score.

use super::tokens::matches;
use crate::time::Instant;
use serde::Serialize;

/// Multiple of the requested limit forming the candidate window (`read.retrieve.candidate-window`).
pub const CANDIDATE_WINDOW_FACTOR: u64 = 8;

/// Smallest candidate window of a ranked read, in rows (`read.retrieve.candidate-window`).
pub const CANDIDATE_WINDOW_FLOOR: u64 = 200;

/// Default weight of the clamped cosine leg in fusion, in percent (`read.rank.fusion`).
pub const FUSION_VECTOR_WEIGHT_PERCENT: u32 = 60;

/// Default weight of the min-max BM25 leg in fusion, in percent (`read.rank.fusion`).
pub const FUSION_LEXICAL_WEIGHT_PERCENT: u32 = 40;

/// How far past the anchor a publication instant still counts in-window, in hours
/// (`read.rank.window-anchor-tolerance`).
pub const WINDOW_ANCHOR_TOLERANCE_HOURS: u64 = 24;

/// BM25 term-frequency saturation and length normalization, at their customary values.
const BM25_K1: f64 = 1.2;
const BM25_B: f64 = 0.75;

/// The candidate window for a requested limit: the larger of 8 times it and 200 rows.
pub fn candidate_window(limit: u64) -> u64 {
    limit.saturating_mul(CANDIDATE_WINDOW_FACTOR).max(CANDIDATE_WINDOW_FLOOR)
}

/// Whether the ranking saw a recency-ordered slice: the candidates fill the window.
pub fn saw_recency_slice(candidates: u64, window: u64) -> bool {
    candidates == window
}

/// One document of a lexical index: its lowercased text and its length in words.
#[derive(Debug, Clone)]
struct Document {
    text: String,
    words: usize,
}

/// A full-text index over one candidate set: each document's text and length, and the
/// average length term statistics normalize against.
#[derive(Debug, Clone)]
pub struct LexicalIndex {
    docs: Vec<Document>,
    average_words: f64,
}

impl LexicalIndex {
    /// Index `docs`; a document with no text indexes as empty and matches nothing.
    pub fn build(docs: &[Option<&str>]) -> LexicalIndex {
        let docs: Vec<Document> = docs
            .iter()
            .map(|d| {
                let text = d.unwrap_or_default().to_lowercase();
                let words = text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).count();
                Document { text, words }
            })
            .collect();
        let total: usize = docs.iter().map(|d| d.words).sum();
        let average_words = if docs.is_empty() { 0.0 } else { total as f64 / docs.len() as f64 };
        LexicalIndex { docs, average_words }
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// The BM25 leg: each document's score over a disjunction of one should-clause per
    /// token, `None` for a document matching no token (`read.rank.lexical-leg-matches-only`).
    pub fn bm25(&self, tokens: &[String]) -> Vec<Option<f64>> {
        let n = self.docs.len() as f64;
        let tf: Vec<Vec<usize>> = self.docs.iter().map(|d| tokens.iter().map(|t| matches(t, &d.text)).collect()).collect();
        let idf: Vec<f64> = (0..tokens.len())
            .map(|i| {
                let df = tf.iter().filter(|row| row[i] > 0).count() as f64;
                (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
            })
            .collect();
        self.docs
            .iter()
            .zip(&tf)
            .map(|(d, row)| {
                if row.iter().all(|&f| f == 0) {
                    return None;
                }
                let norm = if self.average_words > 0.0 { d.words as f64 / self.average_words } else { 0.0 };
                let score = row
                    .iter()
                    .zip(&idf)
                    .map(|(&f, idf)| {
                        let f = f as f64;
                        idf * f * (BM25_K1 + 1.0) / (f + BM25_K1 * (1.0 - BM25_B + BM25_B * norm))
                    })
                    .sum();
                Some(score)
            })
            .collect()
    }
}

/// Min-max normalization over the present documents of a window. A window with no score
/// spread awards every present document full credit (`read.rank.flat-window-full-credit`).
pub fn min_max(scores: &[Option<f64>]) -> Vec<Option<f64>> {
    let present = scores.iter().flatten();
    let lo = present.clone().copied().fold(f64::INFINITY, f64::min);
    let hi = present.copied().fold(f64::NEG_INFINITY, f64::max);
    scores
        .iter()
        .map(|s| s.map(|s| if hi > lo { (s - lo) / (hi - lo) } else { 1.0 }))
        .collect()
}

/// The fused score `0.6 · clamp(cosine, 0, 1) + 0.4 · minmax(bm25)`; a document absent
/// from a leg scores zero there (`read.rank.fusion`).
pub fn fuse(cosine: Option<f64>, lexical: Option<f64>) -> f64 {
    let v = cosine.map_or(0.0, |c| c.clamp(0.0, 1.0));
    let l = lexical.unwrap_or(0.0);
    (f64::from(FUSION_VECTOR_WEIGHT_PERCENT) * v + f64::from(FUSION_LEXICAL_WEIGHT_PERCENT) * l) / 100.0
}

/// A publication value after the cast every comparison runs on
/// (`read.rank.ordering-casts-first`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Publication {
    At(Instant),
    Null,
    Uncastable,
}

impl Publication {
    /// Cast a publication cell's text: an RFC 3339 instant, or a `YYYY-MM-DD` date read
    /// as its UTC midnight. Anything else is uncastable.
    pub fn cast(text: Option<&str>) -> Publication {
        let Some(t) = text.map(str::trim) else { return Publication::Null };
        if let Ok(i) = Instant::parse(t) {
            return Publication::At(i);
        }
        let date = t.len() == 10 && t.as_bytes()[4] == b'-' && t.as_bytes()[7] == b'-';
        match date.then(|| Instant::parse(&format!("{t}T00:00:00Z"))) {
            Some(Ok(i)) => Publication::At(i),
            _ => Publication::Uncastable,
        }
    }

    pub fn instant(self) -> Option<Instant> {
        match self {
            Publication::At(i) => Some(i),
            _ => None,
        }
    }

    /// The basis label: the publication column where the value cast, else which case
    /// applied (`read.rank.window-anchor-tolerance`).
    pub fn basis(self, column: &str) -> String {
        match self {
            Publication::At(_) => column.to_string(),
            Publication::Null => "null".to_string(),
            Publication::Uncastable => "uncastable".to_string(),
        }
    }
}

/// A question's timeframe: an optional lower bound and the anchor it is asked at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeframe {
    pub since: Option<Instant>,
    pub anchor: Instant,
}

impl Timeframe {
    /// The in-window flag: at or after `since`, and no more than 24 h past the anchor. A
    /// null or uncastable value is out of the window (`read.rank.window-anchor-tolerance`).
    pub fn admits(&self, p: Publication) -> bool {
        let Some(at) = p.instant() else { return false };
        let late = self.anchor.plus_secs(WINDOW_ANCHOR_TOLERANCE_HOURS * 3600);
        self.since.is_none_or(|s| at >= s) && at <= late
    }
}

/// One candidate as the ordering sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// The row's identifier, breaking ties.
    pub id: String,
    pub in_window: bool,
    pub fused: f64,
    /// The instant recency order falls back to.
    pub recency: Option<Instant>,
}

/// Order candidates: the in-window flag leads, then the fused score, ties by identifier
/// (`read.rank.question-window-is-a-tier`, `read.rank.fusion`). An empty ranking falls
/// back to recency order, newest first (`read.rank.lexical-leg-matches-only`).
pub fn order(candidates: &mut [Candidate], ranking_empty: bool) {
    candidates.sort_by(|a, b| {
        b.in_window.cmp(&a.in_window).then_with(|| {
            let by_score = if ranking_empty {
                b.recency.cmp(&a.recency)
            } else {
                b.fused.partial_cmp(&a.fused).unwrap_or(std::cmp::Ordering::Equal)
            };
            by_score.then_with(|| a.id.cmp(&b.id))
        })
    });
}

/// The `contextful.retrieval` block (`read.rank.retrieval-block`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RetrievalBlock {
    pub window: u64,
    pub candidates_prefloor: u64,
    pub candidates: u64,
    pub matched: u64,
    pub returned: u64,
    pub in_window: u64,
    pub deduped: u64,
    pub padded: u64,
    pub floor: Option<u32>,
    pub since: Option<String>,
}

/// The ranking fields one row carries: an integer score bounded by the content-token
/// count, the cosine where a vector leg ran, the in-window flag and the basis label. The
/// lexical engine's own float score has no field here
/// (`read.rank.internal-score-stays-internal`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RowRanking {
    #[serde(rename = "_score")]
    pub score: Option<u32>,
    #[serde(rename = "_vscore")]
    pub vscore: Option<f64>,
    #[serde(rename = "_in_window")]
    pub in_window: bool,
    #[serde(rename = "_date_basis")]
    pub date_basis: String,
}
