//! `contextful formal differential` — the engine's pure decision functions against the
//! Lean reference model, over seeded generated cases.
//!
//! Two decisions are under test: table-pattern coverage (`contextful_core::grant`) and
//! grant narrowing legality (`contextful_core::attenuate`). A case is one JSON value; the
//! engine side decodes it through the domain parsers and calls the domain functions, the
//! reference side is the `formal/reference` binary reading the case on standard input.
//! The corpus replays first, then the generated cases run; the first disagreement shrinks
//! by field removal, lands in the corpus and raises `ReferenceModelDrift`.

use anyhow::{bail, Context, Result};
use clap::Args;
use contextful_core::attenuate::{attenuate, Authority, Proposal};
use contextful_core::grant::{Action, AggregateGrant, Grant, TablePattern, TenantScope};
use contextful_core::identify::NormalizedSubject;
use contextful_core::AuthorityError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Minimized disagreements the corpus retains (`assurance-counterexample-corpus`: 256 entries).
pub const COUNTEREXAMPLE_CORPUS_ENTRIES: usize = 256;

/// One invocation, corpus replay included (`assurance-differential-budget`: 300 s).
pub const DIFFERENTIAL_BUDGET: Duration = Duration::from_secs(300);

/// Generated cases a run draws absent `--cases`.
pub const DEFAULT_CASES: u64 = 1000;

/// The corpus, relative to the reference package root.
pub const DEFAULT_CORPUS: &str = "corpus/counterexamples.jsonl";

/// The reference binary `lake build` writes, relative to the reference package root.
pub const REFERENCE_EXE: &str = ".lake/build/bin/contextful-reference";

/// The decision fields the harness compares, in order.
pub const COMPARED_FIELDS: [&str; 3] = ["verdict", "error", "dimension"];

/// The identifier both sides give a case that does not decode as a case.
pub const CASE_MALFORMED: &str = "CaseMalformed";

/// The refusals of `contextful formal differential`. `Display` begins with the identifier.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DifferentialError {
    /// (`assurance.differential-test.disagreement`)
    #[error("ReferenceModelDrift: {0}")]
    ReferenceModelDrift(String),
    /// (`assurance.differential-test.discarded-counterexample`)
    #[error("CounterexampleDiscarded: {0}")]
    CounterexampleDiscarded(String),
}

#[derive(Args)]
pub struct DifferentialArgs {
    /// The generator seed; absent, one is drawn and printed.
    #[arg(long)]
    seed: Option<u64>,
    /// Generated cases to run after the corpus replays.
    #[arg(long, default_value_t = DEFAULT_CASES)]
    cases: u64,
    /// The reference Lean package, built with `lake build` (default: `formal/reference/`
    /// under the repository).
    #[arg(long)]
    root: Option<PathBuf>,
    /// A prebuilt reference executable, used as is instead of building the package.
    #[arg(long)]
    reference: Option<PathBuf>,
    /// The counterexample corpus (default: `<root>/corpus/counterexamples.jsonl`).
    #[arg(long)]
    corpus: Option<PathBuf>,
    /// Lower the run budget below 300 s.
    #[arg(long, hide = true)]
    budget_secs: Option<u64>,
    /// Read one case on standard input and print the engine's decision.
    #[arg(long)]
    decide: bool,
}

pub fn run(args: DifferentialArgs) -> Result<()> {
    if args.decide {
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input)?;
        let decision = match serde_json::from_str::<Value>(&input) {
            Ok(case) => engine_decide(&case),
            Err(_) => Decision::refused(CASE_MALFORMED, None),
        };
        println!("{}", serde_json::to_string(&decision)?);
        return Ok(());
    }
    let budget = match args.budget_secs {
        None => DIFFERENTIAL_BUDGET,
        Some(s) if s <= DIFFERENTIAL_BUDGET.as_secs() => Duration::from_secs(s),
        Some(s) => bail!("a budget of {s} s exceeds the {} s one invocation has", DIFFERENTIAL_BUDGET.as_secs()),
    };
    let started = Instant::now();
    let root = match &args.root {
        Some(r) => r.clone(),
        None => {
            let cwd = std::env::current_dir()?;
            toplevel(&cwd).unwrap_or(cwd).join("formal/reference")
        }
    };
    let reference = match args.reference {
        Some(exe) => exe,
        None => build_reference(&root)?,
    };
    let corpus = args.corpus.unwrap_or_else(|| root.join(DEFAULT_CORPUS));
    let seed = args.seed.unwrap_or_else(fresh_seed);
    let harness = Harness { reference, corpus, deadline: started + budget, budget };
    let report = harness.run(seed, args.cases)?;
    print!("{report}");
    Ok(())
}

