//! The gate's stage sequence (`assurance.gate.stage-sequence`): which stages a run selects,
//! the outputs one stage leaves for a later one, the report every stage prints, and the
//! pins, toolchain, schema, connectors, surfaces and formal stages.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use crate::{refuse, run, tracked, Exited};

/// Gate stages in run order. FlareDispatch dispatches each as its own check.
pub const STAGES: [&str; 13] = [
    "pins",
    "toolchain",
    "schema",
    "test-first",
    "workspace",
    "acceptance",
    "evaluate",
    "features",
    "crate-graph",
    "connectors",
    "surfaces",
    "formal",
    "budget",
];

/// Where a stage leaves an output a later stage reads, under the workspace root.
pub const GATE_DIR: &str = "target/gate";
/// The pins stage's record of every pinned identity it resolved.
pub const PINS_RECORD: &str = "target/gate/pins.json";
/// The toolchain stage's record of the environment it provisioned, one `KEY=VALUE` per line.
pub const TOOLCHAIN_RECORD: &str = "target/gate/toolchain.env";

/// Each stage that reads another stage's output: the stage, its predecessor and the output.
pub const READS: [(&str, &str, &str); 2] = [("toolchain", "pins", PINS_RECORD), ("formal", "toolchain", TOOLCHAIN_RECORD)];

/// The stages a run executes, in canonical order: every stage when none is named, else the
/// named ones, and with `predecessors` every stage whose output a selected stage reads.
/// A selected stage whose predecessor is unselected and whose input is absent refuses the
/// run before any stage starts (`assurance.gate.stage-subset`).
pub fn select(root: &Path, named: &[String], predecessors: bool) -> Result<Vec<&'static str>> {
    let mut chosen: Vec<&str> = if named.is_empty() { STAGES.to_vec() } else { STAGES.iter().copied().filter(|s| named.iter().any(|n| n == s)).collect() };
    if predecessors {
        // READS lists each edge before the edge its predecessor sits on, read backwards.
        for (stage, before, _) in READS.iter().rev() {
            if chosen.contains(stage) && !chosen.contains(before) {
                chosen.push(before);
            }
        }
        chosen = STAGES.iter().copied().filter(|s| chosen.contains(s)).collect();
    }
    for (stage, before, output) in READS {
        if chosen.contains(&stage) && !chosen.contains(&before) && !root.join(output).is_file() {
            return Err(refuse(
                "StagePredecessorMissing",
                format!("stage `{stage}` reads {output}, which stage `{before}` writes; select `{before}` too, or pass --predecessors"),
            ));
        }
    }
    Ok(chosen)
}

// ---------------------------------------------------------------- report

/// The environment and memory state at a stage's start.
pub struct Mark {
    env: BTreeMap<String, String>,
    started: Instant,
    peak: Peak,
}

pub fn mark() -> Mark {
    Mark { env: std::env::vars().collect(), started: Instant::now(), peak: Peak::start() }
}

/// Where a stage's own memory peak reads from.
enum Peak {
    /// cgroup v2 `memory.peak`, reset through this descriptor, which alone then reads the
    /// peak since the reset.
    Reset(std::fs::File),
    /// cgroup v1 `memory.max_usage_in_bytes`, zeroed at the stage's start.
    Zeroed,
    /// A high-water mark no stage resets — the cgroup's lifetime peak, else the largest
    /// resident set of any finished child — and its value at the stage's start. A rise
    /// belongs to this stage; without one, that value bounds this stage's peak.
    Run(Option<u64>),
}

const PEAK_V2: &str = "/sys/fs/cgroup/memory.peak";
const PEAK_V1: &str = "/sys/fs/cgroup/memory/memory.max_usage_in_bytes";

impl Peak {
    fn start() -> Peak {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().read(true).write(true).open(PEAK_V2) {
            if f.write_all(b"reset\n").is_ok() {
                return Peak::Reset(f);
            }
        }
        if std::fs::write(PEAK_V1, "0").is_ok() {
            return Peak::Zeroed;
        }
        Peak::Run(run_peak())
    }

