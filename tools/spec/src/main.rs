//! `contextful-spec` — the one implementation of every rule `spec/00-corpus.md`
//! states. The gate and a local run invoke the identical command.

mod cards;
mod checks;
mod corpus;
mod diagram;
mod slice;
mod targets;
mod scaffold;
mod util;

use anyhow::Result;
use clap::{Parser, Subcommand};
use corpus::*;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;
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
    Extract,
    /// Write `spec/status.md` from `spec/pins.toml`.
    State,
    /// Print the context pack for `<contract>.<operation>`, `<contract>.*` or a milestone number.
    Slice {
        target: String,
        #[arg(long)]
        json: bool,
    },
    /// Set every contract's coverage floor to its live performed count.
    Pins,
    /// Write one failing, tagged test per refusal and limit clause of an operation.
    Scaffold {
        /// `<contract>.<operation>`.
        target: String,
        /// Package directory receiving `tests/integration/<operation>.rs`, relative to `--root`.
        #[arg(long, conflicts_with = "lean", required_unless_present = "lean")]
        package: Option<PathBuf>,
        /// Lean file receiving one `sorry` theorem per clause of the operation, relative to `--root`.
        #[arg(long)]
        lean: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = cli.root.canonicalize().unwrap_or(cli.root.clone());
    let c = Corpus::load(&root)?;
    match cli.cmd {
        Cmd::Lint { check, json } => lint(&c, check.as_deref(), json),
        Cmd::Extract => Ok(std::fs::write(root.join("spec/spec.lock.json"), lock_text(&c))?),
        Cmd::State => {
            std::fs::write(root.join("spec/targets.md"), targets::page(&c))?;
            std::fs::create_dir_all(root.join("spec/cards"))?;
            for (contract, text) in cards::pages(&c) {
                std::fs::write(root.join(format!("spec/cards/{contract}.md")), text)?;
            }
            Ok(std::fs::write(root.join("spec/status.md"), status_text(&c))?)
        }
        Cmd::Pins => raise_floor(&c),
        Cmd::Slice { target, json } => {
            let sl = slice::build(&c, &target)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&sl)?);
            } else {
                print!("{}", slice::markdown(&sl));
            }
            Ok(())
        }
        Cmd::Scaffold { target, package, lean } => match (package, lean) {
            (_, Some(lean)) => scaffold::run_lean(&c, &target, &root.join(lean)),
            (Some(package), None) => scaffold::run(&c, &target, &root.join(package)),
            (None, None) => unreachable!("clap requires one of --package and --lean"),
        },
    }
}

fn lint(c: &Corpus, one: Option<&str>, json: bool) -> Result<()> {
    let names: Vec<&str> = match one {
        Some(n) => vec![n],
        None => checks::CHECKS.to_vec(),
    };
    let mut all: Vec<Finding> = Vec::new();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for n in &names {
        let mut f = checks::run(c, n);
        f.sort_by_key(|a| (a.file.clone(), a.line));
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
        println!("{}", serde_json::to_string_pretty(&Report { total: all.len(), counts: &counts, findings: &all })?);
    } else {
        for f in &all {
            println!("{}:{}  {}  {}", f.file, f.line, f.code, f.message);
        }
        eprintln!("\n--- counts ---");
        for (n, k) in &counts {
            eprintln!("{:<10} {}", n, k);
        }
        eprintln!("{:<10} {}", "TOTAL", all.len());
    }
    if all.is_empty() {
        Ok(())
    } else {
        std::process::exit(1)
    }
}

// ---------------------------------------------------------------- lock

pub fn lock_text(c: &Corpus) -> String {
    #[derive(Serialize)]
    struct Lock<'a> {
        clauses: Vec<&'a Clause>,
        owns: BTreeMap<&'a str, &'a Vec<String>>,
        ledes: BTreeMap<String, &'a str>,
        pointers: Vec<(&'a str, String)>,
        registry: &'a Registry,
    }
    let clauses: Vec<&Clause> = c.clauses().collect();
    let pointers = clauses
        .iter()
        .flat_map(|cl| pointers(&cl.statement).into_iter().map(move |p| (cl.id.as_str(), p)))
        .collect();
    let owns = c.contracts().map(|d| (d.rel.as_str(), &d.owns)).collect();
    let mut s = serde_json::to_string_pretty(&Lock { clauses, owns, ledes: c.ledes(), pointers, registry: &c.reg }).unwrap();
    s.push('\n');
    s
}

// ---------------------------------------------------------------- status

