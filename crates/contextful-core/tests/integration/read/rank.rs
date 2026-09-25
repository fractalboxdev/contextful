//! `read.rank`: the BM25 leg, min-max, fusion, the question's timeframe and the ordering.

use super::at;
use contextful_core::read::embed::cosine;
use contextful_core::read::rank::{
    fuse, min_max, order, Candidate, LexicalIndex, Publication, RetrievalBlock, RowRanking,
    Timeframe, FUSION_LEXICAL_WEIGHT_PERCENT, FUSION_VECTOR_WEIGHT_PERCENT,
    WINDOW_ANCHOR_TOLERANCE_HOURS,
};
use contextful_core::read::tokens::content_tokens;
use serde_json::json;

fn candidate(id: &str, in_window: bool, fused: f64, recency: &str) -> Candidate {
    Candidate { id: id.into(), in_window, fused, recency: Some(at(recency)) }
}

fn ids(c: &[Candidate]) -> Vec<&str> {
    c.iter().map(|c| c.id.as_str()).collect()
}

/// Ranking runs exact cosine similarity over the candidate set, BM25 over a full-text index, and a weighted fusion of the two.
// spec: read.rank.three-legs@9254e7ed
#[test]
fn ranking_fuses_a_cosine_leg_and_a_bm25_leg() {
    let docs = [Some("solar battery storage costs fall"), Some("battery storage for regional grids"), Some("quarterly hiring plan")];
    let bm25 = LexicalIndex::build(&docs).bm25(&content_tokens("solar battery storage"));
    assert!(bm25[0].unwrap() > bm25[1].unwrap());
    assert_eq!(bm25[2], None);
    let lexical = min_max(&bm25);
    let query = [1.0f32, 0.0];
    let vectors = [[0.0f32, 1.0], [1.0, 0.0], [1.0, 0.0]];
    let cos: Vec<Option<f64>> = vectors.iter().map(|v| cosine(&query, v)).collect();
    let fused: Vec<f64> = cos.iter().zip(&lexical).map(|(c, l)| fuse(*c, *l)).collect();
    // The second document leads on the vector leg the first lacks.
    assert!(fused[1] > fused[0], "{fused:?}");
    assert!((fused[2] - 0.6).abs() < 1e-9);
}

/// Fusion computes `w_vec · clamp(cosine, 0, 1) + w_lex · minmax(bm25)` with default weights 60 percent and 40 percent. A document absent from one leg scores zero there; ties break by identifier.
// spec: read.rank.fusion@c26bf771
#[test]
fn fusion_weights_clamps_and_breaks_ties_by_identifier() {
    assert_eq!((FUSION_VECTOR_WEIGHT_PERCENT, FUSION_LEXICAL_WEIGHT_PERCENT), (60, 40));
    assert!((fuse(Some(0.5), Some(1.0)) - 0.7).abs() < 1e-9);
    assert!((fuse(Some(-0.4), Some(1.0)) - 0.4).abs() < 1e-9);
    assert!((fuse(Some(1.7), None) - 0.6).abs() < 1e-9);
    assert_eq!(fuse(None, None), 0.0);
    let mut c = vec![candidate("b", true, 0.5, "2030-01-01T00:00:00Z"), candidate("a", true, 0.5, "2029-01-01T00:00:00Z")];
    order(&mut c, false);
    assert_eq!(ids(&c), ["a", "b"]);
}

/// The BM25 leg ranks a disjunction of should-clauses; a document matching no term is absent. An empty query, candidate set or result yields an empty ranking that falls back to recency order.
// spec: read.rank.lexical-leg-matches-only@34426892
#[test]
fn the_lexical_leg_holds_matching_documents_alone() {
    let index = LexicalIndex::build(&[Some("solar prices"), Some("storage plans"), Some("hiring")]);
    let scores = index.bm25(&content_tokens("solar storage"));
    assert!(scores[0].is_some() && scores[1].is_some());
    assert_eq!(scores[2], None);
    assert!(index.bm25(&[]).iter().all(Option::is_none));
    assert!(LexicalIndex::build(&[]).bm25(&content_tokens("solar")).is_empty());
    let mut c = vec![
        candidate("old", true, 0.0, "2030-01-01T00:00:00Z"),
        candidate("new", true, 0.0, "2030-03-01T00:00:00Z"),
    ];
    order(&mut c, true);
    assert_eq!(ids(&c), ["new", "old"]);
}

