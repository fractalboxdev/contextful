//! `contextful eval` — the read-path quality harness's runner
//! (`assurance.evaluate.run-command`) and its golden-set verbs.
//!
//! `run` is a thin adapter: it loads the case file through the harness's one loader, lands
//! each referenced corpus into a scratch store through the landing and fold paths
//! `contextful context` uses, reads every case through the ranked call under one admitted
//! credential, has the harness core read and judge each case — through the stubs, or
//! through the operator's inference endpoint on the judged tier — and hands the returns to
//! the core for scoring and both verdicts. `draft` reads a project's entity, fact and edge
//! tables and writes the generators' candidates; `commit` appends approved candidates,
//! redacted, to a case file. No metric, floor, baseline or generator rule lives here.

use crate::admit::{face, AdmitArgs};
use crate::memory::INFERENCE_KEY_VAR;
use crate::project::locate;
use crate::context::read_rows;
use crate::clock::SystemClock;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use contextful_context::fold::fold;
use contextful_context::land::{land, Batch, RunContext};
use contextful_context::read::{Face, ReadOptions, RetrieveRequest};
use contextful_core::read::rank::Fusion;
use contextful_context::{node, Store};
use contextful_core::ports::Clock;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::relation::ident;
use contextful_core::store::reconcile::{ColumnType, FloatItem};
use contextful_core::store::reserve::Injection;
use contextful_core::time::Instant;
use contextful_eval::baseline::{self, Baselines, Outcome, RunStamp, Tier};
use contextful_eval::case::{self, Case, KEY_SEPARATOR};
use contextful_eval::checkpoint::{CaseResult, Checkpoint, Header};
use contextful_eval::embed::{StubEmbedder, DEFAULT_SEED};
use contextful_eval::floors;
use contextful_eval::golden::{self, GoldenRedaction, ShapeRows, StoreView};
use contextful_eval::judge::{self, Judge, ModelJudge, ModelReader, Reader, Retrieved, StubJudge, StubReader};
use contextful_eval::metrics::{Returned, RowRef, LEGS};
use contextful_eval::report::{self, CaseRun};
use contextful_eval::systems::{self, Systems};
use contextful_eval::trace::{self, RunRecord, TraceStore, Verdict};
use contextful_core::connector::attach::Allowlist;
use contextful_outbound::{Client, HeaderValue};
use contextful_eval::EvalError;
use contextful_policy::enforce::mask::Pepper;
use contextful_policy::enforce::session::{Request, Session};
use contextful_outbound::infer::Endpoint;
use contextful_policy::verify::{effect_boundary, Admission, AdmittedAuthority};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The corpus manifest a corpus directory holds (`assurance.evaluate.corpus-layout`).
const CORPUS_MANIFEST: &str = "contextful.toml";

/// The directory under a corpus holding one `<table>.jsonl` per table.
const CORPUS_ROWS: &str = "rows";

/// The project name each scratch store opens under.
const SCRATCH_PROJECT: &str = "eval";

/// The column a stored vector lands in, which the ranked call reads.
const EMBEDDING_COLUMN: &str = "embedding";

/// The zone prefix that admits an unlabeled corpus (`assurance.evaluate.unlabeled-corpus`).
const LOCAL_ZONE_PREFIX: &str = "local:";

/// Tokens the deterministic tier's stub reader holds in its context window.
const STUB_READER_CONTEXT_TOKENS: u64 = 128_000;

/// Seconds the trace export waits on its endpoint.
const TRACE_EXPORT_TIMEOUT_SECS: u64 = 5;

/// A deployed project's store root, beneath a corpus directory a run would read.
const DEPLOYED_STORE_ROOT: &str = ".contextful/context";

#[derive(Subcommand)]
pub enum EvalCmd {
    /// Run a case file through the ranked read and score it; exits non-zero on a red floor or baseline.
    Run(RunArgs),
    /// Draft candidate cases from a project's entity, fact and edge tables.
    Draft(DraftArgs),
    /// Append approved drafts, redacted, to a case file.
    Commit(CommitArgs),
}

/// The evaluation tier a run reads and judges under.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum RunTier {
    /// The stub reader and judge: no model call.
    Deterministic,
    /// The model reader and judge, through `--endpoint` serving `--model`.
    Judged,
}