fn toplevel(dir: &Path) -> Option<PathBuf> {
    let out = Command::new("git").args(["rev-parse", "--show-toplevel"]).current_dir(dir).output().ok()?;
    out.status.success().then(|| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

fn build_reference(root: &Path) -> Result<PathBuf> {
    if !root.join("lakefile.toml").is_file() {
        bail!("no reference package at {}", root.display());
    }
    let out = Command::new("lake").arg("build").current_dir(root).output().context("running `lake build`")?;
    if !out.status.success() {
        bail!(
            "`lake build` failed in {}:\n{}{}",
            root.display(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(root.join(REFERENCE_EXE))
}

fn fresh_seed() -> u64 {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    (nanos as u64) ^ u64::from(std::process::id()).rotate_left(32)
}

// ---------------------------------------------------------------- decisions

/// One decision: a verdict, an error identifier, and the dimension a widening names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub verdict: String,
    pub error: Option<String>,
    pub dimension: Option<String>,
}

impl Decision {
    fn verdict(v: &str) -> Decision {
        Decision { verdict: v.to_string(), error: None, dimension: None }
    }

    fn refused(error: &str, dimension: Option<&str>) -> Decision {
        Decision { verdict: "refused".into(), error: Some(error.into()), dimension: dimension.map(str::to_string) }
    }

    fn coverage(covered: bool) -> Decision {
        Decision::verdict(if covered { "covered" } else { "not_covered" })
    }

    /// The fields on which two decisions differ, in [`COMPARED_FIELDS`] order.
    pub fn differing(&self, other: &Decision) -> Vec<&'static str> {
        let pairs = [
            self.verdict == other.verdict,
            self.error == other.error,
            self.dimension == other.dimension,
        ];
        COMPARED_FIELDS.iter().zip(pairs).filter(|(_, same)| !same).map(|(f, _)| *f).collect()
    }

    /// The decision as the corpus prints it.
    fn render(&self) -> String {
        serde_json::to_value(self).map(|v| v.to_string()).unwrap_or_default()
    }
}

/// A fault decoding a case: its shape, or a domain parser's refusal.
enum Fault {
    Malformed,
    Refused(AuthorityError),
}

impl From<AuthorityError> for Fault {
    fn from(e: AuthorityError) -> Fault {
        Fault::Refused(e)
    }
}

type Decoded<T> = std::result::Result<T, Fault>;

/// The error identifier an [`AuthorityError`] carries at the head of its `Display`.
fn identifier(e: &AuthorityError) -> String {
    e.to_string().split(':').next().unwrap_or_default().to_string()
}

/// The engine's decision on one case.
pub fn engine_decide(case: &Value) -> Decision {
    match decide_case(case) {
        Ok(d) => d,
        Err(Fault::Malformed) => Decision::refused(CASE_MALFORMED, None),
        Err(Fault::Refused(e)) => refusal(&e),
    }
}

fn refusal(e: &AuthorityError) -> Decision {
    let id = identifier(e);
    let dimension = match e {
        // The widened dimension heads the message (`authority.attenuate.widens`).
        AuthorityError::AttenuationWidens(msg) => msg.split(':').next().map(str::trim),
        _ => None,
    };
    Decision::refused(&id, dimension)
}

fn decide_case(case: &Value) -> Decoded<Decision> {
    let obj = case.as_object().ok_or(Fault::Malformed)?;
    match string(required(obj, "op")?)? {
        "covers_name" => {
            let pattern = string(required(obj, "pattern")?)?;
            let name = string(required(obj, "name")?)?;
            Ok(Decision::coverage(TablePattern::parse(pattern)?.covers_name(name)))
        }
        "covers_pattern" => {
            let pattern = string(required(obj, "pattern")?)?;
            let other = string(required(obj, "other")?)?;
            let pattern = TablePattern::parse(pattern)?;
            let other = TablePattern::parse(other)?;
            Ok(Decision::coverage(pattern.covers(&other)))
        }
        "narrow" => {
            let parent = required(obj, "parent")?.as_array().ok_or(Fault::Malformed)?;
            let child = required(obj, "child")?.as_array().ok_or(Fault::Malformed)?;
            let parent = parent.iter().map(grant).collect::<Decoded<Vec<_>>>()?;
            let child = child.iter().map(grant).collect::<Decoded<Vec<_>>>()?;
            let authority = Authority { grants: parent, exp: 0, subject: NormalizedSubject::default() };
            let proposal = Proposal { grants: Some(child), ..Proposal::default() };
            Ok(match attenuate(&authority, &proposal) {
                Ok(_) => Decision::verdict("admitted"),
                Err(e) => refusal(&e),
            })
        }
        _ => Err(Fault::Malformed),
    }
}

/// A present, non-null field.
fn field<'a>(obj: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    obj.get(key).filter(|v| !v.is_null())
}

fn required<'a>(obj: &'a Map<String, Value>, key: &str) -> Decoded<&'a Value> {
    field(obj, key).ok_or(Fault::Malformed)
}

