//! `contextful-spec` — the one implementation of every rule `spec/00-corpus.md`
//! states. The gate and a local run invoke the identical command.

mod cards;
mod checks;
mod corpus;
mod diagram;
mod drift;
mod native_ts;
mod pins;
mod ratchet;
mod records;
mod size;
mod slice;
mod targets;
mod scaffold;
mod types;
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
    /// Write `spec/status.md`, `spec/targets.md`, `spec/cards/` and the lock file's pins.
    State,
    /// Print the context pack for `<contract>.<operation>`, `<contract>.*` or a milestone number,
    /// or the clauses a test file or a diff reaches.
    Slice {
        #[arg(required_unless_present_any = ["file", "diff"], conflicts_with_all = ["file", "diff"])]
        target: Option<String>,
        /// Print each clause the file's tags name, with its current statement.
        #[arg(long, conflicts_with = "diff")]
        file: Option<PathBuf>,
        /// Print each clause whose tagged test the working tree changes against the merge base of this revision.
        #[arg(long)]
        diff: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Set every contract's coverage floor to its live performed count.
    Pins {
        /// Move each `test` entry of `spec/pins.toml` into a tag above its test, and set no floor.
        #[arg(long)]
        migrate: bool,
    },
    /// Rewrite the rev on every tag of each clause, and a close `///` statement copy above it.
    Retag {
        #[arg(required = true)]
        clauses: Vec<String>,
    },
    /// Lower each ratchet in `spec/terms/corpus.toml` to its live count.
    Ceilings,
    /// Rewrite bare citations of each named contract ADR to the one section naming the clause.
    WhyMigrate {
        /// `A-<contract>` record ids.
        #[arg(required = true)]
        records: Vec<String>,
    },
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
            for advisory in size::advisories(&c) {
                eprintln!("{advisory}");
            }
            for (clause, path) in types::restatements(&c) {
                eprintln!("advisory: SpecRestatesCode `{clause}` backticks the keys of `{path}`; point with {{{{type:{path}}}}}");
            }
            std::fs::write(root.join("spec/spec.lock.json"), lock_text(&c))?;
            Ok(std::fs::write(root.join("spec/status.md"), status_text(&c))?)
        }
        Cmd::Pins { migrate: true } => pins::migrate(&c),
        Cmd::Pins { migrate: false } => raise_floor(&c),
        Cmd::Retag { clauses } => pins::retag(&c, &clauses),
        Cmd::Ceilings => ratchet::lower(&c),
        Cmd::WhyMigrate { records } => records::migrate(&c, &records),
        Cmd::Slice { file, diff, json, .. } if file.is_some() || diff.is_some() => {
            let (heading, rows) = match (file, diff) {
                (Some(file), _) => (format!("tagged in `{}`", file.display()), pins::by_file(&c, &file)?),
                (None, Some(rev)) => (format!("reached by the diff from `{rev}`"), pins::by_diff(&c, &rev)?),
                (None, None) => unreachable!("guarded above"),
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                print!("{}", pins::markdown(&heading, &rows));
            }
            Ok(())
        }
        Cmd::Slice { target, json, .. } => {
            let sl = slice::build(&c, target.as_deref().unwrap_or_default())?;
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
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        groups: BTreeMap<String, &'a str>,
        /// The clauses citing each contract ADR section, keyed `A-<contract>#<slug>`.
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        sections: BTreeMap<String, Vec<String>>,
        pointers: Vec<(&'a str, String)>,
        /// Each `{{type:<path>}}` pointer's struct or enum, keyed by its path.
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        types: &'a BTreeMap<String, types::TypeRecord>,
        registry: &'a Registry,
        pins: BTreeMap<String, PinRow>,
    }
    let clauses: Vec<&Clause> = c.clauses().collect();
    let pointers = clauses
        .iter()
        .flat_map(|cl| pointers(&cl.statement).into_iter().map(move |p| (cl.id.as_str(), p)))
        .collect();
    let owns = c.contracts().map(|d| (d.rel.as_str(), &d.owns)).collect();
    let mut s = serde_json::to_string_pretty(&Lock {
        clauses,
        owns,
        ledes: c.ledes(),
        groups: c.group_ledes(),
        sections: records::cited_by(c),
        pointers,
        types: types::records(c),
        registry: &c.reg,
        pins: pin_rows(c).clone(),
    })
    .unwrap();
    s.push('\n');
    s
}

