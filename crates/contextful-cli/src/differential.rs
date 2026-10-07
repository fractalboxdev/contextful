//! `contextful formal differential` — the decision module's native and WebAssembly builds
//! against the Lean reference model, over seeded generated cases.
//!
//! Five decisions are under test: table-pattern coverage, grant narrowing legality, zone
//! admission, session-zone resolution and credential admission, all in
//! `contextful_policy::decide`. A case is one text, usually JSON; the native build decides
//! it in process, the `wasm32-unknown-unknown` build decides it under the decision-module
//! host, and the reference is the `formal/reference` binary reading it on standard input.
//! A credential case skips the reference, which models no signature scheme. The corpus
//! replays first, then the generated cases run; the first case on which any two decisions
//! differ shrinks, lands in the corpus and raises `ReferenceModelDrift`.

use anyhow::{bail, Context, Result};
use clap::Args;
pub use contextful_core::decide::{Decision, DECISION_FIELDS as COMPARED_FIELDS};
use contextful_core::grant::{Action, Grant, TablePattern};
use contextful_core::identify::Subject;
use contextful_core::issue::{MintPlan, SignatureAlgorithm};
use contextful_core::time::Instant as At;
use contextful_policy::decide::VERIFY;
use contextful_policy::issue::{mint_seeded, MintClaims, SeedSigner};
use std::cell::RefCell;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
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

/// The target the decision module's WebAssembly build compiles for.
#[cfg(feature = "component-host")]
pub const WASM_TARGET: &str = "wasm32-unknown-unknown";

/// The target directory, under the cargo target directory, the WebAssembly build uses.
#[cfg(feature = "component-host")]
pub const WASM_TARGET_DIR: &str = "decision-wasm";

/// The package holding the decision module.
#[cfg(feature = "component-host")]
pub const DECISION_PACKAGE: &str = "contextful-policy";

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
    /// A prebuilt `wasm32-unknown-unknown` build of the decision module, used as is instead
    /// of building one with cargo.
    #[arg(long)]
    wasm: Option<PathBuf>,
    /// Read one case on standard input and print the native build's decision.
    #[arg(long)]
    decide: bool,
}