#[derive(clap::Args)]
pub struct RunArgs {
    /// The case file: JSON Lines, one case per line.
    #[arg(long)]
    goldens: PathBuf,
    /// The rank cut-off every at-k metric uses, and the limit each ranked call passes.
    #[arg(long, default_value_t = 10)]
    k: usize,
    /// The seed the stub embedder derives from.
    #[arg(long, default_value_t = DEFAULT_SEED)]
    seed: u64,
    /// The zone this process declares for each session; a `local:` zone admits an unlabeled corpus.
    #[arg(long)]
    zone: Option<String>,
    /// The instant each corpus lands and folds at (RFC 3339); absent, the Unix epoch.
    #[arg(long)]
    landed_at: Option<String>,
    /// Write the run report here instead of standard output.
    #[arg(long)]
    report: Option<PathBuf>,
    /// The baseline file the report is held against beside the floors.
    #[arg(long)]
    baseline: Option<PathBuf>,
    /// On a green run, raise each improved baseline entry to its measured value.
    #[arg(long, requires = "baseline")]
    update_baseline: bool,
    /// Append each finished case's result to this JSON Lines file, and resume from the
    /// results it already holds.
    #[arg(long)]
    checkpoint: Option<PathBuf>,
    /// Tokens the reader's context window holds; the systems figures bucket each case's
    /// corpus size against it.
    #[arg(long, default_value_t = STUB_READER_CONTEXT_TOKENS)]
    context_window: u64,
    /// The trace store directory: the run's record joins its history, and the cases a
    /// curator reviews join its staging.
    #[arg(long)]
    trace_store: Option<PathBuf>,
    /// The trace collector the run's record is posted to; a hosted one serves runs over
    /// fixtures alone.
    #[arg(long)]
    trace_endpoint: Option<String>,
    /// The tier the reading stage runs under.
    #[arg(long, value_enum, default_value = "deterministic")]
    tier: RunTier,
    /// The OpenAI-compatible endpoint `/chat/completions` hangs off; the judged tier reads
    /// and judges through it.
    #[arg(long, required_if_eq("tier", "judged"))]
    endpoint: Option<String>,
    /// The pinned model the endpoint serves the reader and judge, stamped in the run block.
    #[arg(long, required_if_eq("tier", "judged"))]
    model: Option<String>,
    /// The lexical leg's calibration under evaluation: `reciprocal-rank`, the ranked read's,
    /// or `min-max`, the one it replaced (`read.rank.calibration-gate`).
    #[arg(long, default_value = "reciprocal-rank", value_parser = parse_fusion)]
    fusion: Fusion,
    #[command(flatten)]
    admit: AdmitArgs,
}

#[derive(clap::Args)]
pub struct DraftArgs {
    /// The project whose store holds the tables; absent, the nearest `contextful.toml` names it.
    #[arg(long)]
    project: Option<String>,
    /// The manifest declaring the tables; absent, the project's `contextful.toml`.
    #[arg(long)]
    declaration: Option<PathBuf>,
    /// The table holding entities: `entity_id` and `name`.
    #[arg(long)]
    entities: String,
    /// The table holding facts: `claim_id`, `subject`, `predicate` and `object`.
    #[arg(long)]
    facts: String,
    /// The table holding edges: `edge_id`, `source_id`, `rel_type` and `target_id`.
    #[arg(long)]
    edges: String,
    /// A name for an absent-entity question; repeatable. A name the store holds is skipped.
    #[arg(long = "decoy")]
    decoys: Vec<String>,
    /// The corpus path each drafted case records, relative to the case file it will join.
    #[arg(long)]
    corpus: String,
    /// The drafts file to write, one case per line.
    #[arg(long)]
    out: PathBuf,
    /// The zone this process declares for the read session.
    #[arg(long)]
    zone: Option<String>,
    #[command(flatten)]
    admit: AdmitArgs,
}

#[derive(clap::Args)]
pub struct CommitArgs {
    /// The drafts file `eval draft` wrote.
    #[arg(long)]
    drafts: PathBuf,
    /// `<case id>=<reviewer>` approving one draft; repeatable.
    #[arg(long = "approve")]
    approvals: Vec<String>,
    /// The case file the approved drafts append to.
    #[arg(long)]
    into: PathBuf,
    /// The manifest whose tables' removal rules redact each draft.
    #[arg(long)]
    declaration: PathBuf,
}

