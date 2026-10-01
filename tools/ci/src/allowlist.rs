//! The connector authoring dependency allowlist (`assurance.build.dependency-allowlist`):
//! the third-party crates a connector package depends on directly, declared under
//! `[package.metadata.contextful] authoring-allowlist` in `crates/contextful-connectors`.
//! It carries the linear-time `regex` engine and `serde_json`, whose parser stops at a
//! fixed nesting depth, and admits no backtracking regex engine and no parser recursing
//! without bound.

use anyhow::{bail, Context, Result};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// The package declaring the allowlist.
const OWNER: &str = "contextful-connectors";
/// The packages a connector author writes against: each direct third-party dependency of
/// these is on the allowlist.
const AUTHORING: [&str; 2] = ["contextful-connectors", "contextful-decode"];
/// What the allowlist carries: a linear-time regex engine and bounded-depth deserialization.
const REQUIRED: [&str; 2] = ["regex", "serde_json"];
/// What it never admits: backtracking regex engines, and crates lifting a parser's
/// recursion bound.
const BANNED: [&str; 6] = ["fancy-regex", "pcre2", "onig", "regress", "serde_stacker", "serde-stacker"];
/// The `serde_json` feature removing its recursion limit.
const UNBOUNDED: &str = "unbounded_depth";

/// Hold the allowlist to its required and banned entries and each authoring package's direct
/// dependencies to it. A workspace without the owning package passes.
pub fn check(root: &Path) -> Result<()> {
    let out = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--locked", "-q"])
        .current_dir(root)
        .output()
        .context("running cargo metadata")?;
    if !out.status.success() {
        bail!("cargo metadata: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).context("parsing cargo metadata")?;
    let packages = meta["packages"].as_array().cloned().unwrap_or_default();
    let members: BTreeSet<String> = meta["workspace_members"].as_array().into_iter().flatten().filter_map(|m| m.as_str()).map(str::to_string).collect();
    let local: BTreeSet<&str> =
        packages.iter().filter(|p| p["id"].as_str().is_some_and(|id| members.contains(id))).filter_map(|p| p["name"].as_str()).collect();
    let Some(owner) = packages.iter().find(|p| p["name"] == OWNER && local.contains(OWNER)) else { return Ok(()) };
    let allowed: BTreeSet<String> = owner["metadata"]["contextful"]["authoring-allowlist"]
        .as_array()
        .with_context(|| format!("{OWNER} declares no `[package.metadata.contextful] authoring-allowlist`"))?
        .iter()
        .filter_map(|v| v.as_str())
        .map(str::to_string)
        .collect();

    let mut faults = Vec::new();
    for r in REQUIRED {
        if !allowed.contains(r) {
            faults.push(format!("the connector authoring allowlist lacks `{r}`"));
        }
    }
    for b in BANNED {
        if allowed.contains(b) {
            faults.push(format!("the connector authoring allowlist admits `{b}`, a backtracking regex engine or an unbounded recursive parser"));
        }
    }
    for p in packages.iter().filter(|p| p["name"].as_str().is_some_and(|n| AUTHORING.contains(&n) && local.contains(n))) {
        let name = p["name"].as_str().unwrap_or_default();
        for d in p["dependencies"].as_array().into_iter().flatten() {
            let dep = d["name"].as_str().unwrap_or_default();
            if d["kind"].is_null() && !local.contains(dep) && !allowed.contains(dep) {
                faults.push(format!("`{name}` depends on `{dep}`, absent from the connector authoring allowlist"));
            }
            if dep == "serde_json" && d["features"].as_array().is_some_and(|f| f.iter().any(|x| x == UNBOUNDED)) {
                faults.push(format!("`{name}` enables `serde_json/{UNBOUNDED}`, lifting the parser's recursion bound"));
            }
        }
    }
    let unbounded = meta["resolve"]["nodes"].as_array().into_iter().flatten().any(|n| {
        n["id"].as_str().is_some_and(|id| id.contains("serde_json@") || id.starts_with("serde_json "))
            && n["features"].as_array().is_some_and(|f| f.iter().any(|x| x == UNBOUNDED))
    });
    if unbounded {
        faults.push(format!("the resolved graph enables `serde_json/{UNBOUNDED}`, lifting the parser's recursion bound"));
    }
    if faults.is_empty() {
        println!("allowlist: {} entries; {} authoring package(s) within it", allowed.len(), AUTHORING.len());
        return Ok(());
    }
    faults.iter().for_each(|f| eprintln!("{f}"));
    bail!("{} connector authoring allowlist fault(s)", faults.len())
}
