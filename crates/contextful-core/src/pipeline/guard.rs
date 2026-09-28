//! `run.guard-secrets`: the write-time mask over credential-shaped spans in a pulled
//! batch. Every matcher is a single forward scan with no regular expression.

use crate::run::ports::Row;
use serde_json::Value;
use std::collections::BTreeMap;
use std::ops::Range;

/// The fixed replacement for a masked span (`run.guard-secrets.mask-replacement`).
pub const MARKER: &str = "[REDACTED:secret]";

/// A credential shape, in priority order: an earlier kind wins an overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    PemPrivateKey,
    /// OpenAI and Anthropic keys: `sk-`, `sk-proj-`, `sk-ant-`.
    LlmProviderKey,
    GoogleApiKey,
    /// Stripe secret and restricted keys: `sk_live_`, `rk_live_`.
    StripeKey,
    Jwt,
    AwsAccessKeyId,
    GithubToken,
    SlackToken,
    /// The token of an `Authorization: Bearer <token>` header; the scheme stays.
    Bearer,
    Assignment,
}

/// Keywords whose assigned value is masked, keeping the `key=` prefix.
pub const KEYWORDS: [&str; 8] = ["password", "passwd", "secret", "token", "api_key", "apikey", "access_key", "auth"];

fn upper_alnum(b: u8) -> bool {
    b.is_ascii_uppercase() || b.is_ascii_digit()
}

fn find_all<'a>(hay: &'a str, needle: &'a str) -> impl Iterator<Item = usize> + 'a {
    hay.match_indices(needle).map(|(i, _)| i)
}

fn run_len(bytes: &[u8], from: usize, ok: impl Fn(u8) -> bool) -> usize {
    bytes[from.min(bytes.len())..].iter().take_while(|b| ok(**b)).count()
}