/// A corpus directory: its manifest and table declarations.
struct Corpus {
    dir: PathBuf,
    manifest: String,
    decls: Vec<TableDecl>,
}

impl Corpus {
    fn open(dir: &Path) -> Result<Corpus> {
        let path = dir.join(CORPUS_MANIFEST);
        let manifest = std::fs::read_to_string(&path).with_context(|| format!("reading the corpus manifest `{}`", path.display()))?;
        let decls = TableDecl::parse_pipeline(&manifest).with_context(|| format!("`{}`", path.display()))?;
        Ok(Corpus { dir: dir.to_path_buf(), manifest, decls })
    }

    /// Whether any table declares a zone or row policy.
    fn labeled(&self) -> Result<bool> {
        let doc: toml::Value = toml::from_str(&self.manifest)?;
        let tables = doc.get("pipeline").and_then(|p| p.get("tables")).and_then(toml::Value::as_array);
        Ok(tables.into_iter().flatten().filter_map(|t| t.get("policy")).any(|p| p.get("zone").is_some() || p.get("rows").is_some()))
    }

    fn decl(&self, table: &str) -> TableDecl {
        self.decls.iter().find(|d| d.name == table).cloned().unwrap_or_else(|| TableDecl::named(table))
    }

    /// Each `rows/<table>.jsonl` under the corpus, as `(table, path)`, in path order.
    fn tables(&self) -> Result<Vec<(String, PathBuf)>> {
        let root = self.dir.join(CORPUS_ROWS);
        let mut out = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).with_context(|| format!("reading `{}`", dir.display()))? {
                let path = entry?.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "jsonl") {
                    let rel = path.strip_prefix(&root)?.with_extension("");
                    let table = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
                    out.push((table, path));
                }
            }
        }
        out.sort();
        Ok(out)
    }
}

