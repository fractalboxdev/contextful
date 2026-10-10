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
pub const BM25_K1: f64 = 1.2;
pub const BM25_B: f64 = 0.75;

/// A term's inverse document frequency among `docs` documents, `df` of which hold it.
pub fn bm25_idf(docs: f64, df: f64) -> f64 {
    (1.0 + (docs - df + 0.5) / (df + 0.5)).ln()
}

/// One term's BM25 contribution to a document holding it `tf` times, `norm` being the
/// document's length over the average length.
pub fn bm25_term(idf: f64, tf: f64, norm: f64) -> f64 {
    idf * tf * (BM25_K1 + 1.0) / (tf + BM25_K1 * (1.0 - BM25_B + BM25_B * norm))
}

/// Sealed full-text sidecar bytes past which the file stays unopened: 256 MiB
/// (`read.retrieve.fulltext-sealed-cap`).
pub const FULLTEXT_SEALED_CAP_BYTES: u64 = 256 * 1024 * 1024;

/// Whether a sealed full-text sidecar file of `sealed_bytes` stays unopened, its arm adding
/// no candidates (`read.retrieve.fulltext-sealed-cap`).
pub fn fulltext_sealed_over_cap(sealed_bytes: u64) -> bool {
    sealed_bytes > FULLTEXT_SEALED_CAP_BYTES
}

/// Opened full-text sidecars one read face keeps, oldest evicted first
/// (`read.rank.lexical-index-cache`).
pub const LEXICAL_INDEX_CACHE_ENTRIES: usize = 64;

/// The candidate window for a requested limit: the larger of 8 times it and 200 rows.
pub fn candidate_window(limit: u64) -> u64 {
    limit.saturating_mul(CANDIDATE_WINDOW_FACTOR).max(CANDIDATE_WINDOW_FLOOR)
}

/// Multiple of the limit one sidecar probe requests (`read.retrieve.sidecar-oversampling`).
pub const SIDECAR_PROBE_FACTOR: u64 = 4;

/// Smallest candidate count one sidecar probe requests, in rows (`read.retrieve.sidecar-oversampling`).
pub const SIDECAR_PROBE_FLOOR: u64 = 64;

/// Further multiple a probe takes where the request carries restriction context
/// (`read.retrieve.sidecar-oversampling`).
pub const SIDECAR_RESTRICTED_FACTOR: u64 = 4;

/// Stored-vector bytes past which a sidecar stays unloaded: 64 MiB
/// (`read.retrieve.sidecar-size-cap`).
pub const SIDECAR_SIZE_CAP_BYTES: u64 = 64 * 1024 * 1024;

/// Candidates one sidecar probe requests: the larger of 4 times the limit and 64 rows,
/// times 4 again where the request carries restriction context, since the probe sees none
/// of the restriction that later discards part of its result.
pub fn sidecar_probe_size(limit: u64, restricted: bool) -> u64 {
    let base = limit.saturating_mul(SIDECAR_PROBE_FACTOR).max(SIDECAR_PROBE_FLOOR);
    if restricted {
        base.saturating_mul(SIDECAR_RESTRICTED_FACTOR)
    } else {
        base
    }
}

/// Whether a sidecar holding `stored_vector_bytes` stays unloaded, the arm taking the
/// exact scan (`read.retrieve.sidecar-size-cap`).
pub fn sidecar_over_cap(stored_vector_bytes: u64) -> bool {
    stored_vector_bytes > SIDECAR_SIZE_CAP_BYTES
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
                bm25_idf(n, df)
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
                    .map(|(&f, idf)| bm25_term(*idf, f as f64, norm))
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

/// The constant of reciprocal rank fusion: a document at lexical rank `r`, counting from
/// 1, scores `(K + 1) / (K + r)`, so the best scores 1 (`read.rank.calibration-gate`).
pub const RECIPROCAL_RANK_K: f64 = 60.0;

/// How the lexical leg is calibrated before fusion. Reciprocal rank is the default, the
/// native evaluation scoring it at or above min-max on every tracked measure
/// (`read.rank.fusion`, `read.rank.calibration-gate`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Fusion {
    /// Min-max over the window, the calibration reciprocal rank replaced.
    MinMax,
    /// Reciprocal rank over the window (`read.rank.fusion`).
    #[default]
    ReciprocalRank,
}

