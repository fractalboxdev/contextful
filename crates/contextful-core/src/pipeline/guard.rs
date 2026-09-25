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
    AwsAccessKeyId,
    GithubToken,
    SlackToken,
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
    bytes[from..].iter().take_while(|b| ok(**b)).count()
}

fn aws(s: &str, out: &mut Vec<(Kind, Range<usize>)>) {
    let b = s.as_bytes();
    for prefix in ["AKIA", "ASIA"] {
        for i in find_all(s, prefix) {
            let before_ok = i == 0 || !upper_alnum(b[i - 1]);
            let tail = run_len(b, i + 4, upper_alnum);
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
        for i in find_all(s, prefix) {
            let n = run_len(b, i + 4, |c| c.is_ascii_alphanumeric());
            if n >= 36 {
                out.push((Kind::GithubToken, i..i + 4 + n));
            }
        }
    }
    for i in find_all(s, "github_pat_") {
        let n = run_len(b, i + 11, |c| c.is_ascii_alphanumeric() || c == b'_');
        if n >= 22 {
            out.push((Kind::GithubToken, i..i + 11 + n));
        }
    }
}

fn slack(s: &str, out: &mut Vec<(Kind, Range<usize>)>) {
    let b = s.as_bytes();
    for prefix in ["xoxb-", "xoxp-", "xoxa-", "xoxr-", "xoxs-"] {
        for i in find_all(s, prefix) {
            let n = run_len(b, i + 5, |c| c.is_ascii_alphanumeric() || c == b'-');
            if n >= 10 {
                out.push((Kind::SlackToken, i..i + 5 + n));
            }
        }
    }
}

fn assignment(s: &str, out: &mut Vec<(Kind, Range<usize>)>) {
    let lower = s.to_ascii_lowercase();
    let b = s.as_bytes();
    for k in KEYWORDS {
        for i in find_all(&lower, k) {
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
    let mut found = Vec::new();
    pem(s, &mut found);
    aws(s, &mut found);
    github(s, &mut found);
    slack(s, &mut found);
    assignment(s, &mut found);
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