pub fn run(args: DifferentialArgs) -> Result<()> {
    if args.decide {
        let mut input = Vec::new();
        std::io::stdin().read_to_end(&mut input)?;
        println!("{}", serde_json::to_string(&contextful_policy::decide::decide(&input))?);
        return Ok(());
    }
    let budget = match args.budget_secs {
        None => DIFFERENTIAL_BUDGET,
        Some(s) if s <= DIFFERENTIAL_BUDGET.as_secs() => Duration::from_secs(s),
        Some(s) => bail!("a budget of {s} s exceeds the {} s one invocation has", DIFFERENTIAL_BUDGET.as_secs()),
    };
    let started = Instant::now();
    let cwd = std::env::current_dir()?;
    let repo = toplevel(&cwd).unwrap_or(cwd);
    let root = args.root.clone().unwrap_or_else(|| repo.join("formal/reference"));
    let reference = match args.reference {
        Some(exe) => exe,
        None => build_reference(&root)?,
    };
    let corpus = args.corpus.unwrap_or_else(|| root.join(DEFAULT_CORPUS));
    let seed = args.seed.unwrap_or_else(fresh_seed);
    let wasm = wasm_build(&repo, args.wasm)?;
    let harness = Harness { reference, wasm, corpus, deadline: started + budget, budget };
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

/// The decision module's WebAssembly build, loaded: the one `--wasm` names, or one cargo
/// builds from the repository. A binary built without the component host compares the
/// native build alone.
#[cfg(feature = "component-host")]
fn wasm_build(repo: &Path, prebuilt: Option<PathBuf>) -> Result<Option<RefCell<contextful_wasm::DecisionModule>>> {
    let path = match prebuilt {
        Some(p) => p,
        None => build_wasm(repo)?,
    };
    let bytes = std::fs::read(&path).with_context(|| format!("reading the WebAssembly build {}", path.display()))?;
    let module = contextful_wasm::DecisionModule::load(&bytes).with_context(|| format!("loading {}", path.display()))?;
    Ok(Some(RefCell::new(module)))
}

#[cfg(not(feature = "component-host"))]
fn wasm_build(_repo: &Path, prebuilt: Option<PathBuf>) -> Result<Option<RefCell<Never>>> {
    match prebuilt {
        Some(p) => bail!("this binary carries no component host to run {}", p.display()),
        None => Ok(None),
    }
}

/// The decision module's host where the binary carries none.
#[cfg(not(feature = "component-host"))]
enum Never {}

#[cfg(not(feature = "component-host"))]
impl Never {
    fn decide(&mut self, _case: &[u8]) -> Result<Vec<u8>> {
        match *self {}
    }
}

/// Build the decision module for `wasm32-unknown-unknown` as a `cdylib`, into its own
/// target directory, and return the module's path.
#[cfg(feature = "component-host")]
fn build_wasm(repo: &Path) -> Result<PathBuf> {
    let base = match std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from) {
        Some(d) if d.is_absolute() => d,
        Some(d) => repo.join(d),
        None => repo.join("target"),
    };
    let target_dir = base.join(WASM_TARGET_DIR);
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let out = Command::new(cargo)
        .args(["rustc", "-q", "-p", DECISION_PACKAGE, "--lib", "--release", "--target", WASM_TARGET, "--crate-type", "cdylib"])
        .arg("--target-dir")
        .arg(&target_dir)
        .current_dir(repo)
        .output()
        .context("running cargo")?;
    if !out.status.success() {
        bail!(
            "building {DECISION_PACKAGE} for {WASM_TARGET} failed in {}:\n{}",
            repo.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(target_dir.join(WASM_TARGET).join("release").join(format!("{}.wasm", DECISION_PACKAGE.replace('-', "_"))))
}

fn fresh_seed() -> u64 {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    (nanos as u64) ^ u64::from(std::process::id()).rotate_left(32)
}

// ---------------------------------------------------------------- decisions

/// The decision as the corpus prints it.
fn render(d: &Decision) -> String {
    serde_json::to_value(d).map(|v| v.to_string()).unwrap_or_default()
}

/// One case: a JSON value, or bytes no JSON value serializes to.
#[derive(Debug, Clone, PartialEq)]
pub enum Case {
    Json(Value),
    Bytes(Vec<u8>),
}

impl Case {
    /// The text every decider reads.
    pub fn text(&self) -> Vec<u8> {
        match self {
            Case::Json(v) => v.to_string().into_bytes(),
            Case::Bytes(b) => b.clone(),
        }
    }

    /// Whether the reference model decides the case: every text but one reading as a JSON
    /// credential case, whatever its field values.
    fn reaches_reference(&self) -> bool {
        let text = self.text();
        serde_json::from_slice::<Value>(&text).map_or(true, |v| v.get("op").and_then(Value::as_str) != Some(VERIFY))
    }

    /// The operation a report counts the case under.
    fn op(&self) -> &str {
        match self {
            Case::Json(v) => v.get("op").and_then(Value::as_str).filter(|op| OPERATIONS.contains(op)).unwrap_or("unknown"),
            Case::Bytes(_) => "bytes",
        }
    }
}

impl std::fmt::Display for Case {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Case::Json(v) => write!(f, "{v}"),
            Case::Bytes(b) => write!(f, "bytes {}", hex(b)),
        }
    }
}

/// The verdicts a credential case decides, in report order.
pub const CREDENTIAL_VERDICTS: [&str; 3] = ["admitted", "not_covered", "refused"];

/// The operations a case names.
pub const OPERATIONS: [&str; 6] = ["covers_name", "covers_pattern", "narrow", "zone_admits", "session_zone", VERIFY];

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

/// The decisions on one case; `wasm` is absent where the binary carries no host, and
/// `reference` on a credential case.
#[derive(Debug, Clone)]
struct Decisions {
    native: Decision,
    wasm: Option<Decision>,
    reference: Option<Decision>,
}

