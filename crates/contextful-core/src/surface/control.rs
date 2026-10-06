//! The snapshot store's names and the pointer grammar (`surface.reconcile`).

use super::arm::Schedule;
use super::SurfaceError;
use std::net::SocketAddr;
use url::Url;

/// The poll schedule of a `[control]` block declaring none (`surface.reconcile.poll-cadence`).
pub const DEFAULT_POLL: &str = "every 30s";

/// The poll schedule of `[control] poll`: any schedule string, or every 30 s when the block
/// declares none (`surface.reconcile.poll-cadence`).
pub fn poll_schedule(declared: Option<&str>) -> Result<Schedule, SurfaceError> {
    Schedule::parse(declared.unwrap_or(DEFAULT_POLL))
}

/// The pointer file naming the applied version.
pub const POINTER_FILE: &str = "manifest@current";

/// The immutable file holding applied version `version`.
pub fn snapshot_file(version: u64) -> String {
    format!("manifest@v{version}.toml")
}

/// The immutable signed receipt beside applied version `version`.
pub fn receipt_file(version: u64) -> String {
    format!("receipt@v{version}.json")
}

/// The canonical version named by one signed receipt filename.
pub fn receipt_version(name: &str) -> Option<u64> {
    let digits = name.strip_prefix("receipt@v")?.strip_suffix(".json")?;
    let version: u64 = digits.parse().ok()?;
    (digits == version.to_string()).then_some(version)
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

/// Read a `[control] url`: an `http` or `https` URL with no userinfo, query or fragment,
/// under which the pointer and each version file sit (`surface.reconcile.url-layout`).
pub fn control_url(text: &str) -> Result<Url, SurfaceError> {
    let refused = |why: &str| SurfaceError::ControlSourceNotLoopback(format!("control URL `{text}` {why}"));
    let url = Url::parse(text).map_err(|e| refused(&format!("does not parse: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(refused("is not http or https"));
    }
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err(refused("carries userinfo, a query or a fragment"));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(refused("names no host"));
    }
    Ok(url)
}

/// The URL of `file` beneath control URL `base` (`surface.reconcile.url-layout`).
pub fn source_file(base: &Url, file: &str) -> Url {
    let mut url = base.clone();
    let path = format!("{}/{file}", base.path().trim_end_matches('/'));
    url.set_path(&path);
    url
}

/// Admit a control host only when it resolves, and every address it resolves to is a
/// loopback address (`surface.reconcile.loopback-only`).
pub fn admit_loopback(host: &str, addrs: &[SocketAddr]) -> Result<(), SurfaceError> {
    if addrs.is_empty() || !addrs.iter().all(|a| a.ip().is_loopback()) {
        let shown: Vec<String> = addrs.iter().map(|a| a.ip().to_string()).collect();
        return Err(SurfaceError::ControlSourceNotLoopback(format!(
            "control host `{host}` resolves to [{}]; a control source is a loopback address",
            shown.join(", ")
        )));
    }
    Ok(())
}

/// Filesystem kinds whose exclusive create and advisory lock are not linearizable across
/// clients: network and user-space filesystems that cache or emulate either.
pub const WEAK_FILESYSTEMS: [&str; 13] = ["nfs", "nfs4", "smbfs", "cifs", "smb2", "smb3", "afpfs", "webdav", "davfs", "9p", "sshfs", "fuse.sshfs", "fuse"];

/// Admit the filesystem holding `role`'s conditional writes (a snapshot directory's claim, a
/// catalog's lease rows) only when its conditional replacement is linearizable; a kind in
/// [`WEAK_FILESYSTEMS`] raises `ConditionalWriteUnsupported` (`surface.apply.weak-conditional-backend`).
pub fn admit_conditional(role: &str, path: &str, fs_kind: Option<&str>) -> Result<(), SurfaceError> {
    match fs_kind {
        Some(kind) if WEAK_FILESYSTEMS.contains(&kind.to_ascii_lowercase().as_str()) => Err(SurfaceError::ConditionalWriteUnsupported(format!(
            "{role} `{path}` sits on a `{kind}` filesystem, whose exclusive create and lock are not linearizable across clients; place it on a local filesystem"
        ))),
        _ => Ok(()),
    }
}