fn string(v: &Value) -> Decoded<&str> {
    v.as_str().ok_or(Fault::Malformed)
}

fn strings(v: &Value) -> Decoded<Vec<String>> {
    v.as_array().ok_or(Fault::Malformed)?.iter().map(|s| string(s).map(str::to_string)).collect()
}

fn unsigned(v: &Value) -> Decoded<u64> {
    v.as_u64().ok_or(Fault::Malformed)
}

fn optional<T>(obj: &Map<String, Value>, key: &str, f: impl Fn(&Value) -> Decoded<T>) -> Decoded<Option<T>> {
    field(obj, key).map(f).transpose()
}

fn tenant(v: &Value) -> Decoded<TenantScope> {
    let obj = v.as_object().ok_or(Fault::Malformed)?;
    let table = string(required(obj, "table")?)?.to_string();
    let value = string(required(obj, "value")?)?.to_string();
    Ok(TenantScope { table, value })
}

fn aggregate(v: &Value) -> Decoded<AggregateGrant> {
    let obj = v.as_object().ok_or(Fault::Malformed)?;
    let min_group_size = unsigned(required(obj, "min_group_size")?)?;
    let max_contributor_share = required(obj, "max_contributor_share")?.as_f64().ok_or(Fault::Malformed)?;
    let functions = strings(required(obj, "functions")?)?;
    let max_groups = unsigned(required(obj, "max_groups")?)?;
    let max_rows = optional(obj, "max_rows", unsigned)?;
    Ok(AggregateGrant { min_group_size, max_contributor_share, functions, max_groups, max_rows })
}

/// One grant: its shape first, then each action, then each table pattern.
fn grant(v: &Value) -> Decoded<Grant> {
    let obj = v.as_object().ok_or(Fault::Malformed)?;
    let actions = strings(required(obj, "actions")?)?;
    let tables = strings(required(obj, "tables")?)?;
    let tenant = optional(obj, "tenant", tenant)?;
    let templates = optional(obj, "templates", strings)?;
    let aggregate = optional(obj, "aggregate", aggregate)?;
    let max_rows = optional(obj, "max_rows", unsigned)?;
    let actions = actions.iter().map(|a| Action::parse(a)).collect::<Result<Vec<_>, _>>()?;
    let tables = tables.iter().map(|t| TablePattern::parse(t)).collect::<Result<Vec<_>, _>>()?;
    Ok(Grant { actions, tables, tenant, aggregate, templates, max_rows })
}

// ---------------------------------------------------------------- generator

/// SplitMix64: a fixed, dependency-free sequence per seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }

    fn subset<T: Clone>(&mut self, xs: &[T], percent: u64) -> Vec<T> {
        xs.iter().filter(|_| self.chance(percent)).cloned().collect()
    }
}

/// The class a generated case is drawn from (`assurance.differential-test.case-classes`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CaseClass {
    Malformed,
    Boundary,
    WellFormed,
}

impl CaseClass {
    pub const ALL: [CaseClass; 3] = [CaseClass::Malformed, CaseClass::Boundary, CaseClass::WellFormed];

    pub fn as_str(self) -> &'static str {
        match self {
            CaseClass::Malformed => "malformed",
            CaseClass::Boundary => "boundary",
            CaseClass::WellFormed => "well-formed",
        }
    }
}

/// The value pools a class draws from.
struct Pools {
    names: &'static [&'static str],
    patterns: &'static [&'static str],
    templates: &'static [&'static str],
    functions: &'static [&'static str],
    tenants: &'static [(&'static str, &'static str)],
    counts: &'static [u64],
    shares: &'static [f64],
    /// How often a grant carries aggregate constraints, in percent.
    aggregate_percent: u64,
}

