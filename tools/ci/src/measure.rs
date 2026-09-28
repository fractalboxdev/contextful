//! `contextful-ci measure` and the evaluate stage (`assurance.measure`,
//! `assurance.gate.evaluate-stage`): resolve the target ledger, run each selected entry's
//! method, read its record back and hold it to its threshold.

use anyhow::{bail, Context, Result};
use contextful_eval::ledger::{self, Ledger, Method, Tier, World};
use contextful_eval::record::{self, MEASURE_DIR_VAR};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use crate::refuse;

/// The evaluate stage's own target directory, under the workspace root, reclaimed once
/// the stage passes (`assurance.build.target-dir-per-stage`).
pub const EVALUATE_TARGET: &str = "target/evaluate";
const LOCK_FILE: &str = "spec/spec.lock.json";
const PROBE_MANIFEST: &str = "tools/probe/Cargo.toml";

/// The ledger under `root`; `None` when the tree carries none.
fn load(root: &Path) -> Result<Option<Ledger>> {
    let p = root.join(ledger::LEDGER_FILE);
    let Ok(text) = std::fs::read_to_string(&p) else { return Ok(None) };
    let parsed: Ledger = toml::from_str(&text).map_err(|e| refuse("MeasureEntryUnresolved", format!("{} does not parse: {e}", ledger::LEDGER_FILE)))?;
    Ok(Some(parsed))
}

/// The tree as the ledger resolver sees it: the lock file's clauses, each workspace
/// package's integration tests, the case sets and the probe binaries.
struct Tree {
    root: PathBuf,
    clauses: Vec<String>,
    packages: BTreeMap<String, PathBuf>,
}

impl Tree {
    fn load(root: &Path) -> Result<Tree> {
        let clauses = match std::fs::read_to_string(root.join(LOCK_FILE)) {
            Ok(text) => {
                let lock: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("parsing {LOCK_FILE}"))?;
                lock["clauses"].as_array().map(|cs| cs.iter().filter_map(|c| c["id"].as_str()).map(str::to_string).collect()).unwrap_or_default()
            }
            Err(_) => Vec::new(),
        };
        let meta = crate::metadata(root)?;
        let packages = meta["packages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| {
                let name = p["name"].as_str()?;
                let dir = Path::new(p["manifest_path"].as_str()?).parent()?.to_path_buf();
                Some((name.to_string(), dir))
            })
            .collect();
        Ok(Tree { root: root.to_path_buf(), clauses, packages })
    }
}

impl World for Tree {
    fn clause(&self, id: &str) -> bool {
        self.clauses.iter().any(|c| c == id)
    }

    /// A test resolves when its package is a workspace member and the module file its path
    /// names under `tests/integration/` defines the function.
    fn test(&self, path: &str) -> Result<(), String> {
        let (package, name) = ledger::test_target(path).ok_or("a test path is `<crate>::<module>::<function>`")?;
        let dir = self.packages.get(&package).ok_or_else(|| format!("`{package}` is no workspace package"))?;
        let segments: Vec<&str> = name.split("::").collect();
        let (function, modules) = segments.split_last().ok_or("no function")?;
        let base = dir.join("tests/integration");
        let candidates: Vec<PathBuf> = if modules.is_empty() {
            vec![base.join("main.rs")]
        } else {
            let m = modules.join("/");
            vec![base.join(format!("{m}.rs")), base.join(&m).join("mod.rs")]
        };
        let file = candidates.iter().find(|p| p.is_file()).ok_or_else(|| format!("no module file for `{}` under {}", modules.join("::"), base.display()))?;
        let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
        let defined = text.lines().any(|l| {
            let l = l.trim_start();
            let l = l.strip_prefix("pub ").unwrap_or(l);
            l.strip_prefix("fn ").is_some_and(|rest| rest.starts_with(&format!("{function}(")))
        });
        if defined {
            Ok(())
        } else {
            Err(format!("{} defines no `fn {function}`", file.strip_prefix(&self.root).unwrap_or(file).display()))
        }
    }

    fn cases(&self, path: &str) -> bool {
        path.starts_with("evals/cases/") && self.root.join(path).is_file()
    }

    fn probe(&self, name: &str) -> bool {
        std::fs::read_to_string(self.root.join(PROBE_MANIFEST)).is_ok_and(|t| t.contains(&format!("name = \"{name}\"")))
    }
}

/// Resolve every entry, refusing the run on the first unresolved set before any measure
/// runs (`assurance.measure.unresolved-entry`).
fn resolved(root: &Path) -> Result<Option<Ledger>> {
    let Some(l) = load(root)? else { return Ok(None) };
    let tree = Tree::load(root)?;
    let unresolved = l.unresolved(&tree);
    unresolved.iter().for_each(|e| eprintln!("{e}"));
    if !unresolved.is_empty() {
        return Err(refuse("MeasureEntryUnresolved", format!("{} ledger entr(ies) resolve to nothing", unresolved.len())));
    }
    Ok(Some(l))
}