pub fn status_text(c: &Corpus) -> String {
    let pins = checks::load_pins(c);
    let all = checks::pins_by_clause(c);
    let verdicts: BTreeMap<&String, &str> = all.iter().map(|(id, g)| (id, checks::clause_verdict(c, g))).collect();
    let clauses = c.clause_map();
    let mut s = String::new();
    s.push_str("# Status\n\nGenerated by `contextful-spec state` from `spec/pins.toml` and `// spec:` tags; not authored.\n");
    s.push_str("An unpinned clause is `committed`; a pinned one is `performed` when its Rust, Lean or native TypeScript test qualifies and matches its tag's rev, else `broken`.\n\n");
    s.push_str("| Contract | Files | Operations | Clauses | Refusals | Limits | Unsettled | Performed | Broken | Floor |\n");
    s.push_str("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n");
    let mut totals = [0usize; 8];
    for (name, e) in &c.reg.contracts {
        let docs: Vec<&Doc> = c.contracts().filter(|d| d.contract.as_deref() == Some(name)).collect();
        let cls: Vec<&Clause> = docs.iter().flat_map(|d| d.clauses.iter()).collect();
        let ops = c.reg.fragments.get(name).map(|f| f.operation.len()).unwrap_or(0);
        let refusals = cls.iter().filter(|x| x.kind == "refusal").count();
        let limits = cls.iter().filter(|x| x.kind == "limit").count();
        let unsettled = docs.iter().flat_map(|d| d.kinds.iter()).filter(|k| **k == LineKind::Unsettled).count();
        let (mut perf, mut broken) = (0, 0);
        for (id, verdict) in &verdicts {
            if clauses.get(*id).map(|x| &x.contract == name).unwrap_or(false) {
                if *verdict == "performed" {
                    perf += 1
                } else {
                    broken += 1
                }
            }
        }
        let floor = pins.floor.get(name).copied().unwrap_or(0);
        let row = [e.files.len(), ops, cls.len(), refusals, limits, unsettled, perf, broken];
        for (i, v) in row.iter().enumerate() {
            totals[i] += v;
        }
        let _ = writeln!(
            s,
            "| `{name}` | {} | {} | {} | {} | {} | {} | {} | {} | {floor} |",
            row[0], row[1], row[2], row[3], row[4], row[5], row[6], row[7]
        );
    }
    let _ = writeln!(
        s,
        "| **total** | {} | {} | {} | {} | {} | {} | {} | {} | |",
        totals[0], totals[1], totals[2], totals[3], totals[4], totals[5], totals[6], totals[7]
    );
    let records = c.records().count();
    let _ = writeln!(s, "\nDecision records: {records}.\n");
    let (claim, _) = checks::expand_roadmap(c);
    if !claim.is_empty() {
        s.push_str("## Milestones\n\nA milestone reads `closed` when its acceptance test computes `passing` and every operation it names holds a `performed` clause.\n\n");
        s.push_str("| Milestone | Operations | Clauses | Performed | Acceptance | Closed |\n| --- | --- | --- | --- | --- | --- |\n");
        for ml in checks::milestone_lines(c) {
            let m = ml.heading.clone();
            let ops: Vec<&String> = claim.iter().filter(|(_, v)| **v == m).map(|(k, _)| k).collect();
            let cls: Vec<&Clause> = c
                .clauses()
                .filter(|cl| ops.iter().any(|o| **o == format!("{}.{}", cl.contract, cl.operation)))
                .collect();
            let performed: Vec<&&Clause> = cls.iter().filter(|cl| verdicts.get(&cl.id) == Some(&"performed")).collect();
            let perf = performed.len();
            let acc = checks::acceptance_verdict(c, ml.acceptance.as_deref());
            // `corpus.state.closed`: every named operation holds at least one performed clause.
            let covered = ops.iter().all(|o| performed.iter().any(|cl| **o == format!("{}.{}", cl.contract, cl.operation)));
            let closed = if acc == "passing" && !ops.is_empty() && covered { "closed" } else { "open" };
            let _ = writeln!(s, "| {m} | {} | {} | {perf} | {acc} | {closed} |", ops.len(), cls.len());
        }
        let unscheduled = c
            .reg
            .fragments
            .iter()
            .flat_map(|(k, f)| f.operation.keys().map(move |o| format!("{k}.{o}")))
            .filter(|o| !claim.contains_key(o))
            .count();
        let _ = writeln!(s, "\nUnscheduled operations: {unscheduled}.");
    }
    if !all.is_empty() {
        s.push_str("\n## Pins\n\n| Clause | Pinned by | Verdict |\n| --- | --- | --- |\n");
        for (id, group) in &all {
            let sites: Vec<String> = group.iter().map(|p| format!("`{}`", p.site())).collect();
            let _ = writeln!(s, "| `{id}` | {} | {} |", sites.join(", "), verdicts[id]);
        }
    }
    s
}

fn raise_floor(c: &Corpus) -> Result<()> {
    let path = c.root.join("spec/pins.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let clauses = c.clause_map();
    let mut live: BTreeMap<String, i64> = c.reg.contracts.keys().map(|k| (k.clone(), 0)).collect();
    for (id, group) in &checks::pins_by_clause(c) {
        if let Some(cl) = clauses.get(id) {
            if checks::clause_verdict(c, group) == "performed" {
                *live.entry(cl.contract.clone()).or_default() += 1;
            }
        }
    }
    let head = text.split("\n[floor]").next().unwrap_or("").trim_end().to_string();
    let mut out = head;
    out.push_str("\n\n[floor]\n");
    for (k, v) in live {
        let _ = writeln!(out, "{k} = {v}");
    }
    std::fs::write(path, out)?;
    Ok(())
}
