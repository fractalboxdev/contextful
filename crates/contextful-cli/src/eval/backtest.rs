//! `contextful eval backtest` and `contextful eval replay` (`assurance.backtest`,
//! `assurance.forecast`): a backtest case file runs end to end — each case lands its corpus
//! into its own scratch store, plants a row past its horizon in every table declaring an
//! availability time and its universe's delisting return, reads through the tool server
//! under its horizon into a signed audit chain, and the harness core scores the recording.
//! A replay re-serves the recorded answers, re-verifies the chain and its signed root, and
//! recomputes the report's digest without reading any store.

use super::{land_corpus, Corpus, SCRATCH_PROJECT};
use crate::admit::AdmitArgs;
use crate::clock::SystemClock;
use crate::context::read_rows;
use anyhow::{bail, Context, Result};
use contextful_agent::mcp::Server;
use contextful_context::fold::fold;
use contextful_context::land::{land, Batch, RunContext};
use contextful_context::read::Face;
use contextful_context::{node, Store};
use contextful_core::connector::model_ref::ModelRef;
use contextful_core::ports::Clock;
use contextful_core::run::journal::sha256_hex;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::reserve::Injection;
use contextful_core::time::Instant;
use contextful_eval::backtest::{execution_block, BacktestStamp, ExecutionComponent};
use contextful_eval::backtest_run::{self, assess, planted_row, CaseSpec, RecordedCall, Recording};
use contextful_eval::embed::{StubEmbedder, DEFAULT_SEED};
use contextful_eval::leak::StubLeakJudge;
use contextful_policy::audit::{self, AuditLog, SignedRoot};
use contextful_policy::enforce::mask::Pepper;
use contextful_policy::issue::{SeedSigner, SignerKey};
use contextful_policy::verify::{effect_boundary, Admission, AdmittedAuthority};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The files a run directory holds.
const CASES_FILE: &str = "cases.jsonl";
const CALLS_FILE: &str = "calls.jsonl";
const MANIFESTS_FILE: &str = "manifests.json";
const REPORT_FILE: &str = "report.json";
const AUDIT_DIR: &str = "audit";

#[derive(clap::Args)]
pub struct BacktestArgs {
    /// The backtest case file: JSON Lines, one case per line.
    #[arg(long)]
    cases: PathBuf,
    /// The run directory the recording, the audit chain and the report are written to.
    #[arg(long)]
    out: PathBuf,
    /// The issuer seed signing the run's audit chain and its root.
    #[arg(long)]
    issuer_key: PathBuf,
    /// The execution component: a JSON file of `{digest, parameters}`
    /// (`assurance.backtest.execution-component`).
    #[arg(long)]
    execution: Option<PathBuf>,
    /// The seed the stub embedder derives from.
    #[arg(long, default_value_t = DEFAULT_SEED)]
    seed: u64,
    /// The zone this process declares for each session.
    #[arg(long)]
    zone: Option<String>,
    #[command(flatten)]
    admit: AdmitArgs,
}

#[derive(clap::Args)]
pub struct ReplayArgs {
    /// The run directory `contextful eval backtest --out` wrote.
    run: PathBuf,
}

fn jsonl<T: serde::Serialize>(items: &[T]) -> Result<String> {
    Ok(items.iter().map(serde_json::to_string).collect::<Result<Vec<_>, _>>()?.join("\n") + "\n")
}

/// Land `rows` into `decl`'s table as run `run_id` at `at`, through the corpus landing path.
fn land_planted(store: &Store, decl: &TableDecl, rows: Vec<Map<String, Value>>, run_id: &str, at: Instant) -> Result<()> {
    let (node, _) = node::resolve(store, |k| std::env::var(k).ok())?;
    let ctx = RunContext {
        node,
        injection: Injection { run_id: run_id.into(), site_id: "eval".into(), batch_seq: Some(0), authored_by: None, authored_agent: None, taint: None },
        committed_at: at,
    };
    land(store, decl, &Batch { rows, types: Default::default() }, &ctx).with_context(|| format!("planting in `{}`", decl.name))?;
    let outcome = fold(store, decl, at)?;
    if outcome.is_failure() {
        bail!("folding `{}`: {outcome}", decl.name);
    }
    Ok(())
}

/// Plant the case's rows: one past its horizon in every table declaring an availability
/// time (`assurance.backtest.planted-future`), and its universe's delisting return
/// (`assurance.backtest.planted-delisting`). Returns each planted table.
fn plant(store: &Store, corpus: &Corpus, case: &CaseSpec, at: Instant) -> Result<Vec<String>> {
    let mut planted = Vec::new();
    for (table, path) in corpus.tables()? {
        let decl = corpus.decl(&table);
        let Some(template) = read_rows(&path)?.into_iter().next() else { continue };
        if let Some(row) = planted_row(&decl, &template, case) {
            land_planted(store, &decl, vec![row], "eval-planted", at)?;
            planted.push(table);
        }
    }
    if let Some(d) = &case.delisting {
        land_planted(store, &corpus.decl(&d.table), vec![d.row.clone()], "eval-delisting", at)?;
    }
    Ok(planted)
}

