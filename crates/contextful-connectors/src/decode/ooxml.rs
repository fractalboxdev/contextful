//! The Office Open XML archive: parts read by exact name, every inflated byte charged to
//! one budget (`connector.source.office-part-selection`,
//! `connector.source.decompression-budget`). Nothing is extracted to disk.

use super::unreadable;
use contextful_core::run::{Failure, FailureTag, RunError};
use quick_xml::events::BytesText;
use quick_xml::name::ResolveResult;
use std::io::{Cursor, Read};

/// Decompressed bytes one office read inflates across every part it opens: 64 MiB
/// (`connector.source.decompression-budget`).
pub const INFLATE_BUDGET: u64 = 64 * 1024 * 1024;

/// An open archive and the bytes inflated out of it so far.
pub struct Archive<'a> {
    zip: zip::ZipArchive<Cursor<&'a [u8]>>,
    input: &'a str,
    inflated: u64,
}

impl<'a> Archive<'a> {
    pub fn open(bytes: &'a [u8], input: &'a str) -> Result<Archive<'a>, Failure> {
        let zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| unreadable(input, "the archive directory".into(), format!("not an Office Open XML archive: {e}")))?;
        Ok(Archive { zip, input, inflated: 0 })
    }

    /// Whether any entry name starts with `prefix`, judged on the directory alone.
    pub fn has_prefix(&self, prefix: &str) -> bool {
        self.zip.file_names().any(|n| n.starts_with(prefix))
    }

    /// Part `name`, or `None` when the archive carries none. The directory's claim is
    /// judged before a byte inflates and the bytes that arrive are judged again, since a
    /// bomb is an archive misreporting its own sizes.
    pub fn part(&mut self, name: &str) -> Result<Option<String>, Failure> {
        let input = self.input;
        let at = || format!("part `{name}`");
        let remaining = INFLATE_BUDGET - self.inflated;
        let mut entry = match self.zip.by_name(name) {
            Ok(entry) => entry,
            Err(zip::result::ZipError::FileNotFound) => return Ok(None),
            Err(e) => return Err(unreadable(input, at(), e)),
        };
        if !entry.is_file() {
            return Err(unreadable(input, at(), "the entry is not a file"));
        }
        if entry.size() > remaining {
            return Err(over_budget(input, name, format!("declares {} decompressed bytes", entry.size())));
        }
        let mut buf = Vec::with_capacity(entry.size().min(1 << 20) as usize);
        (&mut entry).take(remaining + 1).read_to_end(&mut buf).map_err(|e| unreadable(input, at(), e))?;
        if buf.len() as u64 > remaining {
            return Err(over_budget(input, name, format!("inflates past its declared {} bytes", entry.size())));
        }
        self.inflated += buf.len() as u64;
        String::from_utf8(buf).map(Some).map_err(|e| unreadable(input, at(), format!("the part is not UTF-8: {e}")))
    }

    /// As [`Archive::part`], a missing part unreadable input.
    pub fn require(&mut self, name: &str) -> Result<String, Failure> {
        self.part(name)?.ok_or_else(|| unreadable(self.input, format!("part `{name}`"), "the archive carries no such part"))
    }
}

fn over_budget(input: &str, name: &str, why: String) -> Failure {
    Failure::deterministic(
        FailureTag::Permanent,
        RunError::PipelineUnreadableInput(format!("`{input}` at part `{name}`: the part {why}, past the {INFLATE_BUDGET}-byte budget one office read decompresses")).to_string(),
    )
}

/// Whether a resolved namespace is one of `want`.
pub fn in_ns(resolved: &ResolveResult<'_>, want: &[&str]) -> bool {
    matches!(resolved, ResolveResult::Bound(ns) if want.iter().any(|w| ns.as_ref() == w.as_bytes()))
}

/// Element text with the five XML built-ins and character references expanded. Any
/// other entity reference stays literal: no external entity is ever resolved.
pub fn text(raw: &BytesText<'_>) -> String {
    match raw.unescape() {
        Ok(t) => t.into_owned(),
        Err(_) => String::from_utf8_lossy(raw.as_ref()).into_owned(),
    }
}