    /// The stage's peak: its own in bytes, or the bound an earlier stage's peak sets on it.
    fn read(&mut self) -> String {
        use std::io::{Read, Seek};
        let own = match self {
            Peak::Reset(f) => {
                let mut text = String::new();
                f.rewind().ok().and_then(|()| f.read_to_string(&mut text).ok()).and_then(|_| text.trim().parse().ok())
            }
            Peak::Zeroed => std::fs::read_to_string(PEAK_V1).ok().and_then(|t| t.trim().parse().ok()),
            Peak::Run(before) => match (run_peak(), *before) {
                (Some(now), Some(b)) if now <= b => return format!("at most {b} bytes, an earlier stage's peak"),
                (now, _) => now,
            },
        };
        own.map_or("unmeasured".to_string(), |b| format!("{b} bytes"))
    }
}

/// The cgroup's lifetime peak, else the largest resident set any finished child reached.
fn run_peak() -> Option<u64> {
    [PEAK_V2, PEAK_V1].iter().find_map(|p| std::fs::read_to_string(p).ok()).and_then(|t| t.trim().parse().ok()).or_else(children_max_rss)
}

/// Print the stage's report (`assurance.gate.stage-reports`): its verdict and duration, the
/// environment it leaves, the memory limit, the peak and the memory event counts. A failing
/// stage prints the exit code it propagates and its diagnostics.
pub fn report(stage: &str, mark: &mut Mark, outcome: &Result<()>) {
    let secs = mark.started.elapsed().as_secs_f64();
    match outcome {
        Ok(()) => eprintln!("report: stage `{stage}` passed in {secs:.1} s"),
        Err(e) => eprintln!("report: stage `{stage}` failed in {secs:.1} s, exit {}", crate::exit_code(e)),
    }
    let now: BTreeMap<String, String> = std::env::vars().collect();
    let mut left: Vec<String> = now.iter().filter(|(k, v)| mark.env.get(*k) != Some(*v)).map(|(k, v)| format!("{k}={v}")).collect();
    left.extend(mark.env.keys().filter(|k| !now.contains_key(*k)).map(|k| format!("{k} unset")));
    if left.is_empty() {
        eprintln!("report: environment left unchanged");
    } else {
        eprintln!("report: environment left {}", left.join(" "));
    }
    let limit = memory_limit().map_or("none".to_string(), |b| format!("{b} bytes"));
    let peak = mark.peak.read();
    let events = memory_events().map_or("none recorded".to_string(), |ev| ev.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", "));
    eprintln!("report: memory limit {limit}, peak {peak}, events {events}");
    if let Err(e) = outcome {
        if let Some(signal) = e.downcast_ref::<Exited>().and_then(|x| x.signal) {
            eprintln!("report: a child process was killed by signal {signal}; read the events above for an out-of-memory kill");
        }
        for (i, cause) in e.chain().enumerate() {
            eprintln!("report: diagnostic {}: {cause}", i + 1);
        }
    }
}

/// The cgroup memory ceiling in bytes, under cgroup v2 or v1; `None` when the process
/// runs under none.
fn memory_limit() -> Option<u64> {
    ["/sys/fs/cgroup/memory.max", "/sys/fs/cgroup/memory/memory.limit_in_bytes"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| t.trim().parse().ok())
}

#[cfg(unix)]
fn children_max_rss() -> Option<u64> {
    // SAFETY: getrusage writes one rusage into the zeroed struct it is handed.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, &mut usage) } != 0 {
        return None;
    }
    let raw = u64::try_from(usage.ru_maxrss).ok()?;
    // Linux reports KiB, macOS bytes.
    Some(if cfg!(target_os = "macos") { raw } else { raw * 1024 })
}

#[cfg(not(unix))]
fn children_max_rss() -> Option<u64> {
    None
}

