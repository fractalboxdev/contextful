//! `read.retrieve`: content tokens, matching, the relevance floor and the candidate window.

use super::strings;
use contextful_core::read::rank::{candidate_window, saw_recency_slice, LexicalIndex, CANDIDATE_WINDOW_FACTOR, CANDIDATE_WINDOW_FLOOR};
use contextful_core::read::tokens::{
    content_tokens, lexical_score, matches, passes_floor, relevance_floor, CONTENT_TOKEN_CAP, PLURAL_SUFFIX_FLOOR,
    RELEVANCE_FLOOR_NARROW, RELEVANCE_FLOOR_SPLIT, RELEVANCE_FLOOR_WIDE, TOKEN_LENGTH_FLOOR,
};

/// The query text is lowercased and split on non-alphanumeric characters. Stop tokens drop, non-ASCII runs of two or more characters stay, and survivors deduplicate in order.
// spec: read.retrieve.content-tokens@94355737
#[test]
fn content_tokens_are_lowercased_split_stopped_and_deduplicated() {
    assert_eq!(content_tokens("What did Solar-Battery STORAGE cost, and the solar?"), strings(&["solar", "battery", "storage", "cost"]));
    assert_eq!(content_tokens("太陽能 電池 x 電"), strings(&["太陽能", "電池"]));
}

/// An ASCII run shorter than 2 chars leaves the content-token set.
// spec: read.retrieve.token-length-floor@cdcd94ce
#[test]
fn an_ascii_run_below_the_length_floor_leaves() {
    assert_eq!(TOKEN_LENGTH_FLOOR, 2);
    assert_eq!(content_tokens("q 3 ev x2"), strings(&["ev", "x2"]));
}

/// The content-token set holds at most 12 tokens. An empty set omits the relevance predicate.
// spec: read.retrieve.token-cap@f65c8490
#[test]
fn the_token_set_is_capped_and_an_empty_set_omits_the_floor() {
    assert_eq!(CONTENT_TOKEN_CAP, 12);
    let long: Vec<String> = (0..20).map(|i| format!("term{i}")).collect();
    let tokens = content_tokens(&long.join(" "));
    assert_eq!(tokens, long[..12].to_vec());
    let none = content_tokens("the of and");
    assert!(none.is_empty());
    assert_eq!(relevance_floor(&none, None), None);
    assert!(passes_floor(None, Some(0), None));
}

/// An all-lowercase-alphanumeric token matches on an ASCII word boundary with an optional plural suffix; a token with any non-ASCII character matches by containment.
// spec: read.retrieve.script-split-matching@a10fb581
#[test]
fn ascii_tokens_match_on_word_boundaries_and_others_by_containment() {
    assert_eq!(matches("battery", "solar battery storage"), 1);
    assert_eq!(matches("battery", "batterybank"), 0);
    assert_eq!(matches("grid", "regional grids"), 1);
    assert_eq!(matches("grid", "gridlock"), 0);
    assert_eq!(matches("電池", "太陽能電池の価格"), 1);
}

/// A token under 4 chars takes no optional plural suffix.
// spec: read.retrieve.plural-suffix-floor@6eb3947f
#[test]
fn a_short_token_takes_no_plural_suffix() {
    assert_eq!(PLURAL_SUFFIX_FLOOR, 4);
    assert_eq!(matches("ev", "evs are cheaper"), 0);
    assert_eq!(matches("ev", "an ev is cheaper"), 1);
    assert_eq!(matches("cell", "cells"), 1);
}

/// A row reaches a caller when its lexical score is null, at least the floor, or its vector score is positive. The floor is 2 tokens for queries of 3 tokens or more, else 1 tokens; a caller minimum overrides.
// spec: read.retrieve.relevance-floor@a1e3c467
#[test]
fn the_relevance_floor_admits_null_scores_and_positive_vectors() {
    assert_eq!((RELEVANCE_FLOOR_WIDE, RELEVANCE_FLOOR_NARROW, RELEVANCE_FLOOR_SPLIT), (2, 1, 3));
    let three = content_tokens("solar battery storage");
    let two = content_tokens("solar battery");
    assert_eq!(relevance_floor(&three, None), Some(2));
    assert_eq!(relevance_floor(&two, None), Some(1));
    assert_eq!(relevance_floor(&three, Some(3)), Some(3));
    let floor = relevance_floor(&three, None);
    assert!(passes_floor(floor, Some(2), None));
    assert!(!passes_floor(floor, Some(1), None));
    assert!(passes_floor(floor, Some(1), Some(0.2)));
    assert!(!passes_floor(floor, Some(1), Some(0.0)));
    assert!(passes_floor(floor, None, None));
}