fn base64url(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

/// `s[i..]` starts a word: `-` and `_` count as word characters, so `my-sk-learn-...` starts none.
fn word_start(b: &[u8], i: usize) -> bool {
    i == 0 || !base64url(b[i - 1])
}

fn llm_provider(s: &str, out: &mut Vec<(Kind, Range<usize>)>) {
    let b = s.as_bytes();
    let mut floor = 0;
    for i in find_all(s, "sk-") {
        if i < floor || !word_start(b, i) {
            continue;
        }
        let n = run_len(b, i + 3, base64url);
        floor = i + 3 + n;
        // Real keys run 48 to 164 characters; 32 clears hyphenated slugs such as `sk-learn-...`.
        if n >= 32 {
            out.push((Kind::LlmProviderKey, i..i + 3 + n));
        }
    }
}

fn google(s: &str, out: &mut Vec<(Kind, Range<usize>)>) {
    let b = s.as_bytes();
    for i in find_all(s, "AIza") {
        if !word_start(b, i) {
            continue;
        }
        // At most 36 bytes are read: the 35 of the key and one proving it ends.
        let tail = b[(i + 4).min(b.len())..].iter().take(36).take_while(|c| base64url(**c)).count();
        if tail == 35 {
            out.push((Kind::GoogleApiKey, i..i + 39));
        }
    }
}

fn stripe(s: &str, out: &mut Vec<(Kind, Range<usize>)>) {
    let b = s.as_bytes();
    for prefix in ["sk_live_", "rk_live_"] {
        let mut floor = 0;
        for i in find_all(s, prefix) {
            if i < floor || !word_start(b, i) {
                continue;
            }
            let n = run_len(b, i + 8, |c| c.is_ascii_alphanumeric());
            floor = i + 8 + n;
            if n >= 24 {
                out.push((Kind::StripeKey, i..i + 8 + n));
            }
        }
    }
}

/// Three base64url segments joined by `.`, the header and payload each opening `eyJ` (`{"`).
fn jwt(s: &str, out: &mut Vec<(Kind, Range<usize>)>) {
    let b = s.as_bytes();
    let mut floor = 0;
    for i in find_all(s, "eyJ") {
        // A start after `.` sits inside a dotted run, so no token begins there.
        if i < floor || !word_start(b, i) || (i > 0 && b[i - 1] == b'.') {
            continue;
        }
        let header_end = i + run_len(b, i, base64url);
        floor = header_end;
        let payload = header_end + 1;
        if header_end >= b.len() || b[header_end] != b'.' || !b[payload..].starts_with(b"eyJ") {
            continue;
        }
        let payload_end = payload + run_len(b, payload, base64url);
        floor = payload_end;
        if payload_end >= b.len() || b[payload_end] != b'.' {
            continue;
        }
        let end = payload_end + 1 + run_len(b, payload_end + 1, base64url);
        floor = end;
        if end > payload_end + 1 {
            out.push((Kind::Jwt, i..end));
        }
    }
}

/// `Bearer <token68>`, case-insensitive on the scheme, over the lowercase copy the assignment matcher shares.
fn bearer(s: &str, lower: &str, out: &mut Vec<(Kind, Range<usize>)>) {
    let b = s.as_bytes();
    let mut floor = 0;
    for i in find_all(lower, "bearer ") {
        if i < floor || (i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')) {
            continue;
        }
        let j = i + 7 + run_len(b, i + 7, |c| c == b' ');
        let n = run_len(b, j, |c| c.is_ascii_alphanumeric() || b"-._~+/".contains(&c));
        let end = j + n + run_len(b, j + n, |c| c == b'=');
        // A token under 20 bytes is reread at most once by the next candidate, so the scan stays linear.
        floor = if n >= 20 { end } else { j };
        if n >= 20 {
            out.push((Kind::Bearer, j..end));
        }
    }
}

fn aws(s: &str, out: &mut Vec<(Kind, Range<usize>)>) {
    let b = s.as_bytes();
    for prefix in ["AKIA", "ASIA"] {
        for i in find_all(s, prefix) {
            let before_ok = i == 0 || !upper_alnum(b[i - 1]);
            // At most 17 bytes are read: the 16 of the id and one proving it ends.
            let tail = b[(i + 4).min(b.len())..].iter().take(17).take_while(|c| upper_alnum(**c)).count();
            if before_ok && tail == 16 {
                out.push((Kind::AwsAccessKeyId, i..i + 20));
            }
        }
    }
}

fn pem(s: &str, out: &mut Vec<(Kind, Range<usize>)>) {
    for i in find_all(s, "-----BEGIN ") {
        let Some(head_end) = s[i..].find("-----\n").or_else(|| s[i + 11..].find("-----").map(|j| j + 11)) else { continue };
        if !s[i..i + head_end].ends_with("PRIVATE KEY") {
            continue;
        }
        let end = match s[i..].find("-----END ") {
            Some(j) => s[i + j + 9..].find("-----").map(|k| i + j + 9 + k + 5).unwrap_or(s.len()),
            None => s.len(),
        };
        out.push((Kind::PemPrivateKey, i..end));
    }
}

fn github(s: &str, out: &mut Vec<(Kind, Range<usize>)>) {
    let b = s.as_bytes();
    for prefix in ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"] {
        let mut floor = 0;
        for i in find_all(s, prefix) {
            if i < floor {
                continue;
            }
            let n = run_len(b, i + 4, |c| c.is_ascii_alphanumeric());
            floor = i + 4 + n;
            if n >= 36 {
                out.push((Kind::GithubToken, i..i + 4 + n));
            }
        }
    }
    let mut floor = 0;
    for i in find_all(s, "github_pat_") {
        if i < floor {
            continue;
        }
        let n = run_len(b, i + 11, |c| c.is_ascii_alphanumeric() || c == b'_');
        floor = i + 11 + n;
        if n >= 22 {
            out.push((Kind::GithubToken, i..i + 11 + n));
        }
    }
}