impl Decisions {
    /// Every field on which any two decisions differ, in [`COMPARED_FIELDS`] order.
    fn differing(&self) -> Vec<&'static str> {
        let others: Vec<&Decision> = self.wasm.iter().chain(&self.reference).collect();
        let mut pairs: Vec<(&Decision, &Decision)> = others.iter().map(|o| (&self.native, *o)).collect();
        if let (Some(w), Some(r)) = (&self.wasm, &self.reference) {
            pairs.push((w, r));
        }
        COMPARED_FIELDS.iter().copied().filter(|f| pairs.iter().any(|(a, b)| a.differing(b).contains(f))).collect()
    }

    fn agree(&self) -> bool {
        self.differing().is_empty()
    }
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
    zones: &'static [&'static str],
    entries: &'static [&'static str],
    /// Credential lifetimes, in seconds from issue.
    lifetimes: &'static [u64],
    /// Evaluation instants, in seconds from expiry.
    expiry_offsets: &'static [i64],
    /// The audiences a checkpoint declares; an empty one declares none.
    audiences: &'static [&'static str],
    /// How often a credential's text is damaged before the case carries it, in percent.
    damage_percent: u64,
}

const ACTIONS: [&str; 5] = ["read", "write", "execute", "admin", "forget"];

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
    zones: &["local:device", "on-prem:hq", "on-prem:ward-3", "private-cloud:vpc-7", "public-cloud:us-east-1", "public-cloud:eu-west-1"],
    entries: &[
        "*",
        "local:device",
        "on-prem:*",
        "on-prem:hq",
        "private-cloud:*",
        "private-cloud:vpc-7",
        "public-cloud:*",
        "public-cloud:us-east-1",
    ],
    lifetimes: &[60, 900, 3600],
    expiry_offsets: &[-3000, -600, -60],
    audiences: &[AUDIENCE, AUDIENCE, AUDIENCE, OTHER_AUDIENCE],
    damage_percent: 0,
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
    // Whitespace Unicode lists, a zero-width space it does not, identifiers at and past
    // 128 chars, a second colon, case and non-ASCII letters.
    zones: &[
        "",
        " local:device ",
        "\u{a0}on-prem:hq",
        "on-prem:hq\u{3000}",
        "\u{200b}on-prem:hq",
        "\u{85}public-cloud:x\u{2029}",
        "on-prem:",
        "on-prem:*",
        "Local:device",
        "on-prem:a:b",
        "on-prem:xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        "on-prem:xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        "on-prem:\u{e9}",
        "public-cloud:us east",
        "undeclared",
    ],
    entries: &[
        "*",
        " * ",
        "",
        "local:device",
        "on-prem:*",
        "on-prem:**",
        "on-prem:",
        "onprem:*",
        "on-prem:hq",
        "\u{a0}on-prem:*",
        "*:*",
        "local:*",
        "on-prem:a:b",
        "on-prem:xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        "on-prem:xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        "public-cloud:*",
    ],
    lifetimes: &[0, 1, 3600, 3601, 86_400],
    expiry_offsets: &[-1, 0, 1, -3601],
    audiences: &[AUDIENCE, "", " contextful://acme-research", "CONTEXTFUL://ACME-RESEARCH", OTHER_AUDIENCE],
    damage_percent: 40,
};

/// The audience every generated credential names.
const AUDIENCE: &str = "contextful://acme-research";
const OTHER_AUDIENCE: &str = "contextful://another-project";

/// The instant every generated credential is issued at: 2030-01-01T00:00:00Z.
const ISSUED_AT: i64 = 1_893_456_000;

/// The issuer seeds a credential case signs under, each in the library's text form.
const ISSUERS: [&str; 3] = [
    "ed25519-private/1111111111111111111111111111111111111111111111111111111111111111",
    "ed25519-private/2222222222222222222222222222222222222222222222222222222222222222",
    "secp256r1-private/3333333333333333333333333333333333333333333333333333333333333333",
];

