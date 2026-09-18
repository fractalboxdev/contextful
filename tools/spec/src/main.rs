//! `contextful-spec` — the one implementation of every rule `spec/00-corpus.md`
//! states. The gate and a local run invoke the identical command.

mod checks;
mod corpus;

use anyhow::Result;
use clap::{Parser, Subcommand};
use corpus::*;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "contextful-spec", about = "The corpus checker")]
struct Cli {
    /// Repository root holding `spec/`.
    #[arg(long, global = true, default_value = ".")]
    root: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run every check, or one named check.
    Lint {
        #[arg(long)]
        check: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Write `spec/spec.lock.json`.
    Extract {
        #[arg(long)]
        stdout: bool,
    },
    /// Read `spec/pins.toml` and write `spec/status.md`.
    State,
    /// Validate pin entries; `--raise` lifts the coverage floor after a clean run.
    Pins {
        #[arg(long)]
        raise: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = cli.root.canonicalize().unwrap_or(cli.root.clone());
    let c = Corpus::load(&root)?;

    match cli.cmd {
        Cmd::Lint { check, json } => lint(&c, check.as_deref(), json),
        Cmd::Extract { stdout } => extract(&c, stdout),
        Cmd::State => state(&c),
        Cmd::Pins { raise } => pins_cmd(&c, raise),
    }
}

// ---------------------------------------------------------------- lint

fn lint(c: &Corpus, one: Option<&str>, json: bool) -> Result<()> {
    let names: Vec<&str> = match one {
        Some(n) => {
            if !checks::CHECKS.contains(&n) {
                anyhow::bail!("no check named `{}`; known: {}", n, checks::CHECKS.join(", "));
            }
            vec![n]
        }
        None => checks::CHECKS.to_vec(),
    };

    let mut all: Vec<Finding> = Vec::new();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for n in &names {
        let mut f = checks::run(c, n);
        f.sort_by(|a, b| (a.file.clone(), a.line).cmp(&(b.file.clone(), b.line)));
        counts.insert(n, f.len());
        all.append(&mut f);
    }

    if json {
        #[derive(Serialize)]
        struct Report<'a> {
            total: usize,
            counts: &'a BTreeMap<&'a str, usize>,
            findings: &'a [Finding],
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&Report {
                total: all.len(),
                counts: &counts,
                findings: &all,
            })?
        );
    } else {
        for f in &all {
            println!("{}:{}  {}  {}", f.file, f.line, f.code, f.message);
        }
        eprintln!("\n--- counts ---");
        for (n, k) in &counts {
            eprintln!("{:<14} {}", n, k);
        }
        eprintln!("{:<14} {}", "TOTAL", all.len());
    }
    if all.is_empty() {
        Ok(())
    } else {
        std::process::exit(1)
    }
}

// ---------------------------------------------------------------- extract

#[derive(Serialize)]
struct Lock<'a> {
    clauses: Vec<&'a Clause>,
    owns: BTreeMap<String, Vec<String>>,
    registry: &'a Registry,
    named_bounds: BTreeMap<String, BoundRow>,
    references: BTreeMap<String, Vec<String>>,
}

#[derive(Serialize)]
struct BoundRow {
    value: f64,
    unit: String,
    owner: String,
}

fn extract(c: &Corpus, to_stdout: bool) -> Result<()> {
    let clauses = c.all_clauses();
    let owns: BTreeMap<String, Vec<String>> = c
        .docs
        .iter()
        .filter(|d| d.role == "contract")
        .map(|d| (d.rel.clone(), d.owns.clone()))
        .collect();
    let named_bounds: BTreeMap<String, BoundRow> = c
        .reg
        .limits
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                BoundRow { value: v.value, unit: v.unit.clone(), owner: v.owner.clone() },
            )
        })
        .collect();

    let trans = regex::Regex::new(r"\{\{([^}]+)\}\}").unwrap();
    let mut references: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for d in &c.docs {
        for l in &d.lines {
            if l.is_front_matter || l.in_fence {
                continue;
            }
            for m in trans.captures_iter(&l.raw) {
                references
                    .entry(d.rel.clone())
                    .or_default()
                    .push(m[1].trim().to_string());
            }
        }
    }

    let lock = Lock { clauses, owns, registry: &c.reg, named_bounds, references };
    let text = serde_json::to_string_pretty(&lock)?;
    if to_stdout {
        println!("{}", text);
    } else {
        std::fs::write(c.root.join("spec/spec.lock.json"), text + "\n")?;
        eprintln!("wrote spec/spec.lock.json");
    }
    Ok(())
}

// ---------------------------------------------------------------- state