fn slack(s: &str, out: &mut Vec<(Kind, Range<usize>)>) {
    let b = s.as_bytes();
    for prefix in ["xoxb-", "xoxp-", "xoxa-", "xoxr-", "xoxs-"] {
        let mut floor = 0;
        for i in find_all(s, prefix) {
            if i < floor {
                continue;
            }
            let n = run_len(b, i + 5, |c| c.is_ascii_alphanumeric() || c == b'-');
            floor = i + 5 + n;
            if n >= 10 {
                out.push((Kind::SlackToken, i..i + 5 + n));
            }
        }
    }
}

fn assignment(s: &str, lower: &str, out: &mut Vec<(Kind, Range<usize>)>) {
    let b = s.as_bytes();
    for k in KEYWORDS {
        // A later keyword inside a value already scanned starts no scan of its own, so each byte is read once per keyword.
        let mut floor = 0;
        for i in find_all(lower, k) {
            if i < floor {
                continue;
            }
            // The keyword is a whole word ending in `=` or `:`.
            if i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_') {
                continue;
            }
            let mut j = i + k.len();
            j += run_len(b, j, |c| c == b' ');
            if j >= b.len() || !(b[j] == b'=' || b[j] == b':') {
                continue;
            }
            j += 1;
            j += run_len(b, j, |c| c == b' ' || c == b'"' || c == b'\'');
            let n = run_len(b, j, |c| !(c.is_ascii_whitespace() || c == b'"' || c == b'\'' || c == b',' || c == b';' || c == b'&'));
            let mut end = j + n;
            floor = end;
            while !s.is_char_boundary(end) {
                end += 1;
            }
            if n >= 8 {
                out.push((Kind::Assignment, j..end));
            }
        }
    }
}

/// Every credential-shaped span of `s`, merged: overlapping spans join under the kind
/// with the higher priority (`run.guard-secrets.mask-span`).
pub fn spans(s: &str) -> Vec<(Kind, Range<usize>)> {
    // ASCII lowercasing keeps every byte offset, so a span found in `lower` indexes `s`.
    let lower = s.to_ascii_lowercase();
    let mut found = Vec::new();
    pem(s, &mut found);
    llm_provider(s, &mut found);
    google(s, &mut found);
    stripe(s, &mut found);
    jwt(s, &mut found);
    aws(s, &mut found);
    github(s, &mut found);
    slack(s, &mut found);
    bearer(s, &lower, &mut found);
    assignment(s, &lower, &mut found);
    found.sort_by_key(|(k, r)| (r.start, *k));
    let mut merged: Vec<(Kind, Range<usize>)> = Vec::new();
    for (k, r) in found {
        match merged.last_mut() {
            Some((mk, mr)) if r.start < mr.end => {
                mr.end = mr.end.max(r.end);
                *mk = (*mk).min(k);
            }
            _ => merged.push((k, r)),
        }
    }
    merged
}

/// `s` with every credential-shaped span replaced by [`MARKER`], and whether any was.
pub fn mask(s: &str) -> Option<String> {
    let found = spans(s);
    if found.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(s.len());
    let mut at = 0;
    for (_, r) in found {
        out.push_str(&s[at..r.start]);
        out.push_str(MARKER);
        at = r.end;
    }
    out.push_str(&s[at..]);
    Some(out)
}

fn mask_value(v: &mut Value) -> bool {
    match v {
        Value::String(s) => match mask(s) {
            Some(m) => {
                *s = m;
                true
            }
            None => false,
        },
        Value::Array(items) => items.iter_mut().fold(false, |any, x| mask_value(x) | any),
        Value::Object(map) => map.values_mut().fold(false, |any, x| mask_value(x) | any),
        _ => false,
    }
}

/// Mask every cell of a batch in place, returning the count of masked cells per column.
/// The guard blocks nothing (`run.guard-secrets.mask-only`).
pub fn guard_rows(rows: &mut [Row]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for row in rows.iter_mut() {
        for (col, v) in row.iter_mut() {
            if mask_value(v) {
                *counts.entry(col.clone()).or_insert(0) += 1;
            }
        }
    }
    counts
}