/// `--status`: write `evals/ledger.md`, or with `check` refuse when the committed copy
/// differs from the rendering.
pub fn status(root: &Path, check: bool) -> Result<()> {
    let Some(l) = resolved(root)? else {
        eprintln!("measure: no {}", ledger::LEDGER_FILE);
        return Ok(());
    };
    let rendered = l.render();
    let path = root.join(ledger::STATUS_FILE);
    if check {
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        if committed != rendered {
            bail!("{} differs from `contextful-ci measure --status`; regenerate it", ledger::STATUS_FILE);
        }
        println!("measure: {} matches the ledger's {} entries", ledger::STATUS_FILE, l.entry.len());
    } else {
        std::fs::write(&path, rendered).with_context(|| format!("writing {}", ledger::STATUS_FILE))?;
        println!("measure: wrote {}", ledger::STATUS_FILE);
    }
    Ok(())
}

/// Run every entry of `tiers` whose method is known, each test in its package's
/// integration binary built under `target/evaluate`, and hold each record to its target.
pub fn run(root: &Path, tiers: &[Tier]) -> Result<()> {
    let Some(l) = resolved(root)? else {
        eprintln!("measure: no {}; nothing to measure", ledger::LEDGER_FILE);
        return Ok(());
    };
    let open = l.entry.values().filter(|e| matches!(e.method(), Some(Method::Issue(_)))).count();
    let records = root.join(EVALUATE_TARGET).join("records");
    let _ = std::fs::remove_dir_all(&records);
    std::fs::create_dir_all(&records)?;
    let (mut held, mut red, mut missing, mut reseeded) = (0usize, Vec::new(), Vec::new(), Vec::new());
    let started = Instant::now();
    for tier in tiers {
        for (id, e) in l.runnable(*tier) {
            let at = Instant::now();
            match e.method() {
                Some(Method::Test(t)) => run_test(root, t, &records)?,
                Some(Method::Cases(c)) => bail!("`{id}`: the case-set method `{c}` has no runner in this tree"),
                Some(Method::Probe(p)) => bail!("`{id}`: the probe method `{p}` has no runner in this tree"),
                _ => continue,
            }
            let secs = at.elapsed().as_secs_f64();
            match record::read(&records, id) {
                Err(err) if *tier == Tier::Gate => {
                    eprintln!("{err}");
                    missing.push(id.clone());
                }
                Err(err) => eprintln!("measure: {id} ({tier}) recorded nothing: {err} [{secs:.1} s]"),
                // A figure measured under another seed than the ledger declares replays nothing the
                // ledger names (`assurance.measure.seed-mismatch`).
                Ok(r) if e.seed.is_some_and(|s| s != r.seed) => {
                    let declared = e.seed.unwrap_or_default();
                    eprintln!("MeasureSeedMismatch: `{id}` recorded seed {}, the ledger declares {declared} [{secs:.1} s]", r.seed);
                    reseeded.push(id.clone());
                }
                Ok(r) => match e.target {
                    Some(t) if t.holds(r.value) => {
                        held += 1;
                        eprintln!("measure: {id} = {} (n = {}, seed = {}), target {t}: holds [{secs:.1} s]", r.value, r.n, r.seed);
                    }
                    Some(t) if *tier == Tier::Gate => {
                        eprintln!("measure: {id} = {} (n = {}, seed = {}), target {t}: red [{secs:.1} s]", r.value, r.n, r.seed);
                        red.push(format!("{id} = {} against {t}", r.value));
                    }
                    t => eprintln!(
                        "measure: {id} = {} ({tier}), target {}: recorded [{secs:.1} s]",
                        r.value,
                        t.map(|t| t.to_string()).unwrap_or_else(|| "none".into())
                    ),
                },
            }
        }
    }
    eprintln!(
        "measure: {held} held, {} red, {} unrecorded, {open} open, in {:.1} s",
        red.len(),
        missing.len(),
        started.elapsed().as_secs_f64()
    );
    if !missing.is_empty() {
        return Err(refuse("MeasureRecordMissing", format!("{} gate-tier method(s) wrote no record: {}", missing.len(), missing.join(", "))));
    }
    if !reseeded.is_empty() {
        return Err(refuse("MeasureSeedMismatch", format!("{} record(s) carry another seed than the ledger declares: {}", reseeded.len(), reseeded.join(", "))));
    }
    if !red.is_empty() {
        bail!("{} gate-tier target(s) missed: {}", red.len(), red.join("; "));
    }
    Ok(())
}

/// Run one integration test by its exact name, collecting records into `records`.
fn run_test(root: &Path, path: &str, records: &Path) -> Result<()> {
    let (package, name) = ledger::test_target(path).context("an unresolvable test path")?;
    let status = Command::new("cargo")
        .args(["test", "-q", "-p", &package, "--test", "integration", "--", "--exact", &name])
        .env("CARGO_TARGET_DIR", root.join(EVALUATE_TARGET))
        .env(MEASURE_DIR_VAR, records)
        .current_dir(root)
        .status()?;
    if !status.success() {
        bail!("`cargo test -p {package} --test integration -- --exact {name}` exited {}", status.code().unwrap_or(-1));
    }
    Ok(())
}

/// The evaluate stage: every gate-tier entry, then the build directory reclaimed.
pub fn evaluate(root: &Path) -> Result<()> {
    run(root, &[Tier::Gate])?;
    let _ = std::fs::remove_dir_all(root.join(EVALUATE_TARGET));
    Ok(())
}
