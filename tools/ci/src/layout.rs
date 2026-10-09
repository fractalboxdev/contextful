//! Test placement (`assurance.test.one-integration-binary`, `assurance.test.feature-gated-suite`,
//! `assurance.test.own-process`): one integration target per package, a feature-gated suite
//! gated at the head of its own file, and a top-level test file saying why it needs its
//! own process.

use anyhow::{bail, Context, Result};
use std::path::Path;

const MAIN: &str = "tests/integration/main.rs";

/// Hold every package under `crates/` and `tools/` holding a `tests/` directory to the
/// placement rules, naming every breach.
pub fn check(root: &Path) -> Result<()> {
    let mut breaches = Vec::new();
    let mut packages = 0;
    for top in ["crates", "tools"] {
        let Ok(entries) = std::fs::read_dir(root.join(top)) else { continue };
        let mut dirs: Vec<_> = entries.flatten().map(|e| e.path()).filter(|p| p.join("Cargo.toml").is_file() && p.join("tests").is_dir()).collect();
        dirs.sort();
        for dir in dirs {
            packages += 1;
            let rel = dir.strip_prefix(root).unwrap_or(&dir).display().to_string();
            package(&dir, &rel, &mut breaches)?;
        }
    }
    if breaches.is_empty() {
        eprintln!("test-layout: {packages} package(s) hold one integration target each");
        return Ok(());
    }
    breaches.iter().for_each(|b| eprintln!("test-layout: {b}"));
    bail!("{} test placement breach(es), first {}", breaches.len(), breaches[0])
}

fn package(dir: &Path, rel: &str, breaches: &mut Vec<String>) -> Result<()> {
    let manifest: toml::Value = std::fs::read_to_string(dir.join("Cargo.toml"))?.parse().with_context(|| format!("parsing {rel}/Cargo.toml"))?;
    let main = dir.join(MAIN);
    if !main.is_file() {
        breaches.push(format!("{rel} has tests/ but no {MAIN}"));
    }
    for target in manifest.get("test").and_then(toml::Value::as_array).into_iter().flatten() {
        let path = target.get("path").and_then(toml::Value::as_str).unwrap_or_default();
        if path != MAIN {
            let name = target.get("name").and_then(toml::Value::as_str).unwrap_or_default();
            breaches.push(format!("{rel}/Cargo.toml declares test target `{name}` at `{path}`, a second integration binary"));
        }
    }
    for entry in std::fs::read_dir(dir.join("tests"))?.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|e| e == "rs") && !states_own_process(&std::fs::read_to_string(&path)?) {
            breaches.push(format!("{rel}/tests/{} is a top-level test file whose `//!` lede states no reason it needs its own process", entry.file_name().to_string_lossy()));
        }
    }
    if main.is_file() {
        gated_suites(&main, rel, breaches)?;
    }
    Ok(())
}

/// The file's leading `//!` lines name its own process.
fn states_own_process(text: &str) -> bool {
    text.lines().take_while(|l| l.trim_start().starts_with("//!")).any(|l| l.contains("own process"))
}

/// Each `mod <name>;` declared under a `cfg` naming a feature opens its file with a
/// `#![cfg(…feature…)]` attribute.
fn gated_suites(main: &Path, rel: &str, breaches: &mut Vec<String>) -> Result<()> {
    let text = std::fs::read_to_string(main)?;
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let decl = line.trim().strip_prefix("pub ").unwrap_or(line.trim());
        let Some(name) = decl.strip_prefix("mod ").and_then(|r| r.strip_suffix(';')) else { continue };
        let attrs = lines[..i].iter().rev().take_while(|l| l.trim_start().starts_with("#["));
        let mut gated = false;
        let mut path = None;
        for attr in attrs {
            let attr = attr.trim();
            gated |= attr.starts_with("#[cfg(") && attr.contains("feature");
            if let Some(p) = attr.strip_prefix("#[path = \"").and_then(|r| r.strip_suffix("\"]")) {
                path = Some(p.to_string());
            }
        }
        if !gated {
            continue;
        }
        let base = main.parent().unwrap();
        let file = match path {
            Some(p) => base.join(p),
            None if base.join(format!("{name}.rs")).is_file() => base.join(format!("{name}.rs")),
            None => base.join(name).join("mod.rs"),
        };
        let Ok(body) = std::fs::read_to_string(&file) else { continue };
        let head = body.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("//"));
        if !head.is_some_and(|h| h.starts_with("#![cfg(") && h.contains("feature")) {
            breaches.push(format!("{rel}/{MAIN}: suite `{name}` is feature-gated at its declaration but its file opens with no `#![cfg(feature …)]`"));
        }
    }
    Ok(())
}
