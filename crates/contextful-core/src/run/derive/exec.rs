//! `run.exec`: the bounds of an argv chain, the placeholders a step's arguments carry,
//! and the identity of an exec engine.

use crate::run::journal::sha256_hex;

/// Wall clock one unit's whole chain runs for: 1800 s (`run.exec.chain-deadline`).
pub const CHAIN_DEADLINE_SECS: u64 = 1800;
/// Captured standard output and error one step may produce: 8 MiB (`run.exec.captured-output`).
pub const CAPTURED_OUTPUT_BYTES: u64 = 8 * 1024 * 1024;
/// Standard error a failing step's error text carries: 4 KiB (`run.exec.step-error-excerpt`).
pub const STEP_ERROR_EXCERPT_BYTES: usize = 4 * 1024;
/// Digest characters in an exec engine id: 12 chars (`run.exec.engine-id`).
pub const ENGINE_ID_PREFIX: usize = 12;
/// Captured-output entries one chain adds to the run record: 64 entries (`run.exec.audit-entries`).
pub const AUDIT_ENTRIES: usize = 64;

/// The captured output one chain adds to the run record: at most 64 entries, each naming
/// its unit, its addresses redacted and its credentials masked; the rest are counted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChainAudit {
    entries: Vec<String>,
    dropped: u64,
}

impl ChainAudit {
    /// Record one entry of `unit`'s captured output, or count it once the bound is reached.
    pub fn record(&mut self, unit: &str, entry: &str) {
        if self.entries.len() >= AUDIT_ENTRIES {
            self.dropped += 1;
            return;
        }
        self.entries.push(format!("{unit}: {}", crate::run::record::mask_credentials(&super::emit::redact(entry))));
    }

    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// Entries past the bound, counted and discarded.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn into_entries(self) -> Vec<String> {
        self.entries
    }
}

/// The deadline a vendor engine's step holds: its own request deadline, clipped to what the
/// chain deadline leaves (`run.exec.vendor-deadline`). A binding declaring no
/// `endpoint_host` reaches no vendor and holds the chain's alone.
pub fn vendor_deadline(binding: &super::config::Binding, chain_left: std::time::Duration) -> std::time::Duration {
    match (&binding.endpoint_host, binding.request_timeout_secs) {
        (Some(_), Some(secs)) => chain_left.min(std::time::Duration::from_secs(secs)),
        _ => chain_left,
    }
}

/// The files one step reads and writes, for placeholder expansion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepFiles {
    pub input: String,
    pub output: String,
    pub output_stem: String,
}

/// Expand `{input}`, `{output}` and `{output_stem}` in one argument. Row data enters as
/// an argument value, never as syntax: no shell reads it.
pub fn expand(arg: &str, files: &StepFiles) -> String {
    arg.replace("{input}", &files.input).replace("{output_stem}", &files.output_stem).replace("{output}", &files.output)
}

/// `exec:<name>@<prefix>`, the prefix 12 chars of lowercase hex over every step's binary
/// digest and arguments.
pub fn engine_id(name: &str, steps: &[(String, Vec<String>)]) -> String {
    let mut material = String::new();
    for (digest, args) in steps {
        material.push_str(digest);
        material.push('\0');
        for a in args {
            material.push_str(a);
            material.push('\0');
        }
        material.push('\n');
    }
    format!("exec:{name}@{}", &sha256_hex(material.as_bytes())[..ENGINE_ID_PREFIX])
}

/// A failing step's standard error cut to its excerpt bound on a char boundary.
pub fn excerpt(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let mut end = text.len().min(STEP_ERROR_EXCERPT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].trim().to_string()
}

/// A preprocess step's `when` condition (`run.exec.step-condition`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    /// The step's input is an `http` or `https` address.
    MediaIsUrl,
    /// The step's input is anything but 16-bit PCM WAV.
    EngineRequiresPcm16Wav,
}

impl Condition {
    pub fn parse(name: &str) -> Option<Condition> {
        match name {
            "media_is_url" => Some(Condition::MediaIsUrl),
            "engine_requires_pcm16_wav" => Some(Condition::EngineRequiresPcm16Wav),
            _ => None,
        }
    }

    /// Whether the condition holds for an input named `input` whose leading bytes are
    /// `head` (none for an address).
    pub fn holds(self, input: &str, head: Option<&[u8]>) -> bool {
        match self {
            Condition::MediaIsUrl => is_url(input),
            Condition::EngineRequiresPcm16Wav => !head.is_some_and(is_pcm16_wav),
        }
    }
}

/// An `http` or `https` address.
pub fn is_url(media: &str) -> bool {
    media.starts_with("http://") || media.starts_with("https://")
}

/// RIFF/WAVE bytes whose `fmt ` chunk declares PCM (format 1, or extensible) at 16 bits per sample.
pub fn is_pcm16_wav(head: &[u8]) -> bool {
    if head.len() < 12 || &head[..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        return false;
    }
    let u16_at = |i: usize| head.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let mut at = 12;
    while let Some(id) = head.get(at..at + 4) {
        let Some(size) = head.get(at + 4..at + 8).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize) else { return false };
        if id == b"fmt " {
            return matches!(u16_at(at + 8), Some(1 | 0xFFFE)) && u16_at(at + 22) == Some(16);
        }
        at = at.saturating_add(8).saturating_add(size + size % 2);
    }
    false
}