/// One pinned clause in the lock file: where its pins are written, and its verdict.
/// One pinned clause in the lock file: where its pins are written, its verdict, and
/// whether it computes proved.
#[derive(Serialize, Clone)]
pub struct PinRow {
    pub sites: Vec<String>,
    pub verdict: &'static str,
    pub proved: bool,
}

/// Every pinned clause by id (`corpus.state.status-is-computed`), computed once per run.
pub fn pin_rows(c: &Corpus) -> &'static BTreeMap<String, PinRow> {
    static ROWS: std::sync::OnceLock<BTreeMap<String, PinRow>> = std::sync::OnceLock::new();
    ROWS.get_or_init(|| {
        checks::pins_by_clause(c)
            .into_iter()
            .map(|(id, group)| {
                let row = PinRow { sites: group.iter().map(checks::Pin::site).collect(), verdict: checks::clause_verdict(c, &group), proved: checks::proved(c, &group) };
                (id, row)
            })
            .collect()
    })
}

// ---------------------------------------------------------------- status

pub fn status_text(c: &Corpus) -> String {
    let pins = checks::load_pins(c);
    let rows = pin_rows(c);
    let verdicts: BTreeMap<&String, &str> = rows.iter().map(|(id, r)| (id, r.verdict)).collect();
    let clauses = c.clause_map();
    let mut s = String::new();
    s.push_str("# Status\n\nGenerated by `contextful-spec state` from `spec/pins.toml` and `// spec:` tags; not authored.\n");
    s.push_str("An unpinned clause is `committed`; a pinned one is `performed` when its Rust, Lean or native TypeScript test qualifies and matches its tag's rev, else `broken`. A performed clause is `proved` when a theorem its inventory row binds to one of its tests performs beside that test. Each pinned clause's sites, verdict and proof sit under `pins` in `spec/spec.lock.json`.\n\n");
    s.push_str("| Contract | Files | Operations | Clauses | Error cases | Limits | Unsettled | Performed | Proved | Broken | Floor |\n");
    s.push_str("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n");
    let mut totals = [0usize; 9];
    for (name, e) in &c.reg.contracts {
        let docs: Vec<&Doc> = c.contracts().filter(|d| d.contract.as_deref() == Some(name)).collect();
        let cls: Vec<&Clause> = docs.iter().flat_map(|d| d.clauses.iter()).collect();
        let ops = c.reg.fragments.get(name).map(|f| f.operation.len()).unwrap_or(0);
        let refusals = cls.iter().filter(|x| x.kind == "refusal").count();
        let limits = cls.iter().filter(|x| x.kind == "limit").count();
        let unsettled = docs.iter().flat_map(|d| d.kinds.iter()).filter(|k| **k == LineKind::Unsettled).count();
        let (mut perf, mut proved, mut broken) = (0, 0, 0);
        for (id, row) in rows {
            if clauses.get(id).map(|x| &x.contract == name).unwrap_or(false) {
                if row.verdict == "performed" {
                    perf += 1
                } else {
                    broken += 1
                }
                proved += usize::from(row.proved);
            }
        }
        let floor = pins.floor.get(name).copied().unwrap_or(0);
        let row = [e.files.len(), ops, cls.len(), refusals, limits, unsettled, perf, proved, broken];
        for (i, v) in row.iter().enumerate() {
            totals[i] += v;
        }
        let _ = writeln!(
            s,
            "| `{name}` | {} | {} | {} | {} | {} | {} | {} | {} | {} | {floor} |",
            row[0], row[1], row[2], row[3], row[4], row[5], row[6], row[7], row[8]
        );
    }
    let _ = writeln!(
        s,
        "| **total** | {} | {} | {} | {} | {} | {} | {} | {} | {} | |",
        totals[0], totals[1], totals[2], totals[3], totals[4], totals[5], totals[6], totals[7], totals[8]
    );
    let records = c.records().count();
    let _ = writeln!(s, "\nDecision records: {records}.\n");
    let restated = types::restatements(c).len();
    let _ = writeln!(s, "Clauses restating a code type: {restated}.\n");
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
            let closed = if checks::milestone_closed(c, ml.acceptance.as_deref(), &ops, &verdicts) { "closed" } else { "open" };
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
    s
}

fn raise_floor(c: &Corpus) -> Result<()> {
    let path = c.root.join("spec/pins.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let clauses = c.clause_map();
    let mut live: BTreeMap<String, i64> = c.reg.contracts.keys().map(|k| (k.clone(), 0)).collect();
    for (id, row) in pin_rows(c) {
        if let Some(cl) = clauses.get(id) {
            if row.verdict == "performed" {
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