const ACTIONS: [&str; 4] = ["read", "write", "execute", "admin"];

const WELL_FORMED: Pools = Pools {
    names: &["research", "research/filings", "research/filings/eu", "sales/invoices", "sales", "res"],
    patterns: &[
        "*",
        "research/*",
        "research/filings",
        "research/fil*",
        "res*",
        "sales/*",
        "sales/invoices",
        "research",
        "r*",
    ],
    templates: &["quarterly_rollup", "monthly", "*"],
    functions: &["count", "sum", "avg"],
    tenants: &[("research/filings", "acme-eu"), ("research/filings", "acme-us"), ("sales/invoices", "acme-eu")],
    counts: &[2, 5, 10, 20, 100],
    shares: &[0.1, 0.25, 0.5, 0.75, 1.0],
    aggregate_percent: 30,
};

const BOUNDARY: Pools = Pools {
    names: &["", "*", "research/", "research/filings ", "RESEARCH/filings", "\u{e9}t\u{e9}/q", "e\u{301}t\u{e9}/q", "r"],
    patterns: &["*", "", "research/", "research/*", "r*", "\u{e9}*", "e\u{301}*", "RESEARCH/*", "research/filings "],
    templates: &["*", "", "quarterly_rollup", "QUARTERLY_ROLLUP"],
    functions: &["count", "", "COUNT"],
    tenants: &[
        ("research/filings", "acme"),
        ("research/filings", "Acme"),
        ("research/filings", "acme "),
        ("research/filings", "\u{e9}"),
        ("research/filings", "e\u{301}"),
        ("research/filings", ""),
    ],
    counts: &[0, 1, 2, u64::MAX - 1, u64::MAX],
    shares: &[0.0, 1.0, 0.5, 0.9999],
    aggregate_percent: 60,
};

const MISPLACED_STARS: [&str; 5] = ["a*b", "**", "*x", "re*s*", "research/*/eu"];
const UNKNOWN_ACTIONS: [&str; 5] = ["READ", "delete", " read", "", "reads"];

fn gen_pattern(rng: &mut Rng, pools: &Pools) -> Value {
    json!(rng.pick(pools.patterns))
}

fn gen_aggregate(rng: &mut Rng, pools: &Pools) -> Value {
    let mut agg = json!({
        "min_group_size": rng.pick(pools.counts),
        "max_contributor_share": rng.pick(pools.shares),
        "functions": rng.subset(pools.functions, 60),
        "max_groups": rng.pick(pools.counts),
    });
    if rng.chance(40) {
        agg["max_rows"] = json!(rng.pick(pools.counts));
    }
    agg
}

fn gen_grant(rng: &mut Rng, pools: &Pools) -> Value {
    let mut actions = rng.subset(&ACTIONS, 40);
    if actions.is_empty() && rng.chance(80) {
        actions.push(ACTIONS[0]);
    }
    let tables: Vec<Value> = (0..rng.below(3) + usize::from(rng.chance(85))).map(|_| gen_pattern(rng, pools)).collect();
    let mut g = json!({"actions": actions, "tables": tables});
    if rng.chance(30) {
        let (table, value) = rng.pick(pools.tenants);
        g["tenant"] = json!({"table": table, "value": value});
    }
    if rng.chance(40) {
        g["templates"] = json!(rng.subset(pools.templates, 50));
    }
    if rng.chance(pools.aggregate_percent) {
        g["aggregate"] = gen_aggregate(rng, pools);
    }
    if rng.chance(20) {
        g["max_rows"] = json!(rng.pick(pools.counts));
    }
    g
}