/// The cgroup's memory event counters: `memory.events` under v2, the `oom_kill` count of
/// `memory.oom_control` under v1.
fn memory_events() -> Option<Vec<(String, u64)>> {
    let parse = |text: String| -> Vec<(String, u64)> {
        text.lines()
            .filter_map(|l| {
                let (k, v) = l.split_once(' ')?;
                Some((k.to_string(), v.trim().parse().ok()?))
            })
            .collect()
    };
    if let Ok(t) = std::fs::read_to_string("/sys/fs/cgroup/memory.events") {
        return Some(parse(t));
    }
    let t = std::fs::read_to_string("/sys/fs/cgroup/memory/memory.oom_control").ok()?;
    Some(parse(t).into_iter().filter(|(k, _)| k == "oom_kill").collect())
}

// ---------------------------------------------------------------- pins

/// The pins stage (`assurance.gate.pins-stage`): before any compilation, read every pinned
/// identity a run depends on — each Lean package's toolchain, each Lean dependency
/// revision and every crate the lock file names — refuse one that floats,
/// fetch the locked crates, and record them all for the toolchain stage.
pub fn pins(root: &Path) -> Result<()> {
    let files = tracked(root)?;
    let mut record = serde_json::Map::new();

    let mut lean: BTreeMap<String, String> = BTreeMap::new();
    for f in files.iter().filter(|f| f.starts_with("formal/") && f.ends_with("/lean-toolchain")) {
        let pin = std::fs::read_to_string(root.join(f)).with_context(|| format!("reading {f}"))?.trim().to_string();
        if !exact_lean(&pin) {
            bail!("{f} pins `{pin}`, which names no exact release; pin `leanprover/lean4:v<major>.<minor>.<patch>`");
        }
        lean.insert(f.clone(), pin);
    }
    let distinct: Vec<&String> = lean.values().collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    if distinct.len() > 1 {
        bail!("the Lean packages pin {} toolchains: {}", distinct.len(), lean.iter().map(|(f, p)| format!("{f} = {p}")).collect::<Vec<_>>().join(", "));
    }
    if let Some(pin) = distinct.first() {
        record.insert("lean".into(), serde_json::Value::String((*pin).clone()));
    }

    let mut lake = Vec::new();
    for f in files.iter().filter(|f| f.starts_with("formal/") && f.ends_with("lake-manifest.json")) {
        let text = std::fs::read_to_string(root.join(f)).with_context(|| format!("reading {f}"))?;
        let manifest: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("parsing {f}"))?;
        for p in manifest["packages"].as_array().into_iter().flatten() {
            let name = p["name"].as_str().unwrap_or("?");
            match p["rev"].as_str() {
                Some(rev) if rev.len() == 40 && rev.chars().all(|c| c.is_ascii_hexdigit()) => lake.push(serde_json::Value::String(format!("{name}@{rev}"))),
                Some(rev) => bail!("{f}: `{name}` resolves to `{rev}`, which names no commit"),
                None if p["type"].as_str() == Some("path") => {}
                None => bail!("{f}: `{name}` carries no revision"),
            }
        }
    }
    record.insert("lake".into(), serde_json::Value::Array(lake));

    if files.iter().any(|f| f == "Cargo.lock") {
        run(root, "cargo", &["fetch", "--locked"])?;
        let lock = std::fs::read(root.join("Cargo.lock"))?;
        record.insert("cargo-lock".into(), serde_json::Value::String(format!("sha256:{:x}", Sha256::digest(&lock))));
    }

    std::fs::create_dir_all(root.join(GATE_DIR))?;
    let text = serde_json::to_string_pretty(&serde_json::Value::Object(record))?;
    std::fs::write(root.join(PINS_RECORD), format!("{text}\n"))?;
    eprintln!("pins: resolved {} Lean toolchain(s), and recorded {PINS_RECORD}", lean.len());
    Ok(())
}