fn issuer(seed: &str) -> SeedSigner {
    SeedSigner::from_seed(seed).expect("a fixed issuer seed decodes")
}

/// A credential case: a credential minted from the sequence, checked against pinned keys
/// that include its issuer's or not, at an instant around its expiry, for a declared
/// audience or none, optionally denylisted, holder-bound or damaged, and optionally asking
/// whether an action over tables is covered.
fn gen_verify(rng: &mut Rng, pools: &Pools) -> Value {
    let issuer_at = rng.below(ISSUERS.len());
    let signer = issuer(ISSUERS[issuer_at]);
    let grants: Vec<Grant> = (0..rng.below(3) + 1)
        .map(|_| {
            let mut actions = rng.subset(&[Action::Read, Action::Write, Action::Execute, Action::Forget], 50);
            if actions.is_empty() {
                actions.push(Action::Read);
            }
            let mut tables: Vec<TablePattern> =
                (0..rng.below(2) + 1).filter_map(|_| TablePattern::parse(rng.pick(pools.patterns)).ok()).collect();
            if tables.is_empty() {
                tables.push(TablePattern::parse("research/*").expect("a fixed pattern parses"));
            }
            Grant { actions, tables, tenant: None, aggregate: None, templates: None, max_rows: None, max_duration_ms: None, max_response_bytes: None }
        })
        .collect();
    let lifetime = *rng.pick(pools.lifetimes);
    let expires = ISSUED_AT + lifetime as i64;
    let plan = MintPlan {
        subject: Subject {
            on_behalf_of: rng.chance(90).then(|| "user://dana@acme.example".to_string()),
            agent: rng.chance(60).then(|| "agent://research-loop".to_string()),
            ..Subject::default()
        },
        grants,
        audience: AUDIENCE.to_string(),
        issuer: None,
        algorithm: if issuer_at == 2 { SignatureAlgorithm::Es256 } else { SignatureAlgorithm::Ed25519 },
        issued_at: At::from_unix_secs(ISSUED_AT).expect("a fixed instant"),
        expires_at: At::from_unix_secs(expires).expect("a fixed instant"),
        lifetime_secs: lifetime,
    };
    let claims = MintClaims { confirmation: rng.chance(10).then(|| "holder-thumbprint".to_string()), epoch: 0, ..MintClaims::default() };
    let mut seed = [0u8; 32];
    for chunk in seed.chunks_mut(8) {
        chunk.copy_from_slice(&rng.next().to_le_bytes());
    }
    let credential = mint_seeded(&plan, &claims, &signer, &seed).expect("a generated plan mints");
    let mut keys = Vec::new();
    for (i, other) in ISSUERS.iter().enumerate() {
        let pinned = if i == issuer_at { rng.chance(90) } else { rng.chance(30) };
        if pinned {
            keys.push(format!("k{i}={}", issuer(other).public_key_text()));
        }
    }
    let at = expires + *rng.pick(pools.expiry_offsets);
    let mut case = json!({"op": VERIFY, "credential": credential, "keys": keys, "at": at.max(0)});
    let audience = *rng.pick(pools.audiences);
    if !audience.is_empty() {
        case["audience"] = json!(audience);
    }
    if rng.chance(15) {
        let rev = contextful_policy::verify::introspect(&credential).map(|i| i.authority.rev.id).unwrap_or_default();
        case["denylist"] = json!([rev]);
    }
    if rng.chance(50) {
        let tables: Vec<&str> = (0..rng.below(2) + 1).map(|_| *rng.pick(pools.names)).collect();
        case["action"] = json!(rng.pick(&ACTIONS));
        case["tables"] = json!(tables);
    }
    if rng.chance(pools.damage_percent) {
        let mut text = credential.into_bytes();
        let at = rng.below(text.len());
        match rng.below(3) {
            0 => text[at] = if text[at] == b'A' { b'B' } else { b'A' },
            1 => text.truncate(at),
            _ => text.insert(at, b'='),
        }
        case["credential"] = json!(String::from_utf8_lossy(&text));
    }
    case
}

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
    match rng.below(16) {
        14..=15 => gen_verify(rng, pools),
        0..=2 => json!({"op": "covers_name", "pattern": gen_pattern(rng, pools), "name": rng.pick(pools.names)}),
        3..=5 => json!({"op": "covers_pattern", "pattern": gen_pattern(rng, pools), "other": gen_pattern(rng, pools)}),
        10..=11 => {
            let allow: Vec<&str> = (0..rng.below(4)).map(|_| *rng.pick(pools.entries)).collect();
            json!({"op": "zone_admits", "zone": rng.pick(pools.zones), "allow": allow})
        }
        12..=13 => {
            let mut case = json!({"op": "session_zone", "incognito": rng.chance(50)});
            for key in ["asserted", "signed"] {
                if rng.chance(60) {
                    case[key] = json!(rng.pick(pools.zones));
                }
            }
            case
        }
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
        4 => case["op"] = json!(rng.pick(&["cover", "", "NARROW", "covers_names", "ZONE_ADMITS", "session-zone", "VERIFY"])),
        _ => case = rng.pick(&[json!(null), json!([]), json!("narrow"), json!(3), json!({})]).clone(),
    }
    case
}

