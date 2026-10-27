//! Seeded interleavings compare the executable journal model with the memory, file and
//! SQLite journal adapters after every step, each caller running the engine's own
//! `Journal::step` over the adapter.

use anyhow::{bail, Context, Result};
use clap::Args as ClapArgs;
use contextful_core::run::ports::{BlobStore, JournalStore};
use contextful_engine::conformance::interleave::{Interleaving, Outcome, Step, CALLERS, KEYS};
use contextful_engine::stores::{FileBlobStore, FileJournalStore, MemoryBlobStore, MemoryJournalStore};
use proptest::prelude::{any, prop_oneof, Just, Strategy};
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const STEPS_PER_CASE: usize = 40;
const DEFAULT_SEED: u64 = 51_337;
const REGRESSIONS: &str = "formal/protocol/journal-regressions.jsonl";
const ADAPTERS: [&str; 3] = ["memory", "file", "sqlite"];

#[derive(ClapArgs)]
pub struct Args {
    /// The generator seed, printed with the result.
    #[arg(long, default_value_t = DEFAULT_SEED)]
    seed: u64,
    /// Generated interleavings after replaying regressions.
    #[arg(long, default_value_t = 32)]
    cases: usize,
    /// Drop the grace hypothesis: replay the model's shortest sequence to `BlobMissing` on
    /// every adapter and save it.
    #[arg(long)]
    no_grace: bool,
    /// A prebuilt protocol executable; absent, lake builds the package.
    #[arg(long)]
    reference: Option<PathBuf>,
    /// The saved minimized sequences.
    #[arg(long)]
    regressions: Option<PathBuf>,
}

/// A saved sequence and the mode it runs in.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Case {
    grace: bool,
    steps: Vec<String>,
}

fn strategy() -> impl Strategy<Value = Vec<Step>> {
    let caller = 0..CALLERS;
    let outcome = prop_oneof![Just(Outcome::Inline), Just(Outcome::Blob), Just(Outcome::Fail), Just(Outcome::Unjournaled)];
    let step = prop_oneof![
        3 => (caller.clone(), 0..KEYS).prop_map(|(caller, key)| Step::Begin { caller, key }),
        8 => caller.clone().prop_map(|caller| Step::Advance { caller }),
        3 => (caller.clone(), outcome).prop_map(|(caller, outcome)| Step::Effect { caller, outcome }),
        1 => caller.clone().prop_map(|caller| Step::Note { caller }),
        1 => caller.clone().prop_map(|caller| Step::Lapse { caller }),
        1 => caller.clone().prop_map(|caller| Step::Crash { caller }),
        1 => caller.prop_map(|caller| Step::Finish { caller }),
        1 => any::<bool>().prop_map(|aged| Step::Sweep { aged }),
        1 => (0..KEYS).prop_map(|key| Step::Retire { key }),
    ];
    proptest::collection::vec(step, 1..=STEPS_PER_CASE)
}

fn generate(runner: &mut TestRunner) -> Result<Case> {
    let tree = strategy().new_tree(runner).map_err(|e| anyhow::anyhow!("generating a journal case: {e}"))?;
    Ok(Case { grace: true, steps: tree.current().iter().map(Step::line).collect() })
}

/// The model's state after each step of `case`.
fn model(reference: &Path, case: &Case) -> Result<Vec<String>> {
    let mut cmd = Command::new(reference);
    cmd.args(["journal", "run"]);
    if !case.grace {
        cmd.arg("--no-grace");
    }
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().with_context(|| format!("starting {}", reference.display()))?;
    {
        let stdin = child.stdin.as_mut().context("journal model stdin")?;
        for step in &case.steps {
            writeln!(stdin, "{step}")?;
        }
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("journal model: {}", String::from_utf8_lossy(&out.stderr));
    }
    let states: Vec<String> = String::from_utf8(out.stdout)?.lines().map(str::to_string).collect();
    if states.len() != case.steps.len() {
        bail!("journal model returned {} states for {} steps", states.len(), case.steps.len());
    }
    Ok(states)
}

/// The adapter's state after each step of `case`.
fn replay<J: JournalStore + Clone + 'static, B: BlobStore + Clone + 'static>(rows: J, blobs: B, case: &Case) -> Result<Vec<String>> {
    let mut run = Interleaving::new(rows, blobs, case.grace);
    let mut out = Vec::new();
    for line in &case.steps {
        run.apply(Step::parse(line).with_context(|| format!("unrecognised journal step `{line}`"))?);
        out.push(run.show());
    }
    Ok(out)
}

fn adapter(name: &str, case: &Case) -> Result<Vec<String>> {
    match name {
        "memory" => replay(MemoryJournalStore::new(), MemoryBlobStore::new(), case),
        "file" => {
            let dir = tempfile::tempdir()?;
            replay(FileJournalStore::open(dir.path()), FileBlobStore::open(dir.path()), case)
        }
        "sqlite" => {
            let dir = tempfile::tempdir()?;
            let stores = contextful_sqlite::SqliteRunStores::open(&dir.path().join("run.db")).map_err(|f| anyhow::anyhow!("opening the SQLite run store: {}", f.message))?;
            replay(stores.journal, stores.blobs, case)
        }
        other => bail!("no journal adapter `{other}`"),
    }
}

