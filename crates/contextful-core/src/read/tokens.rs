//! `read.retrieve`: content tokens of a query, how a token matches text, a row's
//! integer lexical score, and the relevance floor.

/// Shortest ASCII run kept in the content-token set, in chars (`read.retrieve.token-length-floor`).
pub const TOKEN_LENGTH_FLOOR: usize = 2;

/// Largest content-token set from one query (`read.retrieve.token-cap`).
pub const CONTENT_TOKEN_CAP: usize = 12;

/// Token length below which no plural suffix is matched, in chars (`read.retrieve.plural-suffix-floor`).
pub const PLURAL_SUFFIX_FLOOR: usize = 4;

/// Lexical-score floor for a query of at least [`RELEVANCE_FLOOR_SPLIT`] content tokens
/// (`read.retrieve.relevance-floor`).
pub const RELEVANCE_FLOOR_WIDE: u32 = 2;

/// Lexical-score floor for a query below the split (`read.retrieve.relevance-floor`).
pub const RELEVANCE_FLOOR_NARROW: u32 = 1;

/// Content-token count at which the wider floor applies (`read.retrieve.relevance-floor`).
pub const RELEVANCE_FLOOR_SPLIT: usize = 3;

/// Function words carrying no subject. The list is closed and English; a token of
/// another script is never a stop token.
const STOP_TOKENS: [&str; 44] = [
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "did", "do", "does", "for", "from", "had", "has", "have",
    "how", "i", "in", "is", "it", "its", "of", "on", "or", "that", "the", "their", "there", "these", "this", "to",
    "was", "we", "were", "what", "when", "where", "which", "who", "why", "will", "with",
];

/// The content tokens of `query`, in first-occurrence order (`read.retrieve.content-tokens`).
pub fn content_tokens(query: &str) -> Vec<String> {
    let lowered = query.to_lowercase();
    let mut out: Vec<String> = Vec::new();
    for run in lowered.split(|c: char| !c.is_alphanumeric()).filter(|r| !r.is_empty()) {
        let keep = if run.is_ascii() {
            run.len() >= TOKEN_LENGTH_FLOOR && !STOP_TOKENS.contains(&run)
        } else {
            run.chars().count() >= 2
        };
        if keep && !out.iter().any(|t| t == run) {
            out.push(run.to_string());
        }
        if out.len() == CONTENT_TOKEN_CAP {
            break;
        }
    }
    out
}

/// How many times `token` matches `lowered_text`, which the caller lowercased
/// (`read.retrieve.script-split-matching`). An all-lowercase-alphanumeric ASCII token
/// matches on an ASCII word boundary, taking an optional `s` or `es` when at least
/// [`PLURAL_SUFFIX_FLOOR`] chars long; any other token matches by containment.
pub fn matches(token: &str, lowered_text: &str) -> usize {
    if token.is_empty() {
        return 0;
    }
    let word = token.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    if !word {
        return lowered_text.matches(token).count();
    }
    let bytes = lowered_text.as_bytes();
    let boundary = |i: usize| i >= bytes.len() || !bytes[i].is_ascii_alphanumeric();
    let suffixes: &[&str] = if token.len() >= PLURAL_SUFFIX_FLOOR { &["", "s", "es"] } else { &[""] };
    let mut count = 0;
    for (start, _) in lowered_text.match_indices(token) {
        if start > 0 && bytes[start - 1].is_ascii_alphanumeric() {
            continue;
        }
        let end = start + token.len();
        if suffixes.iter().any(|s| lowered_text[end..].starts_with(s) && boundary(end + s.len())) {
            count += 1;
        }
    }
    count
}

/// A row's lexical score: how many content tokens occur in its snippet. It is bounded by
/// the content-token count; a row with no snippet text scores null
/// (`read.retrieve.text-free-table-scores-null`).
pub fn lexical_score(tokens: &[String], snippet: Option<&str>) -> Option<u32> {
    let text = snippet?.to_lowercase();
    Some(tokens.iter().filter(|t| matches(t, &text) > 0).count() as u32)
}

/// The relevance floor for a content-token set: the caller's minimum where given, else
/// the wide floor for [`RELEVANCE_FLOOR_SPLIT`] tokens or more and the narrow one below.
/// An empty set omits the predicate (`read.retrieve.token-cap`).
pub fn relevance_floor(tokens: &[String], caller_minimum: Option<u32>) -> Option<u32> {
    if tokens.is_empty() {
        return None;
    }
    Some(caller_minimum.unwrap_or(if tokens.len() >= RELEVANCE_FLOOR_SPLIT {
        RELEVANCE_FLOOR_WIDE
    } else {
        RELEVANCE_FLOOR_NARROW
    }))
}

/// Whether a row reaches the caller: its lexical score is null or at least the floor, or
/// its vector score is positive (`read.retrieve.relevance-floor`).
pub fn passes_floor(floor: Option<u32>, lexical: Option<u32>, vector: Option<f64>) -> bool {
    let Some(floor) = floor else { return true };
    match lexical {
        None => true,
        Some(score) => score >= floor || vector.is_some_and(|v| v > 0.0),
    }
}
