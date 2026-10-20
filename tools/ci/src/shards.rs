//! The committed CLI shard map (`assurance.gate.cli-shard-map`): each remaining CLI
//! integration test's shard and its recorded duration, balanced so every shard's estimated
//! test wall clock in the gate container fits [`BUDGET_SECS`]. A group names tests sharing
//! one nested build, which run in one shard so the build runs once.

use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::Path;

/// The map's path from the repository root.
pub const MAP: &str = "tools/ci/cli-shards.toml";

/// The shards the remaining CLI tests split into (`assurance.gate.workspace-parts`).
pub const SHARDS: usize = 8;

/// Libtest threads in one gate container, its processor count.
const THREADS: f64 = 4.0;

/// The estimated test wall clock a shard of several units holds within: 12 min
/// (`assurance.gate.cli-shard-map`).
const BUDGET_SECS: f64 = 720.0;

/// One test's shard and its recorded duration in gate-container seconds.
#[derive(Clone, Copy, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    shard: usize,
    seconds: f64,
}

#[derive(Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    /// Group name to the test-name prefixes its members start with.
    #[serde(default)]
    group: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    test: BTreeMap<String, Entry>,
}

impl File {
    /// The group `name` belongs to: the first, in name order, holding a prefix of it.
    fn group_of(&self, name: &str) -> Option<&str> {
        self.group.iter().find(|(_, prefixes)| prefixes.iter().any(|p| name.starts_with(p.as_str()))).map(|(g, _)| g.as_str())
    }
}

/// The tests one shard runs, and the drift between the map and the listed suite.
pub struct Assignment {
    pub tests: Vec<String>,
    /// Listed tests the map lacks, each run with its group or in the least-loaded shard.
    pub unmapped: Vec<String>,
    /// Map entries naming no listed test.
    pub stale: Vec<String>,
    /// The least-loaded shard, running every unmapped test outside a mapped group.
    pub fallback: usize,
}

fn read(root: &Path) -> Result<File> {
    let text = match std::fs::read_to_string(root.join(MAP)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(File::default()),
        Err(e) => return Err(e).with_context(|| format!("reading {MAP}")),
    };
    let file: File = toml::from_str(&text).with_context(|| format!("parsing {MAP}"))?;
    for (name, entry) in &file.test {
        if !(1..=SHARDS).contains(&entry.shard) || !entry.seconds.is_finite() || entry.seconds < 0.0 {
            bail!("{MAP}: `{name}` names shard {} at {} s; a shard is 1 to {SHARDS} and a duration non-negative", entry.shard, entry.seconds);
        }
    }
    let mut homes: BTreeMap<&str, usize> = BTreeMap::new();
    for (name, entry) in &file.test {
        if let Some(group) = file.group_of(name) {
            if let Some(other) = homes.insert(group, entry.shard).filter(|k| *k != entry.shard) {
                bail!("{MAP}: group `{group}` spans shards {other} and {}", entry.shard);
            }
        }
    }
    Ok(file)
}

/// A shard's estimated test wall clock: its recorded seconds spread over [`THREADS`], and
/// never below its longest test.
fn wall(seconds: &[f64]) -> f64 {
    let sum: f64 = seconds.iter().sum();
    seconds.iter().copied().fold(sum / THREADS, f64::max)
}

/// The listed tests shard `k` runs: those the map assigns it, each unmapped test whose group
/// the map places on `k`, and, when `k` is the shard whose mapped listed tests carry the least
/// estimated wall clock, every other unmapped test.
pub fn assign(root: &Path, listed: &[String], k: usize) -> Result<Assignment> {
    let file = read(root)?;
    let mut per: Vec<Vec<f64>> = vec![Vec::new(); SHARDS];
    for name in listed {
        if let Some(entry) = file.test.get(name) {
            per[entry.shard - 1].push(entry.seconds);
        }
    }
    let fallback = (1..=SHARDS).min_by(|a, b| wall(&per[a - 1]).total_cmp(&wall(&per[b - 1]))).expect("at least one shard");
    let group_shard = |name: &str| {
        let group = file.group_of(name)?;
        file.test.iter().find(|(member, _)| file.group_of(member) == Some(group)).map(|(_, e)| e.shard)
    };
    let shard = |name: &String| file.test.get(name).map(|e| e.shard).or_else(|| group_shard(name)).unwrap_or(fallback);
    Ok(Assignment {
        tests: listed.iter().filter(|n| shard(n) == k).cloned().collect(),
        unmapped: listed.iter().filter(|n| !file.test.contains_key(*n)).cloned().collect(),
        stale: file.test.keys().filter(|n| !listed.contains(n)).cloned().collect(),
        fallback,
    })
}

