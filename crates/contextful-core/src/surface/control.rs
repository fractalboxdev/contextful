//! The snapshot store's names and the pointer grammar (`surface.reconcile`).

use super::SurfaceError;

/// The poll schedule of a `[control]` block declaring none (`surface.reconcile.poll-cadence`).
pub const DEFAULT_POLL: &str = "every 30s";

/// The pointer file naming the applied version.
pub const POINTER_FILE: &str = "manifest@current";

/// The immutable file holding applied version `version`.
pub fn snapshot_file(version: u64) -> String {
    format!("manifest@v{version}.toml")
}

/// Read a pointer body: ASCII digits and at most one trailing newline, nothing else
/// (`surface.reconcile.pointer-malformed`).
pub fn parse_pointer(body: &str) -> Result<u64, SurfaceError> {
    let digits = body.strip_suffix('\n').unwrap_or(body);
    let malformed = |why: &str| SurfaceError::ControlPointerMalformed(format!("pointer body {body:?} {why}"));
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(malformed("is not wholly a version"));
    }
    digits.parse::<u64>().map_err(|_| malformed("overflows a version"))
}