/// The first step after which the model and the adapter differ, with both states.
fn compare(reference: &Path, name: &str, case: &Case) -> Result<Option<(usize, String, String)>> {
    let expected = model(reference, case)?;
    let actual = adapter(name, case)?;
    Ok(expected.into_iter().zip(actual).enumerate().find(|(_, (m, a))| m != a).map(|(i, (m, a))| (i, m, a)))
}

fn minimize(reference: &Path, name: &str, mut case: Case) -> Result<Case> {
    let mut i = 0;
    while i < case.steps.len() && case.steps.len() > 1 {
        let mut candidate = case.clone();
        candidate.steps.remove(i);
        if compare(reference, name, &candidate)?.is_some() {
            case = candidate;
            i = 0;
        } else {
            i += 1;
        }
    }
    Ok(case)
}

fn save(regressions: &Path, case: &Case) -> Result<String> {
    let entry = serde_json::to_string(case)?;
    let saved = if regressions.exists() { std::fs::read_to_string(regressions)? } else { String::new() };
    if !saved.lines().any(|line| line == entry) {
        if let Some(parent) = regressions.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(regressions)?;
        writeln!(file, "{entry}")?;
    }
    Ok(entry)
}

/// The model's shortest sequence to `BlobMissing` without the grace hypothesis.
fn blob_missing(reference: &Path) -> Result<Case> {
    let out = Command::new(reference).args(["journal", "check", "--no-grace"]).output().with_context(|| format!("starting {}", reference.display()))?;
    let text = String::from_utf8_lossy(&out.stderr);
    if out.status.success() || !text.contains("no caller reads BlobMissing") {
        bail!("the journal model without the grace hypothesis reached no `BlobMissing`: {text}");
    }
    let steps = text.lines().filter_map(|l| l.strip_prefix("  ")).map(str::to_string).collect();
    Ok(Case { grace: false, steps })
}

fn build(repo: &Path) -> Result<PathBuf> {
    let root = repo.join("formal/protocol");
    let out = Command::new("lake").arg("build").current_dir(&root).output().context("building the protocol package")?;
    if !out.status.success() {
        bail!("building the protocol package: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(root.join(".lake/build/bin/protocol"))
}

pub fn run(args: Args) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&cwd)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        .unwrap_or(cwd);
    let reference = match args.reference {
        Some(path) => path,
        None => build(&repo)?,
    };
    let regressions = args.regressions.unwrap_or_else(|| repo.join(REGRESSIONS));

    if args.no_grace {
        let case = blob_missing(&reference)?;
        for name in ADAPTERS {
            if let Some((at, model, store)) = compare(&reference, name, &case)? {
                bail!("JournalConformanceDrift: adapter={name} step={at}; sequence={}; model={model}; store={store}", serde_json::to_string(&case.steps)?);
            }
            let last = adapter(name, &case)?.pop().unwrap_or_default();
            if !last.contains(" missing ") {
                bail!("adapter `{name}` ended the sequence without `BlobMissing`: {last}");
            }
        }
        let entry = save(&regressions, &case)?;
        println!("journal-differential: BlobMissing reproduced without the grace hypothesis on {}; sequence={entry}", ADAPTERS.join(","));
        return Ok(());
    }

    let saved = if regressions.exists() { std::fs::read_to_string(&regressions)? } else { String::new() };
    let mut cases = Vec::new();
    for line in saved.lines().filter(|line| !line.trim().is_empty()) {
        cases.push(serde_json::from_str::<Case>(line)?);
    }
    let replayed = cases.len();
    let mut runner = TestRunner::new_with_rng(Config::default(), TestRng::from_seed(RngAlgorithm::ChaCha, &args.seed.to_le_bytes().repeat(4)));
    for _ in 0..args.cases {
        cases.push(generate(&mut runner)?);
    }
    for (n, case) in cases.into_iter().enumerate() {
        for name in ADAPTERS {
            if compare(&reference, name, &case)?.is_some() {
                let reduced = minimize(&reference, name, case)?;
                let (at, model, store) = compare(&reference, name, &reduced)?.context("the minimized journal sequence no longer reproduces the drift")?;
                save(&regressions, &reduced)?;
                bail!(
                    "JournalConformanceDrift: adapter={name} seed={} case={n} step={at}; sequence={}; model={model}; store={store}",
                    args.seed,
                    serde_json::to_string(&reduced.steps)?
                );
            }
        }
    }
    println!(
        "journal-differential: 0 drift cases; adapters={}; seed={}; generated={}; regressions={replayed}",
        ADAPTERS.join(","),
        args.seed,
        args.cases
    );
    Ok(())
}