/// The ordinary tests a libtest JSON report (`--format json -Z unstable-options
/// --report-time`) timed, each duration multiplied by `scale`; `skip` excludes the tests
/// another part runs.
fn timings(report: &str, scale: f64, skip: &str) -> Result<BTreeMap<String, f64>> {
    let mut out = BTreeMap::new();
    for line in report.lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if event["type"] != "test" {
            continue;
        }
        let (Some(name), Some(secs)) = (event["name"].as_str(), event["exec_time"].as_f64()) else { continue };
        if !name.contains(skip) {
            out.insert(name.to_string(), secs * scale);
        }
    }
    if out.is_empty() {
        bail!("the report times no test; run libtest with `--format json -Z unstable-options --report-time`");
    }
    Ok(out)
}

/// Units — a group's members together, every other test alone — longest first onto the
/// shard of least recorded seconds, ties to the lowest shard.
fn balance(file: &mut File, times: &BTreeMap<String, f64>) -> Result<()> {
    let mut units: BTreeMap<String, Vec<(&String, f64)>> = BTreeMap::new();
    for (name, seconds) in times {
        let unit = file.group_of(name).map_or_else(|| name.clone(), |g| format!("group {g}"));
        units.entry(unit).or_default().push((name, *seconds));
    }
    let mut order: Vec<(String, f64, Vec<(&String, f64)>)> =
        units.into_iter().map(|(u, members)| (u, members.iter().map(|(_, s)| s).sum::<f64>(), members)).collect();
    order.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut load = vec![0.0f64; SHARDS];
    file.test.clear();
    for (_, total, members) in order {
        let k = (1..=SHARDS).min_by(|a, b| load[a - 1].total_cmp(&load[b - 1])).expect("at least one shard");
        load[k - 1] += total;
        for (name, seconds) in members {
            file.test.insert(name.clone(), Entry { shard: k, seconds });
        }
    }
    estimate(file)?;
    Ok(())
}

/// Each shard's estimated wall clock and whether it holds one indivisible unit, refusing a map
/// whose shard of several units exceeds the budget, since more shards would split it.
fn estimate(file: &File) -> Result<Vec<(f64, usize, bool)>> {
    let mut per: Vec<Vec<f64>> = vec![Vec::new(); SHARDS];
    let mut units: Vec<std::collections::BTreeSet<String>> = vec![Default::default(); SHARDS];
    for (name, entry) in &file.test {
        per[entry.shard - 1].push(entry.seconds);
        units[entry.shard - 1].insert(file.group_of(name).map_or_else(|| name.clone(), |g| format!("group {g}")));
    }
    let out: Vec<(f64, usize, bool)> = per.iter().zip(&units).map(|(s, u)| (wall(s), s.len(), u.len() == 1)).collect();
    let over: Vec<String> =
        out.iter().enumerate().filter(|(_, (w, _, one))| *w > BUDGET_SECS && !one).map(|(i, (w, _, _))| format!("cli-{} {w:.0} s", i + 1)).collect();
    if !over.is_empty() {
        let total: f64 = file.test.values().map(|e| e.seconds).sum();
        bail!(
            "{} exceed the 12 min shard budget; {SHARDS} shards hold {:.0} s of tests, so raise `SHARDS` in tools/ci/src/shards.rs",
            over.join(", "),
            total / THREADS
        );
    }
    Ok(out)
}

fn render(file: &File) -> String {
    let quote = |s: &str| toml::Value::String(s.to_string()).to_string();
    let mut out = String::from(
        "# Each remaining CLI integration test's shard and its duration in gate-container seconds\n\
         # (`assurance.gate.cli-shard-map`). `contextful-ci cli-shards --report <libtest json>\n\
         # --scale <gate/local>` rewrites [test] from a timed run and keeps [group]. A group names\n\
         # the prefixes of tests sharing one nested build, kept on one shard. An unmapped test runs\n\
         # with its group or in the least-loaded shard, and every shard reports it as drift.\n\n[group]\n",
    );
    for (name, prefixes) in &file.group {
        out.push_str(&format!("{} = [{}]\n", quote(name), prefixes.iter().map(|p| quote(p)).collect::<Vec<_>>().join(", ")));
    }
    out.push_str("\n[test]\n");
    for (name, entry) in &file.test {
        out.push_str(&format!("{} = {{ shard = {}, seconds = {:.1} }}\n", quote(name), entry.shard, entry.seconds));
    }
    out
}

/// `contextful-ci cli-shards`: with `report`, rewrites the map's tests from its timings,
/// refusing a report no assignment fits; then prints each shard's estimated wall clock and
/// fails when a shard of several units exceeds the budget.
pub fn command(root: &Path, report: Option<&Path>, scale: f64, skip: &str) -> Result<()> {
    let mut file = read(root)?;
    if let Some(path) = report {
        let text = std::fs::read_to_string(root.join(path)).with_context(|| format!("reading {}", path.display()))?;
        balance(&mut file, &timings(&text, scale, skip)?)?;
        let out = root.join(MAP);
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&out, render(&file)).with_context(|| format!("writing {MAP}"))?;
    }
    for (i, (wall, count, one)) in estimate(&file)?.iter().enumerate() {
        let note = if *one && *wall > BUDGET_SECS { ", one indivisible unit past the budget" } else { "" };
        println!("cli-{} {count} test(s) {wall:.0} s{note}", i + 1);
    }
    Ok(())
}