/// A child grant derived from a parent grant: kept or narrowed on each dimension, then
/// at times widened on one.
fn derive_grant(rng: &mut Rng, pools: &Pools, parent: &Value) -> Value {
    let mut g = parent.clone();
    let strs = |v: &Value| v.as_array().cloned().unwrap_or_default();
    g["actions"] = json!(rng.subset(&strs(&parent["actions"]), 70));
    let mut tables = Vec::new();
    for t in strs(&parent["tables"]) {
        if !rng.chance(80) {
            continue;
        }
        let narrower = pools
            .names
            .iter()
            .copied()
            .find(|n| t.as_str().and_then(|p| TablePattern::parse(p).ok()).is_some_and(|p| p.covers_name(n)));
        tables.push(match narrower {
            Some(n) if rng.chance(40) => json!(n),
            _ => t,
        });
    }
    g["tables"] = json!(tables);
    if let Some(templates) = parent.get("templates") {
        if rng.chance(30) {
            if let Some(o) = g.as_object_mut() {
                o.remove("templates");
            }
        } else {
            g["templates"] = json!(rng.subset(&strs(templates), 70));
        }
    }
    // Each aggregate count keeps its value, steps to a neighbour, or jumps to a pool value.
    if let Some(agg) = g.get_mut("aggregate").filter(|_| rng.chance(60)) {
        let key = *rng.pick(&["min_group_size", "max_groups", "max_rows"]);
        let value = match agg.get(key).and_then(Value::as_u64) {
            Some(n) if rng.chance(60) => {
                if rng.chance(50) {
                    n.saturating_add(1)
                } else {
                    n.saturating_sub(1)
                }
            }
            _ => *rng.pick(pools.counts),
        };
        agg[key] = json!(value);
    }
    for (key, percent) in [("aggregate", 30), ("tenant", 10)] {
        if parent.get(key).is_some() && rng.chance(percent) {
            if let Some(o) = g.as_object_mut() {
                o.remove(key);
            }
        }
    }
    if rng.chance(40) {
        match rng.below(6) {
            0 => g["actions"] = json!(ACTIONS.to_vec()),
            1 => {
                let extra = gen_pattern(rng, pools);
                if let Some(t) = g["tables"].as_array_mut() {
                    t.push(extra);
                }
            }
            2 => g["templates"] = json!(rng.subset(pools.templates, 60)),
            3 => g["aggregate"] = gen_aggregate(rng, pools),
            4 => {
                let (table, value) = rng.pick(pools.tenants);
                g["tenant"] = json!({"table": table, "value": value});
            }
            _ => {
                if let Some(agg) = g.get_mut("aggregate") {
                    agg["max_groups"] = json!(rng.pick(pools.counts));
                    agg["min_group_size"] = json!(rng.pick(pools.counts));
                }
            }
        }
    }
    g
}

fn gen_request(rng: &mut Rng, pools: &Pools) -> Value {
    match rng.below(10) {
        0..=2 => json!({"op": "covers_name", "pattern": gen_pattern(rng, pools), "name": rng.pick(pools.names)}),
        3..=5 => json!({"op": "covers_pattern", "pattern": gen_pattern(rng, pools), "other": gen_pattern(rng, pools)}),
        _ => {
            let parent: Vec<Value> = (0..rng.below(3) + usize::from(rng.chance(90))).map(|_| gen_grant(rng, pools)).collect();
            let child: Vec<Value> = (0..rng.below(3) + usize::from(rng.chance(80)))
                .map(|_| {
                    if !parent.is_empty() && rng.chance(80) {
                        let p = rng.pick(&parent).clone();
                        derive_grant(rng, pools, &p)
                    } else {
                        gen_grant(rng, pools)
                    }
                })
                .collect();
            json!({"op": "narrow", "parent": parent, "child": child})
        }
    }
}

/// Every path to a value inside `v`, the root included.
fn paths(v: &Value, here: &mut Vec<Step>, out: &mut Vec<Vec<Step>>) {
    out.push(here.clone());
    match v {
        Value::Object(m) => {
            for (k, child) in m {
                here.push(Step::Key(k.clone()));
                paths(child, here, out);
                here.pop();
            }
        }
        Value::Array(a) => {
            for (i, child) in a.iter().enumerate() {
                here.push(Step::Index(i));
                paths(child, here, out);
                here.pop();
            }
        }
        _ => {}
    }
}

#[derive(Debug, Clone)]
enum Step {
    Key(String),
    Index(usize),
}

fn at_mut<'a>(v: &'a mut Value, path: &[Step]) -> Option<&'a mut Value> {
    path.iter().try_fold(v, |v, step| match step {
        Step::Key(k) => v.get_mut(k.as_str()),
        Step::Index(i) => v.get_mut(*i),
    })
}