/// Byte sequences no UTF-8 decoder accepts: a stray byte, a truncated or overlong
/// sequence, an encoded surrogate, a scalar past U+10FFFF, a lone continuation byte.
const INVALID_UTF8: [&[u8]; 7] =
    [b"\xff", b"\xc3\x28", b"\xc0\xaf", b"\xed\xa0\x80", b"\xe2\x82", b"\xf4\x90\x80\x80", b"\x80"];

/// Integer literals outside the unsigned 64-bit range.
const OUT_OF_RANGE: [&str; 6] = [
    "18446744073709551616",
    "18446744073709551617",
    "100000000000000000000",
    "340282366920938463463374607431768211456",
    "-9223372036854775809",
    "-1",
];

/// Stands in for a value until the case serializes; no pool value holds `@`.
const MARKER: &str = "@@@";

/// A well-formed request's text with invalid UTF-8 in one of its strings, or an
/// out-of-range literal in one of its integer fields.
fn malformed_bytes(rng: &mut Rng, pools: &Pools) -> Vec<u8> {
    let mut case = gen_request(rng, pools);
    if rng.chance(50) {
        let bad = *rng.pick(&INVALID_UTF8);
        let text = if mark(rng, &mut case, |_, v| v.is_string()) { case.to_string() } else { format!("{MARKER}{case}") };
        splice_marker(text.as_bytes(), bad)
    } else {
        let literal = rng.pick(&OUT_OF_RANGE).as_bytes();
        let integer = |p: &[Step], _: &Value| {
            matches!(p.last(), Some(Step::Key(k)) if ["min_group_size", "max_groups", "max_rows"].contains(&k.as_str()))
        };
        if !mark(rng, &mut case, integer) {
            case = json!({"op": "narrow", "parent": [{"actions": ["read"], "tables": ["*"], "max_rows": MARKER}], "child": []});
        }
        splice_marker(case.to_string().as_bytes(), &[b"\"", literal, b"\""].concat())
    }
}

/// Replace one value `keep` admits, chosen at random, with the marker; false when `keep`
/// admits none.
fn mark(rng: &mut Rng, case: &mut Value, keep: impl Fn(&[Step], &Value) -> bool) -> bool {
    let mut all = Vec::new();
    paths(case, &mut Vec::new(), &mut all);
    let candidates: Vec<Vec<Step>> = all.into_iter().filter(|p| at_mut(case, p).is_some_and(|v| keep(p, v))).collect();
    if candidates.is_empty() {
        return false;
    }
    let path = &candidates[rng.below(candidates.len())];
    if let Some(v) = at_mut(case, path) {
        *v = json!(MARKER);
    }
    true
}