/// A lexical window with no score spread awards every present document full credit.
// spec: read.rank.flat-window-full-credit@9df2f4c8
#[test]
fn a_flat_window_awards_full_credit() {
    assert_eq!(min_max(&[Some(2.5), None, Some(2.5)]), [Some(1.0), None, Some(1.0)]);
    assert_eq!(min_max(&[Some(1.0), Some(3.0), Some(2.0)]), [Some(0.0), Some(1.0), Some(0.5)]);
}

/// A question's timeframe projects a per-row in-window flag against the resolved publication column, and the flag leads the ordering. An out-of-window row sorts down and stays.
// spec: read.rank.question-window-is-a-tier@49b15c19
#[test]
fn the_in_window_flag_leads_and_an_out_of_window_row_stays() {
    let tf = Timeframe { since: Some(at("2030-01-01T00:00:00Z")), anchor: at("2030-02-01T00:00:00Z") };
    assert!(tf.admits(Publication::cast(Some("2030-01-15"))));
    assert!(!tf.admits(Publication::cast(Some("2029-12-31T23:00:00Z"))));
    let mut c = vec![candidate("strong-old", false, 0.9, "2029-06-01T00:00:00Z"), candidate("weak-new", true, 0.1, "2030-01-15T00:00:00Z")];
    order(&mut c, false);
    assert_eq!(ids(&c), ["weak-new", "strong-old"]);
}

/// A publication instant more than 24 h past the question's anchor sets the in-window flag false. A null or uncastable value sets it false too, and a basis field names which case applied.
// spec: read.rank.window-anchor-tolerance@1acc354f
#[test]
fn the_anchor_tolerates_24_hours_and_names_the_basis() {
    assert_eq!(WINDOW_ANCHOR_TOLERANCE_HOURS, 24);
    let tf = Timeframe { since: None, anchor: at("2030-02-01T00:00:00Z") };
    assert!(tf.admits(Publication::cast(Some("2030-02-01T23:59:59Z"))));
    assert!(!tf.admits(Publication::cast(Some("2030-02-02T00:00:01Z"))));
    let null = Publication::cast(None);
    let bad = Publication::cast(Some("last spring"));
    assert!(!tf.admits(null) && !tf.admits(bad));
    assert_eq!(null.basis("published_at"), "null");
    assert_eq!(bad.basis("published_at"), "uncastable");
    assert_eq!(Publication::cast(Some("2030-01-01")).basis("published_at"), "published_at");
}

/// Ordering compares publication values as `TIMESTAMPTZ` instants, casting a text value before any comparison.
// spec: read.rank.ordering-casts-first@923d257a
#[test]
fn publication_text_casts_to_an_instant_before_comparison() {
    // As strings the first sorts earlier; as instants it is three hours later.
    let (a, b) = ("2030-01-09T23:00:00-05:00", "2030-01-10T01:00:00Z");
    assert!(a < b);
    let (pa, pb) = (Publication::cast(Some(a)).instant().unwrap(), Publication::cast(Some(b)).instant().unwrap());
    assert!(pa > pb);
    let mut c = vec![
        Candidate { id: "b".into(), in_window: true, fused: 0.0, recency: Some(pb) },
        Candidate { id: "a".into(), in_window: true, fused: 0.0, recency: Some(pa) },
    ];
    order(&mut c, true);
    assert_eq!(ids(&c), ["a", "b"]);
}

/// The `contextful.retrieval` block reports window, candidates_prefloor, candidates, matched, returned, in_window, deduped, padded, floor and since. Each row carries an integer score bounded by the content-token count, the in-window flag and the basis label.
// spec: read.rank.retrieval-block@a1cf3e6e
#[test]
fn the_retrieval_block_and_row_fields_carry_their_names() {
    let block = RetrievalBlock { window: 200, candidates_prefloor: 200, candidates: 61, matched: 61, returned: 20, in_window: 14, deduped: 3, padded: 0, floor: Some(2), since: Some("2026-02-07".into()) };
    assert_eq!(
        serde_json::to_value(&block).unwrap(),
        json!({ "window": 200, "candidates_prefloor": 200, "candidates": 61, "matched": 61, "returned": 20, "in_window": 14, "deduped": 3, "padded": 0, "floor": 2, "since": "2026-02-07" }),
    );
    let row = RowRanking { score: Some(2), vscore: Some(0.71), in_window: true, date_basis: "published_at".into() };
    assert_eq!(
        serde_json::to_value(&row).unwrap(),
        json!({ "_score": 2, "_vscore": 0.71, "_in_window": true, "_date_basis": "published_at" }),
    );
}