/// A table with no snippet-worthy text column scores lexically null, never zero, and its rows enter by vector score alone.
// spec: read.retrieve.text-free-table-scores-null@6fd1ad30
#[test]
fn a_row_with_no_snippet_scores_null_and_passes_the_floor() {
    let tokens = content_tokens("solar battery storage");
    assert_eq!(lexical_score(&tokens, None), None);
    assert_eq!(lexical_score(&tokens, Some("quarterly plan")), Some(0));
    assert!(passes_floor(relevance_floor(&tokens, None), lexical_score(&tokens, None), None));
}

/// The candidate window is 8 times the requested limit or 200 rows, whichever is larger. Candidates equal to the window report that the ranking saw a recency-ordered slice.
// spec: read.retrieve.candidate-window@74ab93d6
#[test]
fn the_candidate_window_is_the_larger_of_a_multiple_and_a_floor() {
    assert_eq!((CANDIDATE_WINDOW_FACTOR, CANDIDATE_WINDOW_FLOOR), (8, 200));
    assert_eq!(candidate_window(10), 200);
    assert_eq!(candidate_window(50), 400);
    assert!(saw_recency_slice(200, 200));
    assert!(!saw_recency_slice(61, 200));
}

/// Whether a row reaches a lexical-only read: it scores in the BM25 leg and passes the relevance floor.
fn reaches(tokens: &[String], index: &LexicalIndex, i: usize, text: &str) -> bool {
    index.bm25(tokens)[i].is_some() && passes_floor(relevance_floor(tokens, None), lexical_score(tokens, Some(text)), None)
}

#[test]
fn a_substring_trap_row_scores_zero_and_never_returns() {
    // (query, relevant row, a row holding every token only inside longer words)
    let cases = [
        ("grid", "regional grids report", "gridlock downtown"),
        ("solar battery storage", "solar battery storage costs", "solarpanel batterybank storageunit"),
        ("the battery of the grid", "battery for the grid", "batterybank of the gridlock"),
    ];
    let mut forbidden = 0u64;
    for (query, relevant, trap) in cases {
        let tokens = content_tokens(query);
        let index = LexicalIndex::build(&[Some(relevant), Some(trap)]);
        assert!(reaches(&tokens, &index, 0, relevant), "{query}: the relevant row reaches the read");
        assert_eq!(lexical_score(&tokens, Some(trap)), Some(0), "{query}");
        forbidden += u64::from(reaches(&tokens, &index, 1, trap));
    }
    crate::emit("word-boundary-precision", forbidden as f64, cases.len() as u64, 0);
    assert_eq!(forbidden, 0);
}

#[test]
fn a_cjk_token_matches_inside_its_run_in_the_score_and_the_bm25_leg() {
    let docs = ["設計の変更について", "太陽能電池の価格", "battery prices fell"];
    let tokens = content_tokens("電池");
    assert_eq!(tokens, strings(&["電池"]));
    let index = LexicalIndex::build(&docs.map(Some));
    let scores = index.bm25(&tokens);
    assert_eq!(scores.iter().map(Option::is_some).collect::<Vec<_>>(), [false, true, false], "{scores:?}");
    assert_eq!(docs.map(|d| lexical_score(&tokens, Some(d))), [Some(0), Some(1), Some(0)]);
    let mut ranked: Vec<usize> = (0..docs.len()).filter(|i| scores[*i].is_some()).collect();
    ranked.sort_by(|a, b| scores[*b].partial_cmp(&scores[*a]).unwrap());
    let reciprocal_rank = ranked.iter().position(|i| *i == 1).map_or(0.0, |p| 1.0 / (p + 1) as f64);
    crate::emit("cjk-subrun", reciprocal_rank, 1, 0);
    assert_eq!(reciprocal_rank, 1.0);
}