/// The report's run block (`assurance.backtest.run-stamp`): the deterministic tier's stub
/// models, its seed, and the digest of the empty prompt it sends.
fn stamp(seed: u64) -> BacktestStamp {
    let stub = |name: &str| ModelRef::snapshot(name);
    BacktestStamp {
        reader: stub("stub-reader"),
        judge: stub("stub-judge"),
        derive: stub("stub-derive"),
        embedder: stub("stub-embedder"),
        temperature: 0.0,
        seed,
        prompt_digest: sha256_hex(b""),
        tool_schema_version: env!("CARGO_PKG_VERSION").into(),
    }
}

pub fn backtest(a: BacktestArgs) -> Result<()> {
    let text = std::fs::read_to_string(&a.cases).with_context(|| format!("reading `{}`", a.cases.display()))?;
    let cases = backtest_run::load(&text).map_err(|e| anyhow::anyhow!("`{}`: {e}", a.cases.display()))?;
    // Every refusal of a case lands before any read (`assurance.backtest.label-interval`,
    // `assurance.forecast.resolved-before-freeze`), as does an undeclared execution component.
    for case in &cases {
        case.bounded()?;
    }
    let component: Option<ExecutionComponent> = match &a.execution {
        Some(p) => Some(serde_json::from_str(&std::fs::read_to_string(p).with_context(|| format!("reading `{}`", p.display()))?)?),
        None => None,
    };
    let execution = execution_block(&a.out.display().to_string(), component.as_ref())?;

    std::fs::create_dir_all(&a.out)?;
    let audit_dir = a.out.join(AUDIT_DIR);
    if audit_dir.exists() {
        bail!("`{}` already holds a run", a.out.display());
    }
    let log = AuditLog::anchor(&audit_dir, SeedSigner::resolve(Some(&a.issuer_key))?)?;
    let signer = SeedSigner::resolve(Some(&a.issuer_key))?;
    let (authority, revocation) = a.admit.admit(None, "the backtest run")?;
    let clock = SystemClock;
    let boundary = |authority: &AdmittedAuthority| effect_boundary(authority, &Admission::new(clock.now(), &revocation));
    let embedder = StubEmbedder::new(a.seed);
    let pepper = Pepper::resolve(|k| std::env::var(k).ok());
    let landed_at = Instant::from_unix_nanos(0)?;

    let mut corpora: BTreeMap<String, Corpus> = BTreeMap::new();
    let mut calls = Vec::new();
    let mut planted = BTreeMap::new();
    for case in &cases {
        let dir = a.cases.parent().unwrap_or(Path::new(".")).join(&case.corpus);
        if !corpora.contains_key(&case.corpus) {
            corpora.insert(case.corpus.clone(), Corpus::open(&dir)?);
        }
        let corpus = &corpora[&case.corpus];
        // Each case reads its own store, so no other case's planted row reaches it.
        let scratch = tempfile::tempdir()?;
        let store = Store::open(scratch.path(), SCRATCH_PROJECT)?;
        land_corpus(corpus, &store, Some(&embedder), landed_at)?;
        planted.insert(case.id.clone(), plant(&store, corpus, case, landed_at)?);
        let face = Face::open(store, &corpus.manifest, pepper.clone())?;
        let server = Server::new(&face, authority.clone(), &boundary, &clock, &log).map_err(anyhow::Error::msg)?.with_build(crate::build_identity());
        let bounded = case.bounded()?;
        for (index, read) in case.reads.iter().enumerate() {
            let arguments = bounded.read_arguments(read.arguments.clone());
            let before = log.tip().seq;
            let message = json!({ "jsonrpc": "2.0", "id": index + 1, "method": "tools/call", "params": { "name": read.tool, "arguments": arguments } });
            let answer = server.handle(&message.to_string()).unwrap_or(Value::Null);
            let answer = answer.get("result").or_else(|| answer.get("error")).cloned().unwrap_or(Value::Null);
            let after = log.tip().seq;
            let seq = (after > before).then_some(after);
            calls.push(RecordedCall { case: case.id.clone(), index, tool: read.tool.clone(), arguments, answer: answer.get("structuredContent").cloned().unwrap_or(answer), seq });
        }
    }

    // The signed root covering every entry the run appended (`assurance.backtest.replay-root`).
    let entries = audit::entries(&audit_dir)?;
    let last = entries.last().map(|e| e.entry_hash.clone()).unwrap_or_default();
    let root = SignedRoot::sign(&last, entries.len() as u64, &signer).map_err(anyhow::Error::msg)?;
    let replay_root = json!({ "root": root.root, "count": root.count, "signature": root.signature, "key": SignerKey::of(&signer).to_string() });

    let manifests: BTreeMap<String, String> = corpora.iter().map(|(k, c)| (k.clone(), c.manifest.clone())).collect();
    let decls: BTreeMap<String, Vec<TableDecl>> = corpora.into_iter().map(|(k, c)| (k, c.decls)).collect();
    let by_seq: BTreeMap<u64, Value> = entries.into_iter().map(|e| (e.seq, e.attributes)).collect();
    let mut report = assess(&Recording { cases: &cases, decls: &decls, calls: &calls, entries: &by_seq, judge: &StubLeakJudge }).map_err(anyhow::Error::msg)?;
    report["run"] = serde_json::to_value(stamp(a.seed))?;
    report["execution"] = execution;
    report["planted"] = json!(planted);
    report["replay_root"] = replay_root;
    report["digest"] = json!(backtest_run::digest(&report));

    std::fs::write(a.out.join(CASES_FILE), &text)?;
    std::fs::write(a.out.join(CALLS_FILE), jsonl(&calls)?)?;
    std::fs::write(a.out.join(MANIFESTS_FILE), serde_json::to_string_pretty(&manifests)?)?;
    let rendered = serde_json::to_string_pretty(&report)? + "\n";
    std::fs::write(a.out.join(REPORT_FILE), &rendered)?;
    print!("{rendered}");
    let failed = report["cases"].as_array().map_or(0, |c| c.iter().filter(|c| c["verdict"] == "fail").count());
    if failed > 0 {
        bail!("{failed} backtest case(s) failed");
    }
    Ok(())
}

