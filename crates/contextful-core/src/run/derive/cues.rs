//! `run.parse-cues`: one grammar for SubRip and WebVTT, the blocks it drops, and the
//! coalescing of cues into passages.

use crate::run::RunError;

/// A passage closes once it holds 600 chars (`run.parse-cues.passage-chars`).
pub const PASSAGE_CHARS: usize = 600;
/// A passage closes once it spans 60 s (`run.parse-cues.passage-span`).
pub const PASSAGE_SPAN_MS: u64 = 60_000;
/// One passage's text holds at most 8 KiB (`run.parse-cues.passage-bytes`).
pub const PASSAGE_BYTES: usize = 8 * 1024;
/// One document yields at most 2000 passages (`run.parse-cues.passages-per-document`).
pub const PASSAGES_PER_DOCUMENT: usize = 2000;

/// One timed cue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cue {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

/// A parsed document: its cues in order, and the refusals for the blocks it dropped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parsed {
    pub cues: Vec<Cue>,
    pub defects: Vec<RunError>,
}

/// `HH:MM:SS,mmm`, `HH:MM:SS.mmm` or `MM:SS.mmm` in milliseconds.
fn stamp(s: &str) -> Option<u64> {
    let s = s.trim();
    let (clock, frac) = s.rsplit_once([',', '.'])?;
    let ms: u64 = frac.get(..3)?.parse().ok()?;
    let parts: Vec<u64> = clock.split(':').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    let secs = match parts.as_slice() {
        [h, m, s] => h * 3600 + m * 60 + s,
        [m, s] => m * 60 + s,
        _ => return None,
    };
    Some(secs * 1000 + ms)
}

/// Parse a SubRip or WebVTT document: each block's timing line and the text lines after
/// it. A block starting before the last accepted one is dropped with `DeriveCueOutOfOrder`.
pub fn parse(document: &str) -> Parsed {
    let mut out = Parsed::default();
    let text = document.strip_prefix('\u{feff}').unwrap_or(document).replace("\r\n", "\n");
    for block in text.split("\n\n") {
        let lines: Vec<&str> = block.lines().filter(|l| !l.trim().is_empty()).collect();
        let Some(timing) = lines.iter().position(|l| l.contains("-->")) else { continue };
        let Some((a, b)) = lines[timing].split_once("-->") else { continue };
        let end_field = b.split_whitespace().next().unwrap_or_default();
        let (Some(start_ms), Some(end_ms)) = (stamp(a), stamp(end_field)) else { continue };
        let body: Vec<&str> = lines[timing + 1..].iter().map(|l| l.trim()).collect();
        if body.is_empty() {
            continue;
        }
        if out.cues.last().is_some_and(|c| start_ms < c.start_ms) {
            out.defects.push(RunError::DeriveCueOutOfOrder(format!(
                "a block starting at {start_ms} ms follows one starting at {} ms; it is dropped",
                out.cues.last().map(|c| c.start_ms).unwrap_or_default()
            )));
            continue;
        }
        out.cues.push(Cue { start_ms, end_ms, text: body.join(" ") });
    }
    out
}

/// Coalesce cues into passages: a passage stays open while it holds fewer than 600 chars
/// and spans less than 60 s, holds at most 8 KiB, and a document yields at most 2000.
pub fn passages(cues: &[Cue]) -> Vec<Cue> {
    let mut out: Vec<Cue> = Vec::new();
    let mut open: Option<Cue> = None;
    for cue in cues {
        let current = match open.take() {
            None => cue.clone(),
            Some(mut p) => {
                p.text.push(' ');
                p.text.push_str(&cue.text);
                p.end_ms = p.end_ms.max(cue.end_ms);
                p
            }
        };
        if current.text.chars().count() >= PASSAGE_CHARS || current.end_ms.saturating_sub(current.start_ms) >= PASSAGE_SPAN_MS {
            out.push(current);
        } else {
            open = Some(current);
        }
        if out.len() >= PASSAGES_PER_DOCUMENT {
            open = None;
            break;
        }
    }
    out.extend(open);
    out.truncate(PASSAGES_PER_DOCUMENT);
    for p in &mut out {
        if p.text.len() > PASSAGE_BYTES {
            let mut end = PASSAGE_BYTES;
            while !p.text.is_char_boundary(end) {
                end -= 1;
            }
            p.text.truncate(end);
        }
    }
    out
}