/// One corruption of a well-formed request: a misplaced star, an unknown action, a value
/// of the wrong type, a missing field, a negative or fractional integer.
fn corrupt(rng: &mut Rng, mut case: Value) -> Value {
    let mut all = Vec::new();
    paths(&case, &mut Vec::new(), &mut all);
    let strings: Vec<&Vec<Step>> = all
        .iter()
        .filter(|p| matches!(p.last(), Some(Step::Index(_))) || matches!(p.last(), Some(Step::Key(k)) if k == "pattern" || k == "other"))
        .collect();
    match rng.below(6) {
        0 => {
            let path = strings.get(rng.below(strings.len())).map(|p| (*p).clone());
            let target = path.and_then(|p| at_mut(&mut case, &p).filter(|v| v.is_string()).map(|_| p));
            match target {
                Some(p) => {
                    let star = if rng.chance(50) { *rng.pick(&MISPLACED_STARS) } else { *rng.pick(&UNKNOWN_ACTIONS) };
                    if let Some(v) = at_mut(&mut case, &p) {
                        *v = json!(star);
                    }
                }
                None => case["pattern"] = json!(rng.pick(&MISPLACED_STARS)),
            }
        }
        1 => {
            let path = all[rng.below(all.len())].clone();
            let wrong = [json!(null), json!(true), json!(7), json!(-1), json!(1.5), json!("x"), json!([]), json!({})];
            let replacement = rng.pick(&wrong).clone();
            if let Some(v) = at_mut(&mut case, &path) {
                *v = replacement;
            }
        }
        2 => {
            let objects: Vec<&Vec<Step>> = all.iter().filter(|p| !p.is_empty()).collect();
            if let Some(p) = objects.get(rng.below(objects.len())) {
                let (last, parent) = p.split_last().unwrap_or((&Step::Index(0), &[]));
                if let (Some(Value::Object(o)), Step::Key(k)) = (at_mut(&mut case, parent), last) {
                    o.remove(k);
                }
            }
        }
        3 => {
            let integers: Vec<&Vec<Step>> = all
                .iter()
                .filter(|p| matches!(p.last(), Some(Step::Key(k)) if ["min_group_size", "max_groups", "max_rows"].contains(&k.as_str())))
                .collect();
            let bad = [json!(-1), json!(1.5), json!(3.0), json!("3")];
            let replacement = rng.pick(&bad).clone();
            match integers.get(rng.below(integers.len())) {
                Some(p) => {
                    let p = (*p).clone();
                    if let Some(v) = at_mut(&mut case, &p) {
                        *v = replacement;
                    }
                }
                None => case = json!({"op": "narrow", "parent": [{"actions": ["read"], "tables": ["*"], "max_rows": replacement}], "child": []}),
            }
        }
        4 => case["op"] = json!(rng.pick(&["cover", "", "NARROW", "covers_names"])),
        _ => case = rng.pick(&[json!(null), json!([]), json!("narrow"), json!(3), json!({})]).clone(),
    }
    case
}

/// The case at one position of a seeded sequence.
struct Generated {
    index: u64,
    class: CaseClass,
    case: Value,
}

struct Generator {
    rng: Rng,
    index: u64,
}

impl Generator {
    fn new(seed: u64) -> Generator {
        Generator { rng: Rng(seed), index: 0 }
    }

    fn next_case(&mut self) -> Generated {
        let rng = &mut self.rng;
        let class = match rng.below(10) {
            0..=1 => CaseClass::Malformed,
            2..=4 => CaseClass::Boundary,
            _ => CaseClass::WellFormed,
        };
        let case = match class {
            CaseClass::WellFormed => gen_request(rng, &WELL_FORMED),
            CaseClass::Boundary => gen_request(rng, &BOUNDARY),
            CaseClass::Malformed => {
                let pools = if rng.chance(50) { &WELL_FORMED } else { &BOUNDARY };
                let base = gen_request(rng, pools);
                corrupt(rng, base)
            }
        };
        let g = Generated { index: self.index, class, case };
        self.index += 1;
        g
    }

    /// The case a seed draws at `index`.
    fn nth(seed: u64, index: u64) -> Generated {
        let mut g = Generator::new(seed);
        loop {
            let c = g.next_case();
            if c.index == index {
                return c;
            }
        }
    }
}

// ---------------------------------------------------------------- shrinking

/// Every value with one object key or one array element removed, at any depth.
fn removals(v: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    match v {
        Value::Object(map) => {
            for key in map.keys() {
                let mut m = map.clone();
                m.remove(key);
                out.push(Value::Object(m));
            }
            for (key, child) in map {
                for r in removals(child) {
                    let mut m = map.clone();
                    m.insert(key.clone(), r);
                    out.push(Value::Object(m));
                }
            }
        }
        Value::Array(items) => {
            for i in 0..items.len() {
                let mut a = items.clone();
                a.remove(i);
                out.push(Value::Array(a));
            }
            for (i, child) in items.iter().enumerate() {
                for r in removals(child) {
                    let mut a = items.clone();
                    a[i] = r;
                    out.push(Value::Array(a));
                }
            }
        }
        _ => {}
    }
    out
}