impl Fusion {
    /// The fusion named `min-max` or `reciprocal-rank`.
    pub fn parse(name: &str) -> Option<Fusion> {
        match name {
            "min-max" => Some(Fusion::MinMax),
            "reciprocal-rank" => Some(Fusion::ReciprocalRank),
            _ => None,
        }
    }

    /// The calibrated lexical leg of `scores`.
    pub fn calibrate(self, scores: &[Option<f64>]) -> Vec<Option<f64>> {
        match self {
            Fusion::MinMax => min_max(scores),
            Fusion::ReciprocalRank => reciprocal_rank(scores),
        }
    }
}

/// Reciprocal rank over the present documents of a window: `(K + 1) / (K + r)` at
/// descending-score rank `r`, ties sharing their best rank (`read.rank.calibration-gate`).
pub fn reciprocal_rank(scores: &[Option<f64>]) -> Vec<Option<f64>> {
    scores
        .iter()
        .map(|s| {
            s.map(|s| {
                let rank = 1 + scores.iter().flatten().filter(|o| **o > s).count();
                (RECIPROCAL_RANK_K + 1.0) / (RECIPROCAL_RANK_K + rank as f64)
            })
        })
        .collect()
}

/// The lexical leg of a build without the lexical backend: each row's matching-token
/// count over the query's content-token count, every matching token contributing alike;
/// a row matching no token is absent (`read.rank.degradation-not-error`,
/// `read.rank.fallback-counts-tokens`).
pub fn token_fallback(scores: &[Option<u32>], tokens: usize) -> Vec<Option<f64>> {
    scores
        .iter()
        .map(|s| s.filter(|n| *n > 0 && tokens > 0).map(|n| f64::from(n) / tokens as f64))
        .collect()
}

/// The fused score `0.6 · clamp(cosine, 0, 1) + 0.4 · lexical`, `lexical` being the
/// calibrated BM25 leg; a document absent from a leg scores zero there (`read.rank.fusion`).
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

/// Order candidates under the token fallback: the in-window flag leads, then the fused
/// score, recency breaking ties, newest first, then the identifier
/// (`read.rank.degradation-not-error`). An empty ranking falls back to recency order.
pub fn fallback_order(candidates: &mut [Candidate], ranking_empty: bool) {
    candidates.sort_by(|a, b| {
        b.in_window.cmp(&a.in_window).then_with(|| {
            let by_score = if ranking_empty { std::cmp::Ordering::Equal } else { b.fused.partial_cmp(&a.fused).unwrap_or(std::cmp::Ordering::Equal) };
            by_score.then_with(|| b.recency.cmp(&a.recency)).then_with(|| a.id.cmp(&b.id))
        })
    });
}

/// Most probe rounds one sidecar arm runs while visible rows under-fill the limit
/// (`read.retrieve.adaptive-over-fetch`).
pub const OVER_FETCH_ROUNDS: u32 = 4;

/// The probe size of `round`, counting from zero: the first probe's
/// [`sidecar_probe_size`] doubled once per round, saturating
/// (`read.retrieve.adaptive-over-fetch`).
pub fn over_fetch_size(limit: u64, restricted: bool, round: u32) -> u64 {
    let first = sidecar_probe_size(limit, restricted);
    (0..round).fold(first, |size, _| size.saturating_mul(2))
}

/// Whether an arm probes again: the rows the reader can see under-fill the limit, the
/// last probe returned its full size, so the graph holds more, and a round remains
/// (`read.retrieve.adaptive-over-fetch`).
pub fn probe_again(visible: u64, limit: u64, returned: u64, size: u64, round: u32) -> bool {
    visible < limit && returned >= size && round + 1 < OVER_FETCH_ROUNDS
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
    /// A sidecar arm's rounds ran out with the rows the reader can see under the limit
    /// (`read.retrieve.adaptive-over-fetch`); absent otherwise.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub underfilled: bool,
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