/// `leanprover/lean4:v<major>.<minor>.<patch>`, optionally with a `-rc<n>` suffix.
fn exact_lean(pin: &str) -> bool {
    let Some(version) = pin.strip_prefix("leanprover/lean4:v") else { return false };
    let core = version.split_once("-rc").map_or(version, |(c, rc)| if rc.chars().all(|c| c.is_ascii_digit()) && !rc.is_empty() { c } else { "" });
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

// ---------------------------------------------------------------- toolchain

/// The toolchain stage: provision the Lean toolchain the pins record names and the
/// WebAssembly target, then record the environment it set for a later invocation.
pub fn toolchain(root: &Path) -> Result<()> {
    let text = std::fs::read_to_string(root.join(PINS_RECORD)).with_context(|| format!("reading {PINS_RECORD}"))?;
    let record: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("parsing {PINS_RECORD}"))?;
    let before: BTreeMap<String, String> = std::env::vars().collect();
    if let Some(pin) = record["lean"].as_str() {
        crate::provision_lean_pin(root, pin)?;
    }
    crate::provision_wasm(root)?;
    let set: Vec<String> = std::env::vars().filter(|(k, v)| before.get(k) != Some(v)).map(|(k, v)| format!("{k}={v}")).collect();
    std::fs::create_dir_all(root.join(GATE_DIR))?;
    std::fs::write(root.join(TOOLCHAIN_RECORD), set.iter().map(|l| format!("{l}\n")).collect::<String>())?;
    eprintln!("toolchain: recorded {} variable(s) in {TOOLCHAIN_RECORD}", set.len());
    Ok(())
}

/// Apply the toolchain stage's recorded environment to this process.
pub fn apply_toolchain(root: &Path) -> Result<()> {
    let text = std::fs::read_to_string(root.join(TOOLCHAIN_RECORD)).with_context(|| format!("reading {TOOLCHAIN_RECORD}"))?;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let (k, v) = line.split_once('=').with_context(|| format!("{TOOLCHAIN_RECORD}: `{line}` is no KEY=VALUE"))?;
        std::env::set_var(k, v);
    }
    Ok(())
}

// ---------------------------------------------------------------- schema

struct DerivedArtifact {
    path: String,
    check: String,
}

/// A declaration names the output and the command that proves its currency.
fn declarations(root: &Path) -> Result<Option<Vec<DerivedArtifact>>> {
    let manifest = root.join("spec/derived.toml");
    if !manifest.is_file() {
        if root.join("spec/81-engineering.md").is_file() {
            bail!("schema: spec/derived.toml is absent");
        }
        return Ok(None);
    }
    let contents = std::fs::read_to_string(&manifest)?;
    let parsed: toml::Value = contents.parse().context("parse spec/derived.toml")?;
    let entries = parsed.get("artifact").and_then(toml::Value::as_array).context("spec/derived.toml needs [[artifact]] entries")?;
    let mut out = Vec::new();
    let mut declared_paths = BTreeSet::new();
    for entry in entries {
        let path = entry.get("path").and_then(toml::Value::as_str).context("derived artifact needs path")?;
        let check = entry.get("check").and_then(toml::Value::as_str).context("derived artifact needs check")?;
        let normalized = Path::new(path);
        if normalized.is_absolute() || normalized.components().any(|part| !matches!(part, std::path::Component::Normal(_))) {
            bail!("spec/derived.toml: artifact path `{path}` must stay below the repository root");
        }
        if !["contextful-spec extract", "contextful-spec state", "contextful-ci measure --status"].contains(&check) {
            bail!("spec/derived.toml: unknown check `{check}` for `{path}`");
        }
        if !declared_paths.insert(path) {
            bail!("spec/derived.toml: artifact `{path}` is declared twice");
        }
        out.push(DerivedArtifact { path: path.to_string(), check: check.to_string() });
    }
    Ok(Some(out))
}