/// Every value with one string shortened by one character, at any depth.
fn shortenings(v: &Value) -> Vec<Value> {
    match v {
        Value::String(s) => {
            let chars: Vec<char> = s.chars().collect();
            (0..chars.len())
                .map(|i| Value::String(chars.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, c)| c).collect()))
                .collect()
        }
        Value::Object(map) => map
            .iter()
            .flat_map(|(k, child)| {
                shortenings(child).into_iter().map(move |s| {
                    let mut m = map.clone();
                    m.insert(k.clone(), s);
                    Value::Object(m)
                })
            })
            .collect(),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .flat_map(|(i, child)| {
                shortenings(child).into_iter().map(move |s| {
                    let mut a = items.clone();
                    a[i] = s;
                    Value::Array(a)
                })
            })
            .collect(),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------- corpus

/// One corpus line: the minimized case, both decisions on it, and where it came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub case: Value,
    pub engine: Decision,
    pub reference: Decision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// The generated case before shrinking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original: Option<Value>,
}

fn load_corpus(path: &Path) -> Result<Vec<Entry>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        // No corpus there yet: the run replays nothing, and a write reports what blocks it.
        Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => {
            return Ok(Vec::new())
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(n, l)| serde_json::from_str(l).with_context(|| format!("{}:{}: not a corpus entry", path.display(), n + 1)))
        .collect()
}

fn store_corpus(path: &Path, entries: &[Entry]) -> Result<()> {
    let mut text = String::new();
    for e in entries {
        text.push_str(&serde_json::to_string(e)?);
        text.push('\n');
    }
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
}

// ---------------------------------------------------------------- harness

struct Harness {
    reference: PathBuf,
    corpus: PathBuf,
    deadline: Instant,
    budget: Duration,
}

/// What a run drew and compared.
struct Report {
    replayed: usize,
    generated: u64,
    classes: BTreeMap<CaseClass, u64>,
    operations: BTreeMap<String, u64>,
    digest: String,
    elapsed: Duration,
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let classes: Vec<String> =
            CaseClass::ALL.iter().map(|c| format!("{} {}", c.as_str(), self.classes.get(c).copied().unwrap_or(0))).collect();
        let ops: Vec<String> = self.operations.iter().map(|(op, n)| format!("{op} {n}")).collect();
        writeln!(f, "replayed {}", self.replayed)?;
        writeln!(f, "generated {}", self.generated)?;
        writeln!(f, "classes {}", classes.join(", "))?;
        writeln!(f, "operations {}", ops.join(", "))?;
        writeln!(f, "compared fields {}", COMPARED_FIELDS.join(", "))?;
        writeln!(f, "cases sha256 {}", self.digest)?;
        writeln!(f, "disagreements 0")?;
        writeln!(f, "elapsed {:.1} s", self.elapsed.as_secs_f64())
    }
}

impl Harness {
    fn run(&self, seed: u64, cases: u64) -> Result<Report> {
        let started = Instant::now();
        println!("seed {seed}");
        let corpus = load_corpus(&self.corpus)?;
        for entry in &corpus {
            self.within_budget()?;
            let (engine, reference) = self.both(&entry.case)?;
            if !engine.differing(&reference).is_empty() {
                // The case is already recorded; its entry is the retained counterexample.
                return Err(self.drift(&entry.case, &engine, &reference, "replayed from the corpus").into());
            }
        }
        let mut generator = Generator::new(seed);
        let mut digest = Sha256::new();
        let mut classes = BTreeMap::new();
        let mut operations = BTreeMap::new();
        for _ in 0..cases {
            self.within_budget()?;
            let g = generator.next_case();
            digest.update(g.case.to_string().as_bytes());
            digest.update(b"\n");
            *classes.entry(g.class).or_insert(0) += 1;
            let op = g
                .case
                .get("op")
                .and_then(Value::as_str)
                .filter(|op| ["covers_name", "covers_pattern", "narrow"].contains(op))
                .unwrap_or("unknown");
            *operations.entry(op.to_string()).or_insert(0) += 1;
            let (engine, reference) = self.both(&g.case)?;
            if !engine.differing(&reference).is_empty() {
                return Err(self.record(seed, g)?);
            }
        }
        Ok(Report {
            replayed: corpus.len(),
            generated: cases,
            classes,
            operations,
            digest: format!("{:x}", digest.finalize()),
            elapsed: started.elapsed(),
        })
    }

