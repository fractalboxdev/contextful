//! `contextful eval run` — the read-path quality harness's runner
//! (`assurance.evaluate.run-command`).
//!
//! A thin adapter: it loads the case file through the harness's one loader, lands each
//! referenced corpus into a scratch store through the landing and fold paths
//! `contextful context` uses, reads every case through the ranked call under one admitted
//! credential, and hands the returns to the harness core for scoring and both verdicts. No
//! metric, floor or baseline rule lives here.

use crate::admit::AdmitArgs;
use crate::context::read_rows;
use crate::run::SystemClock;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use contextful_context::fold::fold;
use contextful_context::land::{land, Batch, RunContext};
use contextful_context::read::{Face, RetrieveRequest};
use contextful_context::{node, Store};
use contextful_core::ports::Clock;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::reconcile::{ColumnType, FloatItem};
use contextful_core::store::reserve::Injection;
use contextful_core::time::Instant;
use contextful_eval::baseline::{self, Baselines, Outcome, RunStamp, Tier};
use contextful_eval::case::{self, Case, KEY_SEPARATOR};
use contextful_eval::embed::{StubEmbedder, DEFAULT_SEED};
use contextful_eval::floors;
use contextful_eval::metrics::{Returned, RowRef, LEGS};
use contextful_eval::report::{self, CaseRun};
use contextful_eval::EvalError;
use contextful_policy::enforce::mask::Pepper;
use contextful_policy::enforce::session::{Request, Session};
use contextful_policy::verify::{effect_boundary, Admission, AdmittedAuthority};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap};
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

#[derive(Subcommand)]
pub enum EvalCmd {
    /// Run a case file through the ranked read and score it; exits non-zero on a red floor or baseline.
    Run(RunArgs),
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
    #[command(flatten)]
    admit: AdmitArgs,
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
/// embedding, then fold each table so its declared sidecars build.
fn land_corpus(corpus: &Corpus, store: &Store, embedder: &StubEmbedder, at: Instant) -> Result<()> {
    let (node, _) = node::resolve(store, |k| std::env::var(k).ok())?;
    for (table, path) in corpus.tables()? {
        let decl = corpus.decl(&table);
        let mut rows = read_rows(&path)?;
        for row in &mut rows {
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
    Ok(())
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

/// Read one case on every leg (`assurance.evaluate.legs`).
fn read_case(
    face: &Face,
    session: &Session,
    corpus: &Corpus,
    case: &Case,
    k: usize,
    embedder: &StubEmbedder,
    landed_at: Instant,
) -> Result<BTreeMap<String, Vec<Returned>>> {
    let anchor = instant("time_anchor", case.expected.time_anchor.as_deref(), landed_at)?;
    let since = case.expected.since.as_deref().map(|s| instant("since", Some(s), landed_at)).transpose()?;
    let embedding = case.query_embedding.clone().unwrap_or_else(|| embedder.embed(&case.question));
    let prefix = case.prefix.clone().unwrap_or_default();
    let mut legs = BTreeMap::new();
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
        legs.insert(leg.to_string(), returned);
    }
    Ok(legs)
}

fn session(face: &Face, authority: &AdmittedAuthority, revocation: &contextful_policy::revoke::RevocationState, zone: Option<&str>) -> Result<Session> {
    effect_boundary(authority, &Admission::new(SystemClock.now(), revocation))?;
    Ok(face.session(authority, &Request { zone }, Bounds::default())?)
}

pub fn run(cmd: EvalCmd) -> Result<()> {
    let EvalCmd::Run(a) = cmd;
    let text = std::fs::read_to_string(&a.goldens).with_context(|| format!("reading `{}`", a.goldens.display()))?;
    let cases = case::load(&text).map_err(|e| anyhow::anyhow!("`{}`: {e}", a.goldens.display()))?;
    let stamp = RunStamp { k: a.k, tier: Tier::Deterministic, model: None, samples: 1 };
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

    let (authority, revocation) = a.admit.admit("the evaluation run")?;
    let landed_at = instant("--landed-at", a.landed_at.as_deref(), Instant::from_unix_nanos(0)?)?;
    let embedder = StubEmbedder::new(a.seed);
    let pepper = Pepper::resolve(|k| std::env::var(k).ok());
    if let Some(signal) = pepper.signal() {
        eprintln!("{signal}");
    }
    let mut legs: Vec<BTreeMap<String, Vec<Returned>>> = vec![BTreeMap::new(); cases.len()];
    for (corpus, idx) in &opened {
        let scratch = tempfile::tempdir()?;
        let store = Store::open(scratch.path(), SCRATCH_PROJECT)?;
        land_corpus(corpus, &store, &embedder, landed_at)?;
        let face = Face::open(store, &corpus.manifest, pepper.clone())?;
        let s = session(&face, &authority, &revocation, a.zone.as_deref())?;
        for &i in idx {
            legs[i] = read_case(&face, &s, corpus, &cases[i], a.k, &embedder, landed_at)?;
        }
    }
    let runs: Vec<CaseRun<'_>> = cases.iter().zip(legs).map(|(case, legs)| CaseRun { case, legs }).collect();
    let report = report::build(&stamp, a.seed, &runs);
    let text = serde_json::to_string_pretty(&report)? + "\n";
    match &a.report {
        Some(path) => std::fs::write(path, &text).with_context(|| format!("writing the report `{}`", path.display()))?,
        None => print!("{text}"),
    }
    verdict(&report, baselines, &a)
}

/// Hold the report to the floors and the baseline, print both verdicts, and raise the
/// baseline on green when asked (`assurance.evaluate.run-verdict`).
fn verdict(report: &Value, baselines: Option<Baselines>, a: &RunArgs) -> Result<()> {
    let floors = floors::check(report);
    let mut red: Vec<String> = floors.breaches().map(|c| format!("floor {}: {} against {:?} {}", c.path, c.measured, c.side, c.bound)).collect();
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
