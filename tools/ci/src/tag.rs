//! `assurance.release`: the version a release tag carries, computed from the tagged commit's
//! `spec/status.md` and store format constant, and the refusals a signed annotated tag waits on. The cheap refusals
//! run first, so a mismatch names the computed version before the gate spends its build.

use crate::{gate, git, refuse, BASE_RUN_BOUND_SECS};
use anyhow::{Context, Result};
use std::fmt;
use std::process::Command;
use std::time::Duration;
use syn::visit::Visit;

const STATUS_FILE: &str = "spec/status.md";
const MILESTONES_HEADING: &str = "## Milestones";
const CLOSED_COLUMN: &str = "Closed";
const CLOSED: &str = "closed";
const STORE_FORMAT_FILE: &str = "crates/contextful-core/src/store/lay_out.rs";

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

/// A commit's store format, `<major>.<minor>` (`assurance.release.store-format`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StoreFormat {
    major: u64,
    minor: u64,
}

impl StoreFormat {
    /// The `FormatVersion::CURRENT` constant in `rev`'s `STORE_FORMAT_FILE`; `1.0` where the
    /// file or the constant is absent, since a manifest without `format_version` reads as `1.0`.
    fn at(rev: &str) -> Result<StoreFormat> {
        let path = format!("{rev}:{STORE_FORMAT_FILE}");
        let Some(source) = git(&["cat-file", "-e", &path]).ok().map(|_| git(&["show", &path])).transpose()? else {
            return Ok(StoreFormat { major: 1, minor: 0 });
        };
        let unread = || refuse("TagStoreFormatUnread", format!("{STORE_FORMAT_FILE} at {rev} has an unreadable FormatVersion::CURRENT"));
        let syntax = syn::parse_file(&source).map_err(|_| unread())?;
        let mut current = Current::default();
        current.visit_file(&syntax);
        match current.constants.as_slice() {
            [] => Ok(StoreFormat { major: 1, minor: 0 }),
            [constant] => Self::from_constant(constant).ok_or_else(unread),
            _ => Err(unread()),
        }
    }

    fn from_constant(constant: &syn::ImplItemConst) -> Option<StoreFormat> {
        let syn::Type::Path(ty) = &constant.ty else { return None };
        if !(ty.path.is_ident("Self") || names_format_version(&ty.path)) {
            return None;
        }
        let syn::Expr::Struct(value) = &constant.expr else { return None };
        if !(value.path.is_ident("Self") || names_format_version(&value.path)) || value.rest.is_some() || value.fields.len() != 2 {
            return None;
        }
        let mut major = None;
        let mut minor = None;
        for field in &value.fields {
            let syn::Member::Named(name) = &field.member else { return None };
            let target = match name.to_string().as_str() {
                "major" => &mut major,
                "minor" => &mut minor,
                _ => return None,
            };
            if target.is_some() {
                return None;
            }
            let syn::Expr::Lit(literal) = &field.expr else { return None };
            let syn::Lit::Int(number) = &literal.lit else { return None };
            *target = Some(number.base10_parse().ok()?);
        }
        Some(StoreFormat { major: major?, minor: minor? })
    }
}

#[derive(Default)]
struct Current<'ast> {
    constants: Vec<&'ast syn::ImplItemConst>,
}

impl<'ast> Visit<'ast> for Current<'ast> {
    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        if item.trait_.is_none() && matches!(&*item.self_ty, syn::Type::Path(ty) if names_format_version(&ty.path)) {
            self.constants.extend(item.items.iter().filter_map(|item| match item {
                syn::ImplItem::Const(constant) if constant.ident == "CURRENT" => Some(constant),
                _ => None,
            }));
        }
        syn::visit::visit_item_impl(self, item);
    }
}

fn names_format_version(path: &syn::Path) -> bool {
    path.segments.last().is_some_and(|segment| segment.ident == "FormatVersion")
}

impl fmt::Display for StoreFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
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
    let format = StoreFormat::at(&head)?;
    let existing: Vec<Version> = git(&["tag", "--list"])?.lines().filter_map(Version::of_tag).collect();
    let minor = closed.len() as u64 + format.major.saturating_sub(1);
    let patch = existing.iter().filter(|v| v.major == 0 && v.minor == minor).map(|v| v.patch + 1).max().unwrap_or(0);
    let version = Version { major: 0, minor, patch };

    // `assurance.release.version-regressed`
    if let Some(last) = existing.iter().max().filter(|last| version < **last) {
        return Err(refuse(
            "TagVersionRegressed",
            format!("{} milestone(s) close at HEAD, giving v{version}, below the existing tag v{last}", closed.len()),
        ));
    }

    // `assurance.release.format-break-patch`: the last tag's format, read at that tag.
    let last = existing.iter().max().map(|last| StoreFormat::at(&format!("v{last}")).map(|f| (*last, f))).transpose()?;
    if let Some((last, was)) = last.filter(|(last, was)| last.minor == version.minor && was.major != format.major) {
        return Err(refuse(
            "TagFormatBreakPatch",
            format!("the store format moves from {was} at v{last} to {format} at HEAD, yet v{version} keeps the minor version"),
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
    gate(&[], false, base, Duration::from_secs(BASE_RUN_BOUND_SECS))
        .map_err(|e| refuse("TagGateFailed", format!("the gate fails on {short}, so v{version} is not cut: {e:#}")))?;

    // `assurance.release.annotated`: without `--force`, git refuses an existing name.
    let name = format!("v{version}");
    let mut message = format!("Contextful {name}\n\n{} milestone(s) close:\n", closed.len());
    closed.iter().for_each(|m| message.push_str(&format!("- {m}\n")));
    // `assurance.release.format-notes`
    if let Some((_, was)) = last.filter(|(_, was)| format.major > was.major) {
        message.push_str(&format!("\nStore format {was} → {format}: upgrade every reader, then every writer together.\n"));
    }
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
