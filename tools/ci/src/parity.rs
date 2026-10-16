//! `contextful-ci deploy parity` — `topology.deploy.parity-divergence`'s comparison. Each
//! target's store root, synced from its object store, holds the same table parts as the
//! reference target's, byte for byte; the first part that differs, is absent or is extra,
//! in table then part-path order, raises `ParityDivergence` with the target, the table and
//! the part's path under `data/`. A table is any directory under `tables/` holding `data/`
//! or `schema.json`, keyed by its path relative to `tables/`, so a name holding `/` compares.
//! A reference holding no part, or a comparison naming no target, is refused, so a passing
//! comparison has compared at least one part.

use crate::refuse;
use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Table name to the relative paths of its parts under `tables/<table>/data/`.
type Parts = BTreeMap<String, BTreeSet<String>>;

/// Compare every `<name>=<dir>` target against the store root `reference`.
pub fn run(reference: &Path, targets: &[String]) -> Result<()> {
    if targets.is_empty() {
        bail!("deploy parity names no `--target <name>=<dir>`");
    }
    let targets = targets
        .iter()
        .map(|t| t.split_once('=').map(|(n, d)| (n.to_string(), PathBuf::from(d))).with_context(|| format!("`--target {t}` is not `<name>=<dir>`")))
        .collect::<Result<Vec<_>>>()?;
    let reference_parts = parts(reference)?;
    let total: usize = reference_parts.values().map(BTreeSet::len).sum();
    if total == 0 {
        bail!("reference `{}` holds no table part under `tables/`", reference.display());
    }
    let mut divergent = Vec::new();
    for (name, dir) in &targets {
        match first_divergence(reference, &reference_parts, dir)? {
            None => println!("parity: target `{name}` matches the reference across {total} parts; 0 divergent"),
            Some((table, part, how)) => divergent.push(format!("target `{name}` table `{table}` part `{part}`: {how}")),
        }
    }
    if divergent.is_empty() {
        return Ok(());
    }
    Err(refuse("ParityDivergence", divergent.join("; ")))
}

/// The first `(table, part, how)` at which `dir` departs from the reference, in table then
/// part-path order.
fn first_divergence(reference: &Path, reference_parts: &Parts, dir: &Path) -> Result<Option<(String, String, &'static str)>> {
    let target_parts = parts(dir)?;
    let tables: BTreeSet<&String> = reference_parts.keys().chain(target_parts.keys()).collect();
    let empty = BTreeSet::new();
    for table in tables {
        let want = reference_parts.get(table).unwrap_or(&empty);
        let got = target_parts.get(table).unwrap_or(&empty);
        for part in want.union(got) {
            let how = match (want.contains(part), got.contains(part)) {
                (true, false) => Some("absent from the target"),
                (false, true) => Some("absent from the reference"),
                _ => {
                    let a = read(&part_path(reference, table, part))?;
                    let b = read(&part_path(dir, table, part))?;
                    (a != b).then_some("bytes differ from the reference")
                }
            };
            if let Some(how) = how {
                return Ok(Some((table.clone(), part.clone(), how)));
            }
        }
    }
    Ok(None)
}

fn part_path(root: &Path, table: &str, part: &str) -> PathBuf {
    root.join("tables").join(table).join("data").join(part)
}

fn read(p: &Path) -> Result<Vec<u8>> {
    std::fs::read(p).with_context(|| format!("read {}", p.display()))
}

/// Every `*.parquet` file under each table's `data/`, keyed by the table's path under
/// `<root>/tables/`.
fn parts(root: &Path) -> Result<Parts> {
    let mut out = Parts::new();
    let tables = root.join("tables");
    if tables.is_dir() {
        find_tables(&tables, &tables, &mut out)?;
    }
    Ok(out)
}

/// Record `dir` as a table when it holds `data/` or `schema.json`, then descend into every
/// child other than a table's `data/` and `requests/`.
fn find_tables(tables: &Path, dir: &Path, out: &mut Parts) -> Result<()> {
    let data = dir.join("data");
    let is_table = dir != tables && (data.is_dir() || dir.join("schema.json").is_file());
    if is_table {
        let table = dir.strip_prefix(tables)?.to_string_lossy().replace('\\', "/");
        let mut found = BTreeSet::new();
        if data.is_dir() {
            collect(&data, &data, &mut found)?;
        }
        out.insert(table, found);
    }
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let p = entry?.path();
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if p.is_dir() && !(is_table && (name == "data" || name == "requests")) {
            find_tables(tables, &p, out)?;
        }
    }
    Ok(())
}

fn collect(base: &Path, dir: &Path, out: &mut BTreeSet<String>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let p = entry?.path();
        if p.is_dir() {
            collect(base, &p, out)?;
        } else if p.extension().is_some_and(|x| x == "parquet") {
            out.insert(p.strip_prefix(base)?.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}
