//! `assurance.automate.unchecked-shell`: every tracked shell file passes `shellcheck`.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::refuse;

/// The release the toolchain stage provisions when `shellcheck` is absent from `PATH`.
const VERSION: &str = "v0.10.0";
/// `(architecture, SHA-256 of the release archive)` per Linux architecture served.
const ARCHIVES: [(&str, &str); 2] = [
    ("x86_64", "6c881ab0698e4e6ea235245f22832860544f17ba386442fe7e9d629f8cbedf87"),
    ("aarch64", "324a7e89de8fa2aed0d0c28f3dab59cf84c6d74264022c00c22af665ed1a09bb"),
];

/// Run `shellcheck` over every tracked shell file; a rejection raises `ShellCheckFailed`
/// naming each finding. A tree tracking no shell file needs no `shellcheck`.
pub fn check(root: &Path) -> Result<()> {
    let files: Vec<String> = crate::tracked(root)?.into_iter().filter(|p| is_shell(root, p)).collect();
    if files.is_empty() {
        eprintln!("shellcheck: no tracked shell file");
        return Ok(());
    }
    let binary = provision(root)?;
    let out = Command::new(&binary)
        .args(["--format", "gcc"])
        .args(&files)
        .current_dir(root)
        .output()
        .with_context(|| format!("running {}", binary.display()))?;
    if out.status.success() {
        eprintln!("shellcheck: {} shell file(s) pass", files.len());
        return Ok(());
    }
    let report = String::from_utf8_lossy(&out.stdout);
    let findings: Vec<&str> = report.lines().filter(|l| !l.trim().is_empty()).collect();
    if findings.is_empty() {
        bail!("shellcheck exited {} without a finding: {}", out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).trim());
    }
    findings.iter().for_each(|f| eprintln!("ShellCheckFailed: {f}"));
    Err(refuse("ShellCheckFailed", format!("{} finding(s) in the tracked shell files, first {}", findings.len(), findings[0])))
}

/// A tracked file named `*.sh` or `*.bash`, or an extensionless file opening with a `sh`,
/// `bash`, `dash` or `ksh` shebang.
fn is_shell(root: &Path, path: &str) -> bool {
    if path.ends_with(".sh") || path.ends_with(".bash") {
        return true;
    }
    if Path::new(path).extension().is_some() {
        return false;
    }
    let Ok(bytes) = std::fs::read(root.join(path)) else { return false };
    let first = bytes.split(|b| *b == b'\n').next().unwrap_or_default();
    let Some(shebang) = std::str::from_utf8(first).ok().and_then(|l| l.strip_prefix("#!")) else { return false };
    let mut words = shebang.split_whitespace();
    let program = words.next().unwrap_or_default();
    let program = if program.ends_with("/env") { words.find(|w| !w.starts_with('-')).unwrap_or_default() } else { program };
    matches!(program.rsplit('/').next(), Some("sh" | "bash" | "dash" | "ksh"))
}

/// `shellcheck` from `PATH`, or the pinned release fetched into the gate directory.
fn provision(root: &Path) -> Result<PathBuf> {
    if Command::new("shellcheck").arg("--version").output().is_ok_and(|o| o.status.success()) {
        return Ok(PathBuf::from("shellcheck"));
    }
    let dir = root.join(crate::stage::GATE_DIR).join(format!("shellcheck-{VERSION}"));
    let binary = dir.join("shellcheck");
    if binary.is_file() {
        return Ok(binary);
    }
    let arch = std::env::consts::ARCH;
    let Some((_, digest)) = ARCHIVES.iter().find(|(a, _)| std::env::consts::OS == "linux" && *a == arch) else {
        bail!("shellcheck is absent from PATH and no pinned {VERSION} release serves {}-{arch}", std::env::consts::OS);
    };
    std::fs::create_dir_all(&dir)?;
    let archive = dir.join("shellcheck.tar.xz");
    let url = format!("https://github.com/koalaman/shellcheck/releases/download/{VERSION}/shellcheck-{VERSION}.linux.{arch}.tar.xz");
    eprintln!("shellcheck: fetching {url}");
    crate::run(root, "curl", &["-sSfL", "-o", &archive.to_string_lossy(), &url])?;
    let got = format!("{:x}", Sha256::digest(std::fs::read(&archive)?));
    if got != *digest {
        let _ = std::fs::remove_file(&archive);
        bail!("the shellcheck {VERSION} archive digests to {got}, not the pinned {digest}");
    }
    crate::run(root, "tar", &["-xJf", &archive.to_string_lossy(), "-C", &dir.to_string_lossy(), "--strip-components=1", &format!("shellcheck-{VERSION}/shellcheck")])?;
    let _ = std::fs::remove_file(&archive);
    Ok(binary)
}