/// Re-serve a run's recorded answers, re-verify its audit chain and signed root, and
/// recompute its report's digest (`assurance.backtest.trajectory-replay`).
pub fn replay(a: ReplayArgs) -> Result<()> {
    let read = |name: &str| std::fs::read_to_string(a.run.join(name)).with_context(|| format!("reading `{}`", a.run.join(name).display()));
    let cases = backtest_run::load(&read(CASES_FILE)?).map_err(anyhow::Error::msg)?;
    let recorded: Vec<RecordedCall> = read(CALLS_FILE)?.lines().filter(|l| !l.trim().is_empty()).map(serde_json::from_str).collect::<Result<_, _>>()?;
    let manifests: BTreeMap<String, String> = serde_json::from_str(&read(MANIFESTS_FILE)?)?;
    let report: Value = serde_json::from_str(&read(REPORT_FILE)?)?;

    // Each case's reads, in order, answered from the recording alone; a request the
    // recording does not hold diverges.
    let mut calls = Vec::new();
    for case in &cases {
        let bounded = case.bounded()?;
        for (index, read) in case.reads.iter().enumerate() {
            let arguments = bounded.read_arguments(read.arguments.clone());
            let served = recorded
                .iter()
                .find(|c| c.case == case.id && c.index == index)
                .with_context(|| format!("the recording holds no call {index} of `{}`", case.id))?;
            if served.tool != read.tool || served.arguments != arguments {
                bail!("call {index} of `{}` diverges from its recording", case.id);
            }
            calls.push(served.clone());
        }
    }
    if calls.len() != recorded.len() {
        bail!("the recording holds {} call(s) the case file never makes", recorded.len() - calls.len());
    }

    // The chain verifies, and the signed root names its last entry over its count.
    let audit_dir = a.run.join(AUDIT_DIR);
    audit::verify(&audit_dir)?;
    let entries = audit::entries(&audit_dir)?;
    let r = &report["replay_root"];
    let key: SignerKey = r["key"].as_str().unwrap_or_default().parse().map_err(anyhow::Error::msg)?;
    let root = SignedRoot { root: r["root"].as_str().unwrap_or_default().into(), count: r["count"].as_u64().unwrap_or_default(), signature: r["signature"].as_str().unwrap_or_default().into(), ..SignedRoot::default() };
    if !root.verify(&key) {
        bail!("the report's replay root carries no valid signature under `{key}`");
    }
    if root.count != entries.len() as u64 || entries.last().map(|e| e.entry_hash.as_str()).unwrap_or_default() != root.root {
        bail!("the replay root covers {} entries ending `{}`; the chain holds {}", root.count, root.root, entries.len());
    }

    let decls: BTreeMap<String, Vec<TableDecl>> = manifests.iter().map(|(k, m)| Ok((k.clone(), TableDecl::parse_pipeline(m)?))).collect::<Result<_>>()?;
    let by_seq: BTreeMap<u64, Value> = entries.into_iter().map(|e| (e.seq, e.attributes)).collect();
    let mut replayed = assess(&Recording { cases: &cases, decls: &decls, calls: &calls, entries: &by_seq, judge: &StubLeakJudge }).map_err(anyhow::Error::msg)?;
    for carried in ["run", "execution", "planted", "replay_root"] {
        replayed[carried] = report[carried].clone();
    }
    let digest = backtest_run::digest(&replayed);
    println!("{}", json!({ "digest": digest, "recorded": report["digest"] }));
    if json!(digest) != report["digest"] {
        bail!("the replayed report's digest {digest} differs from the recorded {}", report["digest"]);
    }
    Ok(())
}
