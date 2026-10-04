//! `contextful-ci measure` and the evaluate stage (`assurance.measure`,
//! `assurance.gate.evaluate-stage`): resolve the target ledger, run each selected entry's
//! method, read its record back and hold it to its threshold.

use anyhow::{bail, Context, Result};
use contextful_eval::ledger::{self, Ledger, Method, Tier, World};
use contextful_eval::record::{self, Record, MEASURE_DIR_VAR};
use contextful_eval::trend::{self, Figure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
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
    measure(root, tiers)?.1
}

/// [`run`], every runnable entry measured before the outcome: the ids of the entries that
/// failed — a test that failed, a gate-tier record missing, reseeded or red — and the
/// outcome, which carries the first failed test's exit ahead of the refusals.
fn measure(root: &Path, tiers: &[Tier]) -> Result<(BTreeSet<String>, Result<()>)> {
    let records = root.join(EVALUATE_TARGET).join("records");
    let _ = std::fs::remove_dir_all(&records);
    let Some(l) = resolved(root)? else {
        eprintln!("measure: no {}; nothing to measure", ledger::LEDGER_FILE);
        return Ok((BTreeSet::new(), Ok(())));
    };
    let open = l.entry.values().filter(|e| matches!(e.method(), Some(Method::Issue(_)))).count();
    std::fs::create_dir_all(&records)?;
    let (mut held, mut red, mut missing, mut reseeded) = (0usize, Vec::new(), Vec::new(), Vec::new());
    let (mut failed, mut exited) = (BTreeSet::new(), None);
    let started = Instant::now();
    for tier in tiers {
        for (id, e) in l.runnable(*tier) {
            let at = Instant::now();
            match e.method() {
                Some(Method::Test(t)) => {
                    if let Err(err) = run_test(root, t, &records) {
                        eprintln!("measure: {id} ({tier}) failed: {err}");
                        if *tier == Tier::Trend {
                            let _ = std::fs::remove_file(record::path(&records, id));
                        } else {
                            failed.insert(id.clone());
                            exited.get_or_insert(err);
                        }
                        continue;
                    }
                }
                Some(Method::Cases(c)) => bail!("`{id}`: the case-set method `{c}` has no runner in this tree"),
                Some(Method::Probe(p)) => {
                    if let Err(err) = run_probe(root, p, &records) {
                        eprintln!("measure: {id} ({tier}) failed: {err}");
                        if *tier == Tier::Trend {
                            let _ = std::fs::remove_file(record::path(&records, id));
                        } else {
                            failed.insert(id.clone());
                            exited.get_or_insert(err);
                        }
                        continue;
                    }
                }
                _ => continue,
            }
            let secs = at.elapsed().as_secs_f64();
            match record::read(&records, id) {
                Err(err) if *tier == Tier::Gate => {
                    eprintln!("{err}");
                    missing.push(id.clone());
                    failed.insert(id.clone());
                }
                Err(err) => eprintln!("measure: {id} ({tier}) recorded nothing: {err} [{secs:.1} s]"),
                // A figure measured under another seed than the ledger declares replays nothing the
                // ledger names (`assurance.measure.seed-mismatch`).
                Ok(r) if e.seed.is_some_and(|s| s != r.seed) => {
                    let declared = e.seed.unwrap_or_default();
                    eprintln!("MeasureSeedMismatch: `{id}` recorded seed {}, the ledger declares {declared} [{secs:.1} s]", r.seed);
                    if *tier == Tier::Trend {
                        let _ = std::fs::remove_file(record::path(&records, id));
                    } else {
                        reseeded.push(id.clone());
                        failed.insert(id.clone());
                    }
                }
                Ok(r) => match e.target {
                    Some(t) if t.holds(r.value) => {
                        held += 1;
                        eprintln!("measure: {id} = {} (n = {}, seed = {}), target {t}: holds [{secs:.1} s]", r.value, r.n, r.seed);
                    }
                    Some(t) if *tier == Tier::Gate => {
                        eprintln!("measure: {id} = {} (n = {}, seed = {}), target {t}: red [{secs:.1} s]", r.value, r.n, r.seed);
                        red.push(format!("{id} = {} against {t}", r.value));
                        failed.insert(id.clone());
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
    let broke = failed.len() - red.len() - missing.len() - reseeded.len();
    eprintln!(
        "measure: {held} held, {} red, {} unrecorded, {open} open, {broke} failed, in {:.1} s",
        red.len(),
        missing.len(),
        started.elapsed().as_secs_f64()
    );
    let outcome = if let Some(err) = exited {
        Err(err)
    } else if !missing.is_empty() {
        Err(refuse("MeasureRecordMissing", format!("{} gate-tier method(s) wrote no record: {}", missing.len(), missing.join(", "))))
    } else if !reseeded.is_empty() {
        Err(refuse("MeasureSeedMismatch", format!("{} record(s) carry another seed than the ledger declares: {}", reseeded.len(), reseeded.join(", "))))
    } else if !red.is_empty() {
        Err(anyhow::anyhow!("{} gate-tier target(s) missed: {}", red.len(), red.join("; ")))
    } else {
        Ok(())
    };
    Ok((failed, outcome))
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
        return Err(crate::exited(format!("cargo test -p {package} --test integration -- --exact {name}"), status));
    }
    Ok(())
}

/// Run one probe binary in its own process, collecting records into `records`.
fn run_probe(root: &Path, name: &str, records: &Path) -> Result<()> {
    let status = Command::new("cargo")
        .args(["run", "--locked", "-q", "--manifest-path", PROBE_MANIFEST, "--bin", name])
        .env("CARGO_TARGET_DIR", root.join(EVALUATE_TARGET))
        .env(MEASURE_DIR_VAR, records)
        .current_dir(root)
        .status()?;
    if !status.success() {
        return Err(crate::exited(format!("cargo run --manifest-path {PROBE_MANIFEST} --bin {name}"), status));
    }
    Ok(())
}

/// One annotation in a default-branch report; the baseline names its earlier run.
#[derive(Debug, Serialize, Deserialize)]
struct TrendAnnotation {
    id: String,
    baseline: f64,
    current: f64,
    worse_percent: f64,
    baseline_commit: String,
    baseline_run_id: u64,
    annotation: String,
}

/// One JSON line of `refs/notes/measures`; earlier reports lack `annotations`.
#[derive(Debug, Serialize, Deserialize)]
struct RunReport {
    commit: String,
    run_id: u64,
    run_attempt: u64,
    exit_code: i32,
    records: Vec<Record>,
    #[serde(default)]
    annotations: Vec<TrendAnnotation>,
}

fn git_text(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").args(args).current_dir(root).output()?;
    if !output.status.success() {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim());
    }
    String::from_utf8(output.stdout).context("Git output is UTF-8")
}

/// Reports on the measured commit and its first-parent ancestors, newest run first.
fn earlier_reports(root: &Path, commit: &str) -> Result<Vec<RunReport>> {
    let exists = Command::new("git").args(["show-ref", "--verify", "--quiet", "refs/notes/measures"]).current_dir(root).status()?;
    if exists.code() == Some(1) {
        return Ok(Vec::new());
    }
    if !exists.success() {
        bail!("reading refs/notes/measures failed: {exists}");
    }
    let history = git_text(root, &["log", "--first-parent", "--notes=measures", "--format=%H%x00%N%x00", commit])?;
    let mut reports = Vec::new();
    let mut fields = history.split('\0');
    while let (Some(ancestor), Some(note)) = (fields.next(), fields.next()) {
        let ancestor = ancestor.trim();
        if ancestor == commit || note.trim().is_empty() {
            continue;
        }
        for line in note.lines().filter(|line| !line.trim().is_empty()).rev() {
            reports.push(serde_json::from_str(line).with_context(|| format!("parsing measure note on {ancestor}"))?);
        }
    }
    Ok(reports)
}

/// Write one JSON-line run report, annotating a trend only against matching earlier
/// successful history. The measure's exit status remains a separate workflow verdict.
pub fn report(root: &Path, commit: &str, run_id: u64, run_attempt: u64, exit_code: i32, out: &Path) -> Result<()> {
    let dir = root.join(EVALUATE_TARGET).join("records");
    let mut records = Vec::new();
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            for entry in entries {
                let path = entry?.path();
                if path.extension().is_some_and(|ext| ext == "json") {
                    let id = path.file_stem().and_then(|s| s.to_str()).context("a measure record filename is UTF-8")?;
                    records.push(record::read(&dir, id)?);
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| dir.display().to_string()),
    }
    records.sort_by(|a, b| a.id.cmp(&b.id));
    let history = earlier_reports(root, commit)?;
    let ledger = load(root)?;
    let mut annotations = Vec::new();
    for current in &records {
        let Some(entry) = ledger.as_ref().and_then(|l| l.entry.get(&current.id)) else { continue };
        if entry.tier != Tier::Trend {
            continue;
        }
        let direction = entry.direction.context("a trend-tier entry declares a direction")?;
        let baseline = history.iter().filter(|report| report.exit_code == 0).find_map(|report| {
            report.records.iter().find(|old| old.id == current.id && old.seed == current.seed && old.run == current.run).map(|old| (report, old))
        });
        let Some((past, old)) = baseline else { continue };
        let comparison = trend::compare(
            &Figure { value: current.value, runner: current.run.clone() },
            &Figure { value: old.value, runner: old.run.clone() },
            direction,
        );
        if let trend::Comparison::Annotated { worse_percent } = comparison {
            annotations.push(TrendAnnotation {
                id: current.id.clone(),
                baseline: old.value,
                current: current.value,
                worse_percent,
                baseline_commit: past.commit.clone(),
                baseline_run_id: past.run_id,
                annotation: comparison.annotation().unwrap_or_default(),
            });
        }
    }
    let report = RunReport { commit: commit.to_string(), run_id, run_attempt, exit_code, records, annotations };
    let path = root.join(out);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, format!("{}\n", serde_json::to_string(&report)?)).with_context(|| path.display().to_string())?;
    Ok(())
}

/// Append one report to the remote notes ref. Each attempt starts from the remote ref,
/// so a competing writer's accepted note remains in the next push.
pub fn publish(root: &Path, commit: &str, report: &Path, remote: &str) -> Result<()> {
    let body = std::fs::read_to_string(root.join(report)).with_context(|| report.display().to_string())?;
    let parsed: RunReport = serde_json::from_str(body.trim()).context("measure report is one JSON object")?;
    if parsed.commit != commit {
        bail!("measure report names commit {}, not {commit}", parsed.commit);
    }
    for attempt in 1..=3 {
        let listed = git_text(root, &["ls-remote", "--refs", remote, "refs/notes/measures"])?;
        if listed.trim().is_empty() {
            let deletion = Command::new("git").args(["update-ref", "-d", "refs/notes/measures"]).current_dir(root).output()?;
            if !deletion.status.success() && deletion.status.code() != Some(1) {
                bail!("clearing local measures notes: {}", String::from_utf8_lossy(&deletion.stderr).trim());
            }
        } else {
            git_text(root, &["fetch", remote, "+refs/notes/measures:refs/notes/measures"])?;
        }
        let existing = Command::new("git").args(["notes", "--ref=measures", "show", commit]).current_dir(root).output()?;
        if existing.status.success() && String::from_utf8_lossy(&existing.stdout).lines().any(|line| line == body.trim()) {
            return Ok(());
        }
        git_text(root, &["notes", "--ref=measures", "append", "-F", report.to_str().context("report path is UTF-8")?, commit])?;
        let push = Command::new("git").args(["push", remote, "refs/notes/measures"]).current_dir(root).output()?;
        if push.status.success() {
            return Ok(());
        }
        if attempt == 3 {
            bail!("pushing refs/notes/measures after {attempt} attempts: {}", String::from_utf8_lossy(&push.stderr).trim());
        }
    }
    unreachable!()
}

/// The native case set, scored in the deterministic tier against its floors and baseline.
const NATIVE_CASES: &str = "evals/cases/native.jsonl";
/// The clause owning the ledger entries that run the native case set.
const NATIVE_GATE: &str = "assurance.baseline.native-gate";

/// The evaluate stage (`assurance.gate.evaluate-stage`): every gate-tier entry, the native
/// case set among them, then the floor and baseline verdicts and the build directory
/// reclaimed. A tree carrying the native case set with no gate-tier entry running it fails.
pub fn evaluate(root: &Path) -> Result<()> {
    let native: Vec<String> = load(root)?
        .map(|l| {
            l.entry
                .iter()
                .filter(|(_, e)| e.tier == Tier::Gate && e.clause == NATIVE_GATE)
                .filter(|(_, e)| matches!(e.method(), Some(Method::Test(_))))
                .map(|(id, _)| id.clone())
                .collect()
        })
        .unwrap_or_default();
    if root.join(NATIVE_CASES).is_file() && native.is_empty() {
        bail!("the tree carries {NATIVE_CASES}, and no gate-tier ledger entry owned by `{NATIVE_GATE}` runs it");
    }
    let (failed, outcome) = measure(root, &[Tier::Gate])?;
    let baseline_red = native.iter().any(|id| failed.contains(id));
    eprintln!("evaluate: floor verdict {} over the gate-tier targets", if outcome.is_err() { "red" } else { "held" });
    if !native.is_empty() {
        eprintln!("evaluate: baseline verdict {} for {NATIVE_CASES} ({})", if baseline_red { "red" } else { "held" }, native.join(", "));
    }
    outcome?;
    let _ = std::fs::remove_dir_all(root.join(EVALUATE_TARGET));
    Ok(())
}