fn derived(root: &Path, declared: Option<&[DerivedArtifact]>) -> Result<Vec<String>> {
    let fallback = ["spec/spec.lock.json", "spec/status.md", "spec/targets.md", "spec/cards/", "evals/ledger.md"];
    let paths: Vec<&str> = match declared {
        Some(entries) => entries.iter().map(|item| item.path.as_str()).collect(),
        None => fallback.to_vec(),
    };
    let mut out = Vec::new();
    for path in paths {
        if path.ends_with('/') {
            let dir = root.join(path);
            if !dir.is_dir() {
                if declared.is_some() {
                    bail!("schema: declared artifact directory `{path}` is absent");
                }
                continue;
            }
            for entry in std::fs::read_dir(dir)? {
                let entry = entry?;
                if entry.file_type()?.is_file() {
                    out.push(format!("{path}{}", entry.file_name().to_string_lossy()));
                }
            }
        } else {
            if declared.is_some() || root.join(path).is_file() {
                out.push(path.to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// The schema stage's regeneration (`assurance.gate.schema-stage`): export the measured
/// commit into a scratch directory, regenerate every derived artifact there, and compare
/// each byte for byte against the committed copy.
pub fn regenerate(root: &Path) -> Result<()> {
    if !root.join("spec").is_dir() {
        eprintln!("schema: no spec/ to regenerate");
        return Ok(());
    }
    let scratch = root.join(GATE_DIR).join("schema");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch)?;
    let archive = Command::new("git").args(["archive", "--format=tar", "HEAD"]).current_dir(root).output().context("running git archive")?;
    if !archive.status.success() {
        return Err(crate::exited_output("git archive HEAD", &archive));
    }
    let mut tar = Command::new("tar").args(["-x", "-C"]).arg(&scratch).stdin(std::process::Stdio::piped()).spawn().context("running tar")?;
    std::io::Write::write_all(tar.stdin.as_mut().context("tar stdin")?, &archive.stdout)?;
    let unpacked = tar.wait()?;
    if !unpacked.success() {
        return Err(crate::exited("tar -x".into(), unpacked).context("tar could not unpack the exported tree"));
    }
    let declared = declarations(&scratch)?;
    let committed: BTreeMap<String, Option<Vec<u8>>> = derived(&scratch, declared.as_deref())?.into_iter().map(|p| { let bytes = std::fs::read(scratch.join(&p)).ok(); (p, bytes) }).collect();
    let scratch_arg = scratch.to_string_lossy().to_string();
    let checks: BTreeSet<&str> = declared.as_ref().map(|entries| entries.iter().map(|item| item.check.as_str()).collect()).unwrap_or_else(|| ["contextful-spec extract", "contextful-spec state", "contextful-ci measure --status"].into_iter().collect());
    for (check, verb) in [("contextful-spec extract", "extract"), ("contextful-spec state", "state")] {
        if checks.contains(check) {
            run(root, "cargo", &["run", "--locked", "-q", "-p", "contextful-spec", "--", "--root", &scratch_arg, verb])?;
        }
    }
    if checks.contains("contextful-ci measure --status") && scratch.join(contextful_eval::ledger::LEDGER_FILE).is_file() {
        crate::measure::status(&scratch, false)?;
    }
    let regenerated: BTreeMap<String, Option<Vec<u8>>> = derived(&scratch, declared.as_deref())?.into_iter().map(|p| { let bytes = std::fs::read(scratch.join(&p)).ok(); (p, bytes) }).collect();
    let mut stale: Vec<String> = Vec::new();
    for path in committed.keys().chain(regenerated.keys()).collect::<std::collections::BTreeSet<_>>() {
        match (committed.get(path), regenerated.get(path)) {
            (Some(Some(a)), Some(Some(b))) if a == b => {}
            (Some(None), Some(None)) => stale.push(format!("{path} is declared but absent")),
            (Some(_), Some(_)) => stale.push(format!("{path} differs from its regeneration")),
            (None, Some(_)) => stale.push(format!("{path} is regenerated but not committed")),
            (Some(_), None) => stale.push(format!("{path} is committed but no longer regenerated")),
            (None, None) => {}
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    if !stale.is_empty() {
        stale.iter().for_each(|s| eprintln!("schema: {s}"));
        bail!("{} derived artifact(s) differ from regeneration; run `contextful-spec extract`, `state` and `contextful-ci measure --status`", stale.len());
    }
    eprintln!("schema: {} derived artifact(s) match their regeneration byte for byte", committed.len());
    Ok(())
}

// ---------------------------------------------------------------- connectors

/// The packages carrying the connectors and the guest-connector host.
const CONNECTOR_PACKAGES: [&str; 2] = ["contextful-connectors", "contextful-wasm"];

/// The connectors stage: each connector package's suite with every feature on, so the
/// object and drive sources and the interpreted host target compile and run.
pub fn connectors(root: &Path) -> Result<()> {
    let members = crate::workspace_packages(root)?;
    let present: Vec<&str> = CONNECTOR_PACKAGES.iter().copied().filter(|p| members.iter().any(|m| m == p)).collect();
    if present.is_empty() {
        eprintln!("connectors: no connector package in this workspace");
        return Ok(());
    }
    for package in present {
        run(root, "cargo", &["test", "--locked", "-p", package, "--all-features"])?;
    }
    Ok(())
}

// ---------------------------------------------------------------- surfaces

/// The scripts a surface runs, in order: typecheck, unit tests, framework build.
const SURFACE_SCRIPTS: [&str; 3] = ["typecheck", "test", "build"];

/// Every TypeScript surface: a directory directly under `apps/` or `packages/` holding a
/// `package.json`.
fn surfaces(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for parent in ["apps", "packages"] {
        let Ok(dir) = std::fs::read_dir(root.join(parent)) else { continue };
        for e in dir.flatten() {
            if e.file_type().is_ok_and(|kind| kind.is_dir()) && e.path().join("package.json").is_file() {
                out.push(format!("{parent}/{}", e.file_name().to_string_lossy()));
            }
        }
    }
    out.sort();
    out
}

fn native_surface_tests(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for folder in ["test", "tests"] {
        let mut pending = vec![dir.join(folder)];
        while let Some(path) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(path) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(kind) = entry.file_type() else { continue };
                if kind.is_dir() {
                    pending.push(path);
                } else if kind.is_file() && path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.ends_with(".test.ts")) {
                    out.push(path);
                }
            }
        }
    }
    out.sort();
    out
}

#[path = "../../spec/src/native_ts.rs"]
mod native_ts;

fn pinned_native_names(source: &str) -> Vec<(String, bool)> {
    let mut names = Vec::new();
    let mut tagged = false;
    for line in source.lines() {
        let line = line.trim();
        if native_ts::tag(line).is_some() {
            tagged = true;
            continue;
        }
        if !tagged || line.is_empty() || line.starts_with("//") { continue }
        if let Some(call) = native_ts::test_call(line) { names.push((call.title.to_string(), call.disabled)); }
        tagged = false;
    }
    names
}

/// The TypeScript surfaces stage (`assurance.gate.typescript-surfaces`): install each
/// surface from its lock file, then run each check it declares a script for; a failing
/// check raises `SurfaceCheckFailed` (`assurance.gate.surface-check-failed`).
pub fn typescript(root: &Path) -> Result<()> {
    let all = surfaces(root);
    if all.is_empty() {
        eprintln!("surfaces: no TypeScript surface under apps/ or packages/");
        return Ok(());
    }
    for surface in &all {
        let dir = root.join(surface);
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("package.json"))?).with_context(|| format!("parsing {surface}/package.json"))?;
        let declared: Vec<&str> = SURFACE_SCRIPTS.iter().copied().filter(|s| manifest["scripts"][*s].is_string()).collect();
        let skipped: Vec<&str> = SURFACE_SCRIPTS.iter().copied().filter(|s| !declared.contains(s)).collect();
        if !skipped.is_empty() {
            eprintln!("surfaces: {surface} declares no {} script", skipped.join(" or "));
        }
        if !declared.is_empty() {
            let pm = manager(&dir);
            let install: &[&str] = match pm {
                "npm" => &["ci"],
                _ => &["install", "--frozen-lockfile"],
            };
            let status = Command::new(pm).args(install).current_dir(&dir).status().with_context(|| format!("running {pm} in {surface}"))?;
            if !status.success() {
                return Err(crate::exited(format!("{pm} {} in {surface}", install.join(" ")), status));
            }
            for script in declared {
                eprintln!("surfaces: {surface} {script}");
                let status = Command::new(pm).args(["run", script]).current_dir(&dir).status().with_context(|| format!("running {pm} in {surface}"))?;
                if !status.success() {
                    return Err(refuse(
                        "SurfaceCheckFailed",
                        format!("surface {surface}: script `{script}` exited {}", status.code().map_or("by signal".into(), |c| c.to_string())),
                    ));
                }
            }
        }
        for test in native_surface_tests(&dir) {
            let relative = test.strip_prefix(&dir).unwrap();
            let output = Command::new("node").args(["--experimental-strip-types", "--test", "--test-reporter=tap"]).arg(relative)
                .current_dir(&dir).output().with_context(|| format!("running node test in {surface}"))?;
            let tap = String::from_utf8_lossy(&output.stdout);
            let passed = tap.lines().find_map(|line| line.strip_prefix("# pass ")?.parse::<usize>().ok()).is_some_and(|n| n > 0);
            let source = std::fs::read_to_string(&test)?;
            let pinned_passed = pinned_native_names(&source).iter().all(|(name, disabled)| !disabled && tap.lines().any(|line| {
                let line = line.trim_start();
                line.starts_with("ok ") && line.split_once(" - ").is_some_and(|(_, reported)| reported == name)
            }));
            if !output.status.success() || !passed || !pinned_passed {
                return Err(refuse("SurfaceCheckFailed", format!(
                    "surface {surface}: test `{}` exited {}, ran no passing tests, or omitted a pinned test", relative.display(), output.status.code().map_or("by signal".into(), |c| c.to_string())
                )));
            }
        }
    }
    Ok(())
}

/// The package manager a surface's lock file names; pnpm when it carries none.
fn manager(dir: &Path) -> &'static str {
    if dir.join("package-lock.json").is_file() {
        "npm"
    } else if dir.join("bun.lockb").is_file() || dir.join("bun.lock").is_file() {
        "bun"
    } else {
        "pnpm"
    }
}

