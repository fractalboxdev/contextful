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