/// The text with the first quoted or bare marker replaced by `with`: a quoted marker loses
/// its quotes only where `with` supplies them.
fn splice_marker(text: &[u8], with: &[u8]) -> Vec<u8> {
    let needle = MARKER.as_bytes();
    match text.windows(needle.len()).position(|w| w == needle) {
        Some(at) => {
            let quoted = with.first() == Some(&b'"') && at > 0 && text.get(at + needle.len()) == Some(&b'"');
            let (from, to) = if quoted { (at - 1, at + needle.len() + 1) } else { (at, at + needle.len()) };
            let body = if quoted { &with[1..with.len() - 1] } else { with };
            [&text[..from], body, &text[to..]].concat()
        }
        None => [text, with].concat(),
    }
}

/// The case at one position of a seeded sequence.
struct Generated {
    index: u64,
    class: CaseClass,
    case: Case,
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
            CaseClass::WellFormed => Case::Json(gen_request(rng, &WELL_FORMED)),
            CaseClass::Boundary => Case::Json(gen_request(rng, &BOUNDARY)),
            CaseClass::Malformed => {
                let pools = if rng.chance(50) { &WELL_FORMED } else { &BOUNDARY };
                if rng.chance(30) {
                    Case::Bytes(malformed_bytes(rng, pools))
                } else {
                    let base = gen_request(rng, pools);
                    Case::Json(corrupt(rng, base))
                }
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

/// One corpus line: the minimized case, every decision on it, and where it came from. A
/// case no JSON value serializes to sits in `bytes`, hex-encoded, with `case` absent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub case: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<String>,
    /// The native build's decision.
    pub engine: Decision,
    /// The WebAssembly build's decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wasm: Option<Decision>,
    /// The reference model's decision; absent on a credential case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<Decision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// The generated case before shrinking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_bytes: Option<String>,
}

impl Entry {
    fn case(&self) -> Case {
        match self.bytes.as_deref().and_then(unhex) {
            Some(b) => Case::Bytes(b),
            None => Case::Json(self.case.clone()),
        }
    }
}

/// A case split into the entry's JSON and hex-encoded forms.
fn entry_forms(case: &Case) -> (Value, Option<String>) {
    match case {
        Case::Json(v) => (v.clone(), None),
        Case::Bytes(b) => (Value::Null, Some(hex(b))),
    }
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

#[cfg(feature = "component-host")]
type WasmHost = contextful_wasm::DecisionModule;
#[cfg(not(feature = "component-host"))]
type WasmHost = Never;

struct Harness {
    reference: PathBuf,
    wasm: Option<RefCell<WasmHost>>,
    corpus: PathBuf,
    deadline: Instant,
    budget: Duration,
}

/// What a run drew and compared.
struct Report {
    replayed: usize,
    generated: u64,
    builds: &'static str,
    classes: BTreeMap<CaseClass, u64>,
    operations: BTreeMap<String, u64>,
    /// The native build's verdict on each credential case, counted.
    credential_verdicts: BTreeMap<String, u64>,
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
        writeln!(f, "builds {}", self.builds)?;
        writeln!(f, "classes {}", classes.join(", "))?;
        writeln!(f, "operations {}", ops.join(", "))?;
        let verdicts: Vec<String> = CREDENTIAL_VERDICTS
            .iter()
            .map(|v| format!("{v} {}", self.credential_verdicts.get(*v).copied().unwrap_or(0)))
            .collect();
        writeln!(f, "verify verdicts {}", verdicts.join(", "))?;
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
            let case = entry.case();
            let decisions = self.decide(&case)?;
            if !decisions.agree() {
                // The case is already recorded; its entry is the retained counterexample.
                return Err(self.drift(&case, &decisions, "replayed from the corpus").into());
            }
        }
        let mut generator = Generator::new(seed);
        let mut digest = Sha256::new();
        let mut classes = BTreeMap::new();
        let mut operations = BTreeMap::new();
        let mut credential_verdicts = BTreeMap::new();
        for _ in 0..cases {
            self.within_budget()?;
            let g = generator.next_case();
            digest.update(g.case.text());
            digest.update(b"\n");
            *classes.entry(g.class).or_insert(0) += 1;
            *operations.entry(g.case.op().to_string()).or_insert(0) += 1;
            let decisions = self.decide(&g.case)?;
            if !decisions.agree() {
                return Err(self.record(seed, g)?);
            }
            if g.case.op() == VERIFY {
                *credential_verdicts.entry(decisions.native.verdict).or_insert(0) += 1;
            }
        }
        Ok(Report {
            replayed: corpus.len(),
            generated: cases,
            builds: if self.wasm.is_some() { "native, wasm32-unknown-unknown" } else { "native" },
            classes,
            operations,
            credential_verdicts,
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

    fn reference_decide(&self, case: &Case) -> Result<Decision> {
        let mut child = Command::new(&self.reference)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("running the reference binary {}", self.reference.display()))?;
        if let Some(mut stdin) = child.stdin.take() {
            let mut text = case.text();
            text.push(b'\n');
            stdin.write_all(&text)?;
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

    fn wasm_decide(&self, case: &Case) -> Result<Option<Decision>> {
        let Some(module) = &self.wasm else { return Ok(None) };
        let out = module.borrow_mut().decide(&case.text())?;
        let decision = serde_json::from_slice(&out).with_context(|| {
            format!("the WebAssembly build printed no decision on {case}: {}", String::from_utf8_lossy(&out))
        })?;
        Ok(Some(decision))
    }

    fn decide(&self, case: &Case) -> Result<Decisions> {
        let native = contextful_policy::decide::decide(&case.text());
        let wasm = self.wasm_decide(case)?;
        let reference = if case.reaches_reference() { Some(self.reference_decide(case)?) } else { None };
        Ok(Decisions { native, wasm, reference })
    }

    fn disagrees(&self, case: &Case) -> Result<bool> {
        self.within_budget()?;
        Ok(!self.decide(case)?.agree())
    }

    /// Shrink until no single field removal, string shortening or byte removal keeps the
    /// disagreement.
    fn shrink(&self, mut case: Case) -> Result<Case> {
        loop {
            let candidates: Vec<Case> = match &case {
                Case::Json(v) => removals(v).into_iter().chain(shortenings(v)).map(Case::Json).collect(),
                Case::Bytes(b) => (0..b.len())
                    .map(|i| {
                        let mut c = b.clone();
                        c.remove(i);
                        Case::Bytes(c)
                    })
                    .collect(),
            };
            let mut next = None;
            for candidate in candidates {
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
        let decisions = self.decide(&minimized)?;
        let drift = self.drift(&minimized, &decisions, &format!("seed {seed}, case {} ({})", g.index, g.class.as_str()));
        let (case, bytes) = entry_forms(&minimized);
        let (original, original_bytes) = entry_forms(&g.case);
        let entry = Entry {
            case,
            bytes,
            engine: decisions.native,
            wasm: decisions.wasm,
            reference: decisions.reference,
            seed: Some(seed),
            index: Some(g.index),
            class: Some(g.class.as_str().to_string()),
            original: (!original.is_null()).then_some(original),
            original_bytes,
        };
        let written = self.append(entry).and_then(|()| load_corpus(&self.corpus));
        match written {
            Ok(entries) if entries.iter().any(|e| e.case() == minimized) => Ok(drift.into()),
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

    fn drift(&self, case: &Case, d: &Decisions, origin: &str) -> DifferentialError {
        let wasm = d.wasm.as_ref().map_or_else(|| "not built".to_string(), render);
        let reference = d.reference.as_ref().map_or_else(|| "not consulted on a credential case".to_string(), render);
        DifferentialError::ReferenceModelDrift(format!(
            "the native build, the WebAssembly build and the reference model disagree on {}\n  minimized case  {case}\n  native build    {}\n  wasm build      {wasm}\n  reference       {}\n  origin          {origin}\n  corpus          {}",
            d.differing().join(", "),
            render(&d.native),
            reference,
            self.corpus.display()
        ))
    }
}