// ---------------------------------------------------------------- formal

/// Every Lean package under `formal/` carrying a theorem inventory.
fn inventoried(root: &Path) -> Vec<PathBuf> {
    let formal = root.join("formal");
    let mut out: Vec<PathBuf> = std::iter::once(formal.clone())
        .chain(std::fs::read_dir(&formal).into_iter().flatten().flatten().map(|e| e.path()))
        .filter(|d| d.join("inventory.toml").is_file())
        .collect();
    out.sort();
    out
}

/// The formal stage's commands (`assurance.gate.formal-stage`): the assumption audit over
/// each inventoried package, the differential run, and the protocol model's bounded check.
pub fn formal_commands(root: &Path) -> Result<Vec<(PathBuf, String, Vec<String>)>> {
    let base = ["run", "--locked", "-q", "-p", "contextful-cli", "--bin", "contextful", "--", "formal"];
    let cli = |rest: &[&str]| base.iter().chain(rest).map(|s| s.to_string()).collect::<Vec<_>>();
    let mut out = Vec::new();
    for package in inventoried(root) {
        let rel = package.strip_prefix(root).unwrap_or(&package).to_string_lossy().to_string();
        out.push((root.to_path_buf(), "cargo".to_string(), cli(&["check", "--root", &rel])));
    }
    out.push((root.to_path_buf(), "cargo".to_string(), cli(&["differential"])));
    let protocol = root.join("formal/protocol");
    if !protocol.join("lakefile.toml").is_file() {
        bail!("the formal stage finds no protocol model at formal/protocol");
    }
    out.push((protocol, "lake".to_string(), vec!["exe".into(), "protocol".into(), "check".into()]));
    Ok(out)
}

/// The formal stage: under the toolchain stage's environment, run every command; the first
/// non-zero exit reds the run.
pub fn formal(root: &Path) -> Result<()> {
    if !root.join("formal").is_dir() {
        eprintln!("formal: no formal/ package in this tree");
        return Ok(());
    }
    apply_toolchain(root)?;
    for (dir, program, args) in formal_commands(root)? {
        eprintln!("formal: {program} {}", args.join(" "));
        let status = Command::new(&program).args(&args).current_dir(&dir).status().with_context(|| format!("running {program}"))?;
        if !status.success() {
            return Err(crate::exited(format!("{program} {}", args.join(" ")), status));
        }
    }
    Ok(())
}