fn state(c: &Corpus) -> Result<()> {
    let pins_path = c.root.join("spec/pins.toml");
    let pins: toml::Value = std::fs::read_to_string(&pins_path)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(toml::Value::Table(Default::default()));
    let pinned: BTreeMap<String, String> = pins
        .get("pin")
        .and_then(|x| x.as_table())
        .map(|t| {
            t.iter()
                .map(|(k, v)| {
                    let shown = v
                        .as_str()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| v.to_string());
                    (k.clone(), shown)
                })
                .collect()
        })
        .unwrap_or_default();

    let broken: Vec<&Finding> = Vec::new();
    let _ = broken;
    let pin_findings = checks::pins(c);
    let broken_ids: std::collections::HashSet<String> = pin_findings
        .iter()
        .filter(|f| f.code == "SpecBrokenPin")
        .filter_map(|f| {
            f.message
                .split('`')
                .nth(1)
                .map(|s| s.to_string())
        })
        .collect();

    let mut per_contract: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut per_file: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut rows = String::new();
    for cl in c.all_clauses() {
        let verdict = if broken_ids.contains(&cl.id) {
            "broken"
        } else if pinned.contains_key(&cl.id) {
            "performed"
        } else {
            "committed"
        };
        let pin = pinned.get(&cl.id).cloned().unwrap_or_default();
        rows.push_str(&format!(
            "| `{}` | {} | {} |\n",
            cl.id,
            verdict,
            if pin.is_empty() { "—".to_string() } else { format!("`{}`", pin) }
        ));
        let e = per_contract.entry(cl.contract.clone()).or_insert((0, 0));
        e.1 += 1;
        if verdict == "performed" {
            e.0 += 1;
        }
        let e = per_file.entry(cl.file.clone()).or_insert((0, 0));
        e.1 += 1;
        if verdict == "performed" {
            e.0 += 1;
        }
    }

    let floors: BTreeMap<String, i64> = pins
        .get("floor")
        .and_then(|x| x.as_table())
        .map(|t| t.iter().filter_map(|(k, v)| v.as_integer().map(|i| (k.clone(), i))).collect())
        .unwrap_or_default();

    let ops_per_contract: BTreeMap<&str, usize> =
        c.reg.operations.values().fold(BTreeMap::new(), |mut m, o| {
            *m.entry(o.contract.as_str()).or_default() += 1;
            m
        });

    let mut unsettled_per_file: BTreeMap<&str, usize> = BTreeMap::new();
    for d in &c.docs {
        if d.role == "contract" {
            unsettled_per_file.insert(d.rel.as_str(), d.unsettled.len());
        }
    }

    let mut out = String::new();
    out.push_str("# Corpus state\n\nGenerated by `contextful-spec state`.\n\n");
    out.push_str("## Per contract\n\n| Contract | Pinned | Clauses | Floor | Operations |\n| --- | --- | --- | --- | --- |\n");
    for (k, (p, t)) in &per_contract {
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} |\n",
            k,
            p,
            t,
            floors.get(k).copied().unwrap_or(0),
            ops_per_contract.get(k.as_str()).copied().unwrap_or(0)
        ));
    }
    out.push_str("\n## Per file\n\n| File | Pinned | Clauses | Unsettled |\n| --- | --- | --- | --- |\n");
    for (k, (p, t)) in &per_file {
        out.push_str(&format!(
            "| `{}` | {} | {} | {} |\n",
            k,
            p,
            t,
            unsettled_per_file.get(k.as_str()).copied().unwrap_or(0)
        ));
    }
    out.push_str("\n## Per clause\n\n| Clause | Verdict | Pin |\n| --- | --- | --- |\n");
    out.push_str(&rows);

    std::fs::write(c.root.join("spec/status.md"), out)?;
    eprintln!("wrote spec/status.md");
    Ok(())
}

// ---------------------------------------------------------------- pins

fn pins_cmd(c: &Corpus, raise: bool) -> Result<()> {
    let findings = checks::pins(c);
    for f in &findings {
        println!("{}:{}  {}  {}", f.file, f.line, f.code, f.message);
    }
    if !raise {
        if findings.is_empty() {
            eprintln!("pins clean");
            return Ok(());
        }
        std::process::exit(1);
    }
    if !findings.is_empty() {
        anyhow::bail!("the floor rises after a clean run; {} findings stand", findings.len());
    }
    let path = c.root.join("spec/pins.toml");
    let raw = std::fs::read_to_string(&path)?;
    let v: toml::Value = raw.parse()?;
    let mut live: BTreeMap<String, i64> = BTreeMap::new();
    if let Some(t) = v.get("pin").and_then(|x| x.as_table()) {
        for id in t.keys() {
            if let Some(contract) = id.split('.').next() {
                *live.entry(contract.to_string()).or_default() += 1;
            }
        }
    }
    let mut doc = v.clone();
    let table = doc.as_table_mut().unwrap();
    let floor = table
        .entry("floor".to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()));
    let floor = floor.as_table_mut().unwrap();
    for (k, n) in &live {
        floor.insert(k.clone(), toml::Value::Integer(*n));
    }
    std::fs::write(&path, toml::to_string_pretty(&doc)?)?;
    eprintln!("floor raised to the live pinned count per contract");
    Ok(())
}