    fn within_budget(&self) -> Result<()> {
        if Instant::now() > self.deadline {
            bail!("the run exceeded its budget of {} s (one invocation has at most {} s)", self.budget.as_secs(), DIFFERENTIAL_BUDGET.as_secs());
        }
        Ok(())
    }

    fn reference_decide(&self, case: &Value) -> Result<Decision> {
        let mut child = Command::new(&self.reference)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("running the reference binary {}", self.reference.display()))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(format!("{case}\n").as_bytes())?;
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!(
                "the reference binary exited {} on {case}:\n{}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            );
        }
        serde_json::from_slice(&out.stdout).with_context(|| {
            format!("the reference binary printed no decision on {case}: {}", String::from_utf8_lossy(&out.stdout))
        })
    }

    fn both(&self, case: &Value) -> Result<(Decision, Decision)> {
        Ok((engine_decide(case), self.reference_decide(case)?))
    }

    fn disagrees(&self, case: &Value) -> Result<bool> {
        self.within_budget()?;
        let (e, r) = self.both(case)?;
        Ok(!e.differing(&r).is_empty())
    }

    /// Shrink until no single field removal keeps the disagreement.
    fn shrink(&self, mut case: Value) -> Result<Value> {
        loop {
            let mut next = None;
            for candidate in removals(&case).into_iter().chain(shortenings(&case)) {
                if self.disagrees(&candidate)? {
                    next = Some(candidate);
                    break;
                }
            }
            match next {
                Some(c) => case = c,
                None => return Ok(case),
            }
        }
    }

    /// Whether the case an entry's seed draws at its index still disagrees.
    fn reproduces(&self, entry: &Entry) -> Result<bool> {
        match (entry.seed, entry.index) {
            (Some(seed), Some(index)) => self.disagrees(&Generator::nth(seed, index).case),
            _ => Ok(false),
        }
    }

    /// Shrink a disagreement, record it, and return the refusal the run ends with.
    fn record(&self, seed: u64, g: Generated) -> Result<anyhow::Error> {
        let minimized = self.shrink(g.case.clone())?;
        let (engine, reference) = self.both(&minimized)?;
        let drift = self.drift(&minimized, &engine, &reference, &format!("seed {seed}, case {} ({})", g.index, g.class.as_str()));
        let entry = Entry {
            case: minimized.clone(),
            engine,
            reference,
            seed: Some(seed),
            index: Some(g.index),
            class: Some(g.class.as_str().to_string()),
            original: Some(g.case),
        };
        let written = self.append(entry).and_then(|()| load_corpus(&self.corpus));
        match written {
            Ok(entries) if entries.iter().any(|e| e.case == minimized) => Ok(drift.into()),
            Ok(_) => Ok(DifferentialError::CounterexampleDiscarded(format!(
                "{} holds no entry for the disagreement\n{drift}",
                self.corpus.display()
            ))
            .into()),
            Err(e) => Ok(DifferentialError::CounterexampleDiscarded(format!(
                "{} took no entry ({e:#})\n{drift}",
                self.corpus.display()
            ))
            .into()),
        }
    }

    /// Append an entry, evicting at capacity the oldest entry its own seed reproduces,
    /// else the oldest.
    fn append(&self, entry: Entry) -> Result<()> {
        let mut entries = load_corpus(&self.corpus)?;
        while entries.len() >= COUNTEREXAMPLE_CORPUS_ENTRIES {
            let mut evict = 0;
            for (i, e) in entries.iter().enumerate() {
                if self.reproduces(e)? {
                    evict = i;
                    break;
                }
            }
            entries.remove(evict);
        }
        entries.push(entry);
        store_corpus(&self.corpus, &entries)
    }

    fn drift(&self, case: &Value, engine: &Decision, reference: &Decision, origin: &str) -> DifferentialError {
        DifferentialError::ReferenceModelDrift(format!(
            "the engine and the reference model disagree on {}\n  minimized case  {case}\n  engine          {}\n  reference       {}\n  origin          {origin}\n  corpus          {}",
            engine.differing(reference).join(", "),
            engine.render(),
            reference.render(),
            self.corpus.display()
        ))
    }
}