/// The text the stub embedder reads off a row: its string values outside the primary key,
/// in column order.
fn row_text(row: &Map<String, Value>, key: &[String]) -> String {
    row.iter()
        .filter(|(k, _)| !key.contains(k))
        .filter_map(|(_, v)| v.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Land every table of `corpus` into `store` at `at`, embedding each row that carries no
/// embedding, then fold each table so its declared sidecars build. Returns the corpus's
/// size in tokens.
fn land_corpus(corpus: &Corpus, store: &Store, embedder: &StubEmbedder, at: Instant) -> Result<u64> {
    let (node, _) = node::resolve(store, |k| std::env::var(k).ok())?;
    let mut corpus_tokens = 0;
    for (table, path) in corpus.tables()? {
        let decl = corpus.decl(&table);
        let mut rows = read_rows(&path)?;
        for row in &mut rows {
            corpus_tokens += systems::tokens(&row_text(row, decl.primary_key()));
            if !row.contains_key(EMBEDDING_COLUMN) {
                let v = embedder.embed(&row_text(row, decl.primary_key()));
                row.insert(EMBEDDING_COLUMN.into(), serde_json::to_value(v)?);
            }
        }
        let dim = rows.first().and_then(|r| r[EMBEDDING_COLUMN].as_array()).map_or(0, Vec::len);
        let mut types = HashMap::new();
        if dim > 0 {
            types.insert(EMBEDDING_COLUMN.to_string(), ColumnType::FixedSizeList(FloatItem::Float32, u32::try_from(dim)?));
        }
        let ctx = RunContext {
            node: node.clone(),
            injection: Injection { run_id: "eval-0001".into(), site_id: "eval".into(), batch_seq: Some(0), authored_by: None, taint: None },
            committed_at: at,
        };
        land(store, &decl, &Batch { rows, types }, &ctx).with_context(|| format!("landing `{}`", path.display()))?;
        let outcome = fold(store, &decl, at)?;
        if outcome.is_failure() {
            bail!("folding `{table}`: {outcome}");
        }
    }
    Ok(corpus_tokens)
}

/// A returned row's reference: its table and its declared primary-key values
/// (`assurance.evaluate.row-reference`).
fn row_ref(corpus: &Corpus, table: &str, row: &Value) -> RowRef {
    let key: Vec<String> = corpus
        .decl(table)
        .primary_key()
        .iter()
        .map(|c| match row.get(c) {
            Some(Value::String(s)) => s.clone(),
            Some(v) => v.to_string(),
            None => String::new(),
        })
        .collect();
    RowRef::new(table, key.join(KEY_SEPARATOR))
}

fn instant(field: &str, value: Option<&str>, default: Instant) -> Result<Instant> {
    value.map_or(Ok(default), |s| Instant::parse(s).with_context(|| format!("`{field}` is not an RFC 3339 instant: {s}")))
}

/// What reading one case yields: its return on each leg, and the tokens the reader reads —
/// the question and each row of the hybrid leg it is handed.
struct CaseRead {
    legs: BTreeMap<String, Vec<Returned>>,
    reader_tokens: u64,
    /// The hybrid leg's rows, top first, as the reader reads them.
    rows: Vec<Retrieved>,
}

fn parse_fusion(name: &str) -> std::result::Result<Fusion, String> {
    Fusion::parse(name).ok_or_else(|| format!("`{name}` is not `min-max` or `reciprocal-rank`"))
}

/// Read one case on every leg (`assurance.evaluate.legs`).
#[allow(clippy::too_many_arguments)]
fn read_case(
    face: &Face,
    session: &Session,
    corpus: &Corpus,
    case: &Case,
    k: usize,
    embedder: &StubEmbedder,
    landed_at: Instant,
    fusion: Fusion,
) -> Result<CaseRead> {
    let anchor = instant("time_anchor", case.expected.time_anchor.as_deref(), landed_at)?;
    let since = case.expected.since.as_deref().map(|s| instant("since", Some(s), landed_at)).transpose()?;
    let embedding = case.query_embedding.clone().unwrap_or_else(|| embedder.embed(&case.question));
    let prefix = case.prefix.clone().unwrap_or_default();
    let mut legs = BTreeMap::new();
    let mut reader_tokens = systems::tokens(&case.question);
    let mut rows = Vec::new();
    for leg in LEGS {
        let (query, query_embedding) = match leg {
            "lexical" => (case.question.as_str(), None),
            "vector" => ("", Some(embedding.clone())),
            _ => (case.question.as_str(), Some(embedding.clone())),
        };
        let request = RetrieveRequest {
            query_embedding,
            limit: Some(u64::try_from(k)?),
            since,
            fusion,
            ..RetrieveRequest::new(prefix.clone(), query, anchor)
        };
        let response = face.retrieve(session, &request, Bounds::default()).with_context(|| format!("case `{}`, {leg} leg", case.id))?;
        let at = |name: &str| response.columns.iter().position(|c| c == name).with_context(|| format!("the ranked response carries no `{name}`"));
        let (t, r, w) = (at("_table")?, at("_row")?, at("_in_window")?);
        let returned = response
            .rows
            .iter()
            .map(|row| {
                let table = row[t].as_str().unwrap_or_default();
                Returned { row: row_ref(corpus, table, &row[r]), in_window: row[w].as_bool().unwrap_or(false) }
            })
            .collect();
        if leg == "hybrid" {
            for row in &response.rows {
                let table = row[t].as_str().unwrap_or_default();
                if let Some(values) = row[r].as_object() {
                    let text = row_text(values, corpus.decl(table).primary_key());
                    reader_tokens += systems::tokens(&text);
                    rows.push(Retrieved { row: row_ref(corpus, table, &row[r]), text, in_window: row[w].as_bool().unwrap_or(false) });
                }
            }
        }
        legs.insert(leg.to_string(), returned);
    }
    Ok(CaseRead { legs, reader_tokens, rows })
}

fn session(face: &Face, authority: &AdmittedAuthority, revocation: &contextful_policy::revoke::RevocationState, zone: Option<&str>) -> Result<Session> {
    effect_boundary(authority, &Admission::new(SystemClock.now(), revocation))?;
    Ok(face.session(authority, &Request { zone }, Bounds::default())?)
}

pub fn run(cmd: EvalCmd) -> Result<()> {
    match cmd {
        EvalCmd::Run(a) => run_cases(a),
        EvalCmd::Draft(a) => draft(a),
        EvalCmd::Commit(a) => commit(a),
    }
}

fn run_cases(a: RunArgs) -> Result<()> {
    let text = std::fs::read_to_string(&a.goldens).with_context(|| format!("reading `{}`", a.goldens.display()))?;
    let cases = case::load(&text).map_err(|e| anyhow::anyhow!("`{}`: {e}", a.goldens.display()))?;
    // The judged tier reaches the operator's endpoint; the host holds its key
    // (`assurance.evaluate.model-endpoint`).
    let endpoint = match (a.tier, &a.endpoint, &a.model) {
        (RunTier::Judged, Some(url), Some(model)) => {
            Some((Endpoint::new(url, model, std::env::var(INFERENCE_KEY_VAR).ok()).map_err(anyhow::Error::msg)?, model.clone()))
        }
        _ => None,
    };
    let (reader, judge, stamp): (Box<dyn Reader + '_>, Box<dyn Judge + '_>, RunStamp) = match &endpoint {
        Some((e, model)) => {
            let judge = ModelJudge::new(e, model.clone());
            let stamp = judge.stamp(a.k);
            (Box::new(ModelReader::new(e)), Box::new(judge), stamp)
        }
        None => (Box::new(StubReader), Box::new(StubJudge), StubJudge.stamp(a.k)),
    };
    debug_assert!(stamp.tier == if endpoint.is_some() { Tier::Judged } else { Tier::Deterministic });
    // Every baseline refusal needing no report lands before any corpus does.
    let baselines = match &a.baseline {
        Some(path) => {
            let raw = std::fs::read_to_string(path).with_context(|| format!("reading the baseline `{}`", path.display()))?;
            let b = Baselines::parse(&raw)?;
            b.check_run(&stamp)?;
            Some(b)
        }
        None => None,
    };

    let mut corpora: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
    for (i, c) in cases.iter().enumerate() {
        corpora.entry(c.corpus_dir(&a.goldens)).or_default().push(i);
    }
    let local = a.zone.as_deref().is_some_and(|z| z.starts_with(LOCAL_ZONE_PREFIX));
    let mut opened = Vec::with_capacity(corpora.len());
    for (dir, idx) in corpora {
        let corpus = Corpus::open(&dir)?;
        if !corpus.labeled()? && !local {
            return Err(EvalError::EvalCorpusUnlabeled { corpus: dir.display().to_string() }.into());
        }
        opened.push((corpus, idx));
    }
    // A corpus directory holding a project's store root is a deployed store; a hosted
    // trace endpoint refuses such a run before any row lands (`assurance.baseline.trace-export`).
    let deployed: Vec<String> = opened
        .iter()
        .map(|(c, _)| c.dir.join(DEPLOYED_STORE_ROOT))
        .filter(|p| p.is_dir())
        .map(|p| p.display().to_string())
        .collect();
    trace::admit_export(a.trace_endpoint.as_deref(), &deployed)?;

    let (authority, revocation) = a.admit.admit(None, "the evaluation run")?;
    let landed_at = instant("--landed-at", a.landed_at.as_deref(), Instant::from_unix_nanos(0)?)?;
    let embedder = StubEmbedder::new(a.seed);
    let pepper = Pepper::resolve(|k| std::env::var(k).ok());
    if let Some(signal) = pepper.signal() {
        eprintln!("{signal}");
    }
    // A checkpoint resumes the cases it holds whole; the case a crash interrupted runs again
    // (`assurance.evaluate.checkpoint`).
    let (mut checkpoint, mut done) = match &a.checkpoint {
        Some(path) => {
            let (cp, done) = Checkpoint::open(path, &Header { run: stamp.clone(), seed: a.seed })?;
            if let Some(stale) = done.keys().find(|id| !cases.iter().any(|c| &c.id == *id)) {
                bail!("checkpoint `{}` holds case `{stale}`, which the case file does not carry", path.display());
            }
            eprintln!("eval: resuming {} finished case(s) from `{}`", done.len(), path.display());
            (Some(cp), done)
        }
        None => (None, BTreeMap::new()),
    };
    let mut results: Vec<Option<CaseResult>> = cases.iter().map(|c| done.remove(&c.id)).collect();
    for (corpus, idx) in &opened {
        if idx.iter().all(|&i| results[i].is_some()) {
            continue;
        }
        let scratch = tempfile::tempdir()?;
        let store = Store::open(scratch.path(), SCRATCH_PROJECT)?;
        let corpus_tokens = land_corpus(corpus, &store, &embedder, landed_at)?;
        let face = Face::open(store, &corpus.manifest, pepper.clone())?;
        let s = session(&face, &authority, &revocation, a.zone.as_deref())?;
        for &i in idx {
            if results[i].is_some() {
                continue;
            }
            let started = std::time::Instant::now();
            let read = read_case(&face, &s, corpus, &cases[i], a.k, &embedder, landed_at, a.fusion)?;
            let systems = Systems {
                tokens: read.reader_tokens,
                latency_ms: started.elapsed().as_secs_f64() * 1000.0,
                // The deterministic tier calls no model.
                cost: 0.0,
                corpus_tokens,
                context_window: a.context_window,
            };
            let judged = judge::judge_case(&cases[i], &read.rows, reader.as_ref(), judge.as_ref()).map_err(anyhow::Error::msg)?;
            let result = CaseResult { id: cases[i].id.clone(), legs: read.legs, edges: None, systems: Some(systems), judged: Some(judged) };
            if let Some(cp) = &mut checkpoint {
                cp.append(&result)?;
            }
            results[i] = Some(result);
        }
    }
    let mut judged = Vec::with_capacity(cases.len());
    let mut runs: Vec<CaseRun<'_>> = Vec::with_capacity(cases.len());
    for (case, r) in cases.iter().zip(results) {
        let r = r.expect("every case read or resumed");
        judged.push(r.judged.with_context(|| format!("case `{}` carries no judged result; its checkpoint predates the reading stage", case.id))?);
        runs.push(CaseRun { case, legs: r.legs, edges: r.edges, systems: r.systems });
    }
    let mut report = report::build(&stamp, a.seed, &runs);
    report["judge"] = judge::fold(&judged, a.seed).to_json();
    let text = serde_json::to_string_pretty(&report)? + "\n";
    match &a.report {
        Some(path) => std::fs::write(path, &text).with_context(|| format!("writing the report `{}`", path.display()))?,
        None => print!("{text}"),
    }
    verdict(&report, &cases, baselines, &a)
}

/// Record the run in the trace store and export its record to the trace endpoint, each
/// when configured. Both follow the verdicts and decide none: a failure prints and the
/// run's outcome stands (`assurance.baseline.trace-store`).
fn trace(report: &Value, cases: &[Case], floors: Verdict, baseline: Option<Verdict>, a: &RunArgs) {
    if a.trace_store.is_none() && a.trace_endpoint.is_none() {
        return;
    }
    let record = match RunRecord::of(report, floors, baseline) {
        Ok(r) => r,
        Err(e) => return eprintln!("eval: trace: {e}"),
    };
    if let Some(dir) = &a.trace_store {
        let staged = trace::staged(cases, report);
        match TraceStore::open(dir).and_then(|s| s.record(&record, &staged)) {
            Ok(()) => eprintln!("eval: trace store `{}`: recorded the run, staged {} case(s)", dir.display(), staged.len()),
            Err(e) => eprintln!("eval: trace store `{}` recorded nothing: {e}", dir.display()),
        }
    }
    if let Some(endpoint) = &a.trace_endpoint {
        match export_trace(endpoint, &record) {
            Ok(status) => eprintln!("eval: trace endpoint answered {status}"),
            Err(e) => eprintln!("eval: trace endpoint `{endpoint}` received nothing: {e}"),
        }
    }
}

/// POST the run record as JSON to the trace endpoint.
fn export_trace(endpoint: &str, record: &RunRecord) -> Result<u16> {
    let url = url::Url::parse(endpoint)?;
    let allow = Allowlist::parse(&[url.host_str().unwrap_or_default()])?;
    let client = Client::new(allow, url.clone()).with_timeout(std::time::Duration::from_secs(TRACE_EXPORT_TIMEOUT_SECS));
    let headers = [("Content-Type".to_string(), HeaderValue::Plain("application/json".to_string()))];
    let body = serde_json::to_vec(record)?;
    let response = client.send_once("POST", &url, &headers, Some(&body)).map_err(|f| anyhow::anyhow!(f.message))?;
    Ok(response.status)
}

/// Hold the report to the floors and the baseline, print both verdicts, and raise the
/// baseline on green when asked (`assurance.evaluate.run-verdict`).
fn verdict(report: &Value, cases: &[Case], baselines: Option<Baselines>, a: &RunArgs) -> Result<()> {
    let floors = floors::check(report);
    let mut red: Vec<String> = floors.breaches().map(|c| format!("floor {}: {} against {:?} {}", c.path, c.measured, c.side, c.bound)).collect();
    let floor_red = red.len();
    let mut compared = None;
    if let Some(b) = baselines {
        let v = baseline::gate(report, &b)?;
        for c in &v.comparisons {
            eprintln!("eval: {} = {} against {} (band {}): {:?}", c.path, c.current, c.baseline, c.band, c.outcome);
            if c.outcome == Outcome::Regressed {
                red.push(format!("baseline {}: {} regressed from {}", c.path, c.current, c.baseline));
            }
        }
        compared = Some((b, v));
    }
    // Both verdicts come from the report and the in-tree files alone (`assurance.baseline.offline`).
    eprintln!("eval: floor verdict: {} ({} floor(s), {floor_red} breached)", if floor_red == 0 { "held" } else { "red" }, floors.checks.len());
    match &a.baseline {
        Some(path) => eprintln!(
            "eval: baseline verdict: {} against `{}` ({} regressed)",
            if red.len() == floor_red { "held" } else { "red" },
            path.display(),
            red.len() - floor_red
        ),
        None => eprintln!("eval: baseline verdict: none, no --baseline"),
    }
    let baseline_verdict = a.baseline.as_ref().map(|_| Verdict::of(red.len() == floor_red));
    trace(report, cases, Verdict::of(floor_red == 0), baseline_verdict, a);
    if !red.is_empty() {
        red.iter().for_each(|r| eprintln!("eval: red: {r}"));
        bail!("the evaluation run is red: {} breach(es)", red.len());
    }
    eprintln!("eval: green: {} floor(s) hold", floors.checks.len());
    if let (true, Some((mut b, v)), Some(path)) = (a.update_baseline, compared, &a.baseline) {
        let moved = b.raise(&v).unwrap_or_default();
        if !moved.is_empty() {
            std::fs::write(path, b.to_json()).with_context(|| format!("writing the baseline `{}`", path.display()))?;
        }
        eprintln!("eval: raised {} baseline entr(ies): {}", moved.len(), moved.join(", "));
    }
    Ok(())
}

/// Every row of `table`, read through `session`.
fn table_rows(face: &Face, session: &Session, table: &str) -> Result<Vec<Map<String, Value>>> {
    let response = face.query(session, &format!("SELECT * FROM {}", ident(table)), ReadOptions::default()).with_context(|| format!("reading `{table}`"))?;
    Ok(response
        .rows
        .iter()
        .map(|row| response.columns.iter().cloned().zip(row.iter().cloned()).collect())
        .collect())
}

/// `contextful eval draft`: read the three tables under one admitted session, run the
/// generators, and write every candidate (`assurance.baseline.draft-command`).
fn draft(a: DraftArgs) -> Result<()> {
    let (authority, revocation) = a.admit.admit(a.project.as_deref(), "drafting golden candidates")?;
    let located = locate(a.project.as_deref(), a.declaration.clone())?;
    let face = face(&located)?;
    let session = session(&face, &authority, &revocation, a.zone.as_deref())?;
    let manifest = std::fs::read_to_string(&located.declaration).with_context(|| format!("reading `{}`", located.declaration.display()))?;
    let decls = TableDecl::parse_pipeline(&manifest)?;
    let key = |table: &str| decls.iter().find(|d| d.name == table).map(|d| d.primary_key().to_vec()).unwrap_or_default();
    let (entities, facts, edges) = (table_rows(&face, &session, &a.entities)?, table_rows(&face, &session, &a.facts)?, table_rows(&face, &session, &a.edges)?);
    let (ek, fk, gk) = (key(&a.entities), key(&a.facts), key(&a.edges));
    let view = StoreView::from_memory(
        ShapeRows { table: &a.entities, key: &ek, rows: &entities },
        ShapeRows { table: &a.facts, key: &fk, rows: &facts },
        ShapeRows { table: &a.edges, key: &gk, rows: &edges },
    );
    let decoys: Vec<&str> = a.decoys.iter().map(String::as_str).collect();
    let candidates: Vec<golden::Candidate> =
        golden::walks(&view, &a.corpus).into_iter().chain(golden::chains(&view, &a.corpus)).chain(golden::absent(&view, &decoys, &a.corpus)).collect();
    let mut text = String::new();
    for c in &candidates {
        text.push_str(&serde_json::to_string(&c.case)?);
        text.push('\n');
    }
    std::fs::write(&a.out, text).with_context(|| format!("writing `{}`", a.out.display()))?;
    eprintln!(
        "eval: read {} entities, {} facts and {} edges; drafted {} candidate(s) into `{}`; none is committed until approved",
        entities.len(),
        facts.len(),
        edges.len(),
        candidates.len(),
        a.out.display()
    );
    Ok(())
}

/// `contextful eval commit`: append each approved draft, redacted under the declaration's
/// removal rules, to the case file (`assurance.baseline.commit-command`).
fn commit(a: CommitArgs) -> Result<()> {
    let text = std::fs::read_to_string(&a.drafts).with_context(|| format!("reading `{}`", a.drafts.display()))?;
    let drafts = case::load(&text).map_err(|e| anyhow::anyhow!("`{}`: {e}", a.drafts.display()))?;
    let mut approvals = BTreeMap::new();
    for approval in &a.approvals {
        let (id, reviewer) = approval.split_once('=').with_context(|| format!("`--approve {approval}` is not <case id>=<reviewer>"))?;
        if reviewer.trim().is_empty() {
            bail!("`--approve {approval}` names no reviewer");
        }
        if !drafts.iter().any(|c| c.id == id) {
            bail!("`--approve {approval}` names no draft of `{}`", a.drafts.display());
        }
        approvals.insert(id.to_string(), reviewer.to_string());
    }
    let manifest = std::fs::read_to_string(&a.declaration).with_context(|| format!("reading `{}`", a.declaration.display()))?;
    let rules = TableDecl::parse_pipeline(&manifest)?.into_iter().flat_map(|d| d.redaction.unwrap_or_default()).collect();
    let redaction = GoldenRedaction::new(rules)?;
    let candidates = drafts.into_iter().map(|case| golden::Candidate { generator: generator_of(&case), case }).collect();
    let committed = golden::commit(candidates, &approvals, &redaction)?;
    let existing = match std::fs::read_to_string(&a.into) {
        Ok(t) if !t.trim().is_empty() => case::load(&t).map_err(|e| anyhow::anyhow!("`{}`: {e}", a.into.display()))?,
        Ok(_) => Vec::new(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e).with_context(|| format!("reading `{}`", a.into.display())),
    };
    let held: BTreeSet<&str> = existing.iter().map(|c| c.id.as_str()).collect();
    if let Some(c) = committed.iter().find(|c| held.contains(c.id.as_str())) {
        bail!("`{}` already holds case `{}`; nothing was committed", a.into.display(), c.id);
    }
    if let Some(dir) = a.into.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("creating `{}`", dir.display()))?;
    }
    let mut lines = String::new();
    for c in &committed {
        lines.push_str(&serde_json::to_string(c)?);
        lines.push('\n');
    }
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&a.into).with_context(|| format!("opening `{}`", a.into.display()))?;
    file.write_all(lines.as_bytes())?;
    eprintln!("eval: committed {} case(s) to `{}`", committed.len(), a.into.display());
    Ok(())
}

/// The generator a drafted case's tags name.
fn generator_of(case: &Case) -> golden::Generator {
    match case.tags.iter().find_map(|t| match t.as_str() {
        "chain" => Some(golden::Generator::Chain),
        "absent" => Some(golden::Generator::Absent),
        "walk" => Some(golden::Generator::Walk),
        _ => None,
    }) {
        Some(g) => g,
        None => golden::Generator::Walk,
    }
}
