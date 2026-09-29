//! `assurance.release`: the version a release tag carries, computed from the tagged commit's
//! `spec/status.md`, and the refusals a signed annotated tag waits on. The cheap refusals
//! run first, so a mismatch names the computed version before the gate spends its build.

use crate::{gate, git, refuse, BASE_RUN_BOUND_SECS};
use anyhow::{Context, Result};
use std::fmt;
use std::process::Command;
use std::time::Duration;

const STATUS_FILE: &str = "spec/status.md";
const MILESTONES_HEADING: &str = "## Milestones";
const CLOSED_COLUMN: &str = "Closed";
const CLOSED: &str = "closed";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    /// A tag name `v<major>.<minor>.<patch>` of three decimal fields; any other name is no version.
    fn of_tag(name: &str) -> Option<Version> {
        let mut fields = name.strip_prefix('v')?.split('.').map(|f| {
            if f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            f.parse().ok()
        });
        let v = Version { major: fields.next()??, minor: fields.next()??, patch: fields.next()?? };
        fields.next().is_none().then_some(v)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Cut the release tag on `HEAD`: `branch` names the default branch, and `base` is the
/// revision the gate's test-first stage measures `HEAD` against.
pub fn tag(branch: &str, base: &str) -> Result<()> {
    // `assurance.release.dirty-tree`: untracked files count, since the gate reads them.
    let changed = git(&["status", "--porcelain"])?;
    if !changed.is_empty() {
        let files: Vec<&str> = changed.lines().map(|l| l.get(3..).unwrap_or(l)).collect();
        return Err(refuse("TagTreeDirty", format!("the working tree differs from HEAD in {}", files.join(", "))));
    }
    let head = git(&["rev-parse", "HEAD"])?;
    let short = &head[..head.len().min(12)];

    // `assurance.release.off-branch`
    let tip = git(&["rev-parse", "--verify", &format!("{branch}^{{commit}}")])
        .with_context(|| format!("resolving the default branch `{branch}`"))?;
    if !reaches(&tip, &head)? {
        return Err(refuse(
            "TagOffDefaultBranch",
            format!("HEAD {short} is not reachable from the default branch `{branch}`"),
        ));
    }

    // `assurance.release.version`
    let status = git(&["show", &format!("HEAD:{STATUS_FILE}")]).with_context(|| format!("reading {STATUS_FILE} at HEAD"))?;
    let closed = closed_milestones(&status);
    let existing: Vec<Version> = git(&["tag", "--list"])?.lines().filter_map(Version::of_tag).collect();
    let minor = closed.len() as u64;
    let patch = existing.iter().filter(|v| v.major == 0 && v.minor == minor).map(|v| v.patch + 1).max().unwrap_or(0);
    let version = Version { major: 0, minor, patch };

    // `assurance.release.version-regressed`
    if let Some(last) = existing.iter().max().filter(|last| version < **last) {
        return Err(refuse(
            "TagVersionRegressed",
            format!("{} milestone(s) close at HEAD, giving v{version}, below the existing tag v{last}", closed.len()),
        ));
    }

    // `assurance.release.workspace-version`
    let manifest: toml::Table = git(&["show", "HEAD:Cargo.toml"])?.parse().context("parsing Cargo.toml at HEAD")?;
    let declared = manifest
        .get("workspace")
        .and_then(|w| w.get("package"))
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str());
    if declared != Some(version.to_string().as_str()) {
        let found = declared.map_or("no version".to_string(), |d| format!("version {d}"));
        return Err(refuse(
            "TagWorkspaceVersionMismatch",
            format!("Cargo.toml [workspace.package] declares {found}; the computed tag is v{version}, so commit version = \"{version}\" first"),
        ));
    }

    // `assurance.release.gate-failed`
    eprintln!("tag: v{version} on {short}; running every gate stage against {base}");
    gate(&[], base, Duration::from_secs(BASE_RUN_BOUND_SECS))
        .map_err(|e| refuse("TagGateFailed", format!("the gate fails on {short}, so v{version} is not cut: {e:#}")))?;

    // `assurance.release.annotated`: without `--force`, git refuses an existing name.
    let name = format!("v{version}");
    let mut message = format!("Contextful {name}\n\n{} milestone(s) close:\n", closed.len());
    closed.iter().for_each(|m| message.push_str(&format!("- {m}\n")));
    git(&["tag", "--sign", "--message", &message, &name, &head]).context("creating the signed tag")?;
    println!("tag: created signed {name} on {short}; publish it with `git push origin {name}`");
    Ok(())
}

/// Whether `commit` is an ancestor of, or equal to, `tip`.
fn reaches(tip: &str, commit: &str) -> Result<bool> {
    let status = Command::new("git").args(["merge-base", "--is-ancestor", commit, tip]).status().context("running git")?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => anyhow::bail!("git merge-base --is-ancestor {commit} {tip} exited {status}"),
    }
}

/// The label of each row of the `## Milestones` table whose `Closed` cell reads `closed`.
/// A table without a `Closed` column closes no milestone.
fn closed_milestones(status: &str) -> Vec<String> {
    let cells = |row: &str| -> Vec<String> { row.trim().trim_matches('|').split('|').map(|c| c.trim().to_string()).collect() };
    let mut lines = status.lines().skip_while(|l| l.trim() != MILESTONES_HEADING).skip(1);
    let mut table = lines.by_ref().skip_while(|l| !l.trim_start().starts_with('|')).take_while(|l| l.trim_start().starts_with('|'));
    let Some(column) = table.next().and_then(|header| cells(header).iter().position(|c| c == CLOSED_COLUMN)) else {
        return Vec::new();
    };
    table
        .skip(1)
        .filter_map(|row| {
            let cells = cells(row);
            (cells.get(column).map(String::as_str) == Some(CLOSED)).then(|| cells[0].clone())
        })
        .collect()
}
