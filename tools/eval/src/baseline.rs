//! A baseline file and the gate that holds a run report against it.
//!
//! A file carries the reserved run block `_run` and one entry per gated metric, keyed by
//! the report's field path. The gate resolves every entry before it compares any, so an
//! entry naming nothing refuses the run rather than dropping out of it; it then compares
//! each figure within its dead band and holds the report to the absolute floors.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::EvalError;
use crate::floors::{self, FloorVerdict};
use crate::metrics::{LEGS, RANKED_METRICS, RELEVANCE_METRICS};

/// Dead band of a rate entry with no override (`assurance-default-dead-band`: 2 percent).
pub const DEFAULT_DEAD_BAND: f64 = 0.02;

/// Dead band of the ranked-quality entry (`assurance-rank-dead-band`: 3 percent).
pub const RANK_DEAD_BAND: f64 = 0.03;

/// Fewest cases behind a gated mean (`assurance-gated-mean-cases`: 30 cases).
pub const GATED_MEAN_CASES: u64 = 30;

/// The reserved key holding the run block.
pub const RUN_KEY: &str = "_run";

/// The key a run report carries its run block under.
pub const REPORT_RUN_KEY: &str = "run";

/// The ranked-quality metric, gated at [`RANK_DEAD_BAND`].
const RANK_QUALITY: &str = "ndcg_at_k";

/// The judged dimensions, by report field name.
pub const JUDGE_DIMENSIONS: [&str; 5] =
    ["accuracy", "correct_refusal", "hallucination_on_unknown", "citation_faithfulness", "temporal_correctness"];

/// Metrics where a smaller figure is the better one.
const LOWER_IS_BETTER: [&str; 4] = ["forbidden_row_rate", "duplicate_row_rate", "hallucination_on_unknown", "latency_ms"];

/// Absolute slack on a band comparison, below any metric's resolution, so a delta equal
/// to the band in decimal reads as inside it despite binary rounding.
const BAND_EPSILON: f64 = 1e-9;

const GRAMMAR: &str = "an entry names `retrieval.<leg>.<metric>`, `edge_retrieval.<leg>.<metric>`, \
     `judge.<dimension>`, `slices.<tag>.<metric>`, `slices.<tag>.<path>`, `latency_ms` or `n_cases`, a mean optionally suffixed `.n`";

/// Whether a figure grades higher or lower as better.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    HigherIsBetter,
    LowerIsBetter,
}

/// The evaluation tier a run executes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Deterministic,
    Judged,
}

/// The configuration a set of figures was measured under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunStamp {
    /// The retrieval cut-off every at-k metric used.
    pub k: usize,
    pub tier: Tier,
    /// The pinned judge model; `None` in the deterministic tier.
    pub model: Option<String>,
    /// Judge calls per judged item.
    pub samples: usize,
}

impl std::fmt::Display for RunStamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tier = match self.tier {
            Tier::Deterministic => "deterministic",
            Tier::Judged => "judged",
        };
        write!(f, "k={}, tier={tier}, model={}, samples={}", self.k, self.model.as_deref().unwrap_or("none"), self.samples)
    }
}

/// One baseline entry: the committed value and, in the object form, its own band.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Entry {
    pub value: f64,
    pub band: Option<f64>,
}

/// What a metric path names in the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricPath {
    /// The walk from the report root to the summary or count node.
    pub steps: Vec<String>,
    pub kind: PathKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    /// A mean-valued summary `{n, mean, ..}`; the gate reads its `mean`.
    Mean { direction: Direction, ranked_quality: bool },
    /// A count, pinned at band zero: its own number, or a summary's `n` under a `.n` suffix.
    Count,
}

impl MetricPath {
    /// Parse `path` against the metric-path grammar.
    pub fn parse(path: &str) -> Result<MetricPath, EvalError> {
        let (head, sample_count) = match path.strip_suffix(".n") {
            Some(h) => (h, true),
            None => (path, false),
        };
        let parsed = parse_head(head, true).ok_or_else(|| EvalError::unresolved(path, GRAMMAR))?;
        if !sample_count {
            return Ok(parsed);
        }
        match parsed.kind {
            PathKind::Mean { .. } => {
                let mut steps = parsed.steps;
                steps.push("n".into());
                Ok(MetricPath { steps, kind: PathKind::Count })
            }
            PathKind::Count => Err(EvalError::unresolved(path, "a count carries no sample count of its own")),
        }
    }

    fn metric(&self) -> &str {
        self.steps.last().map(String::as_str).unwrap_or_default()
    }
}

fn mean(steps: Vec<&str>) -> MetricPath {
    let metric = *steps.last().unwrap_or(&"");
    let direction = if LOWER_IS_BETTER.contains(&metric) { Direction::LowerIsBetter } else { Direction::HigherIsBetter };
    let ranked_quality = metric == RANK_QUALITY;
    MetricPath { steps: steps.into_iter().map(String::from).collect(), kind: PathKind::Mean { direction, ranked_quality } }
}

/// Parse a path with no `.n` suffix. `top` admits the paths a slice cannot nest.
fn parse_head(head: &str, top: bool) -> Option<MetricPath> {
    let parts: Vec<&str> = head.split('.').collect();
    match parts.as_slice() {
        ["n_cases"] => Some(MetricPath { steps: vec!["n_cases".into()], kind: PathKind::Count }),
        ["latency_ms"] if top => Some(mean(vec!["latency_ms"])),
        ["judge", dim] if JUDGE_DIMENSIONS.contains(dim) => Some(mean(vec!["judge", dim])),
        [surface @ ("retrieval" | "edge_retrieval"), leg, metric]
            if LEGS.contains(leg) && (RANKED_METRICS.contains(metric) || RELEVANCE_METRICS.contains(metric)) =>
        {
            Some(mean(vec![surface, leg, metric]))
        }
        ["slices", tag, metric]
            if top && !tag.is_empty() && (RANKED_METRICS.contains(metric) || RELEVANCE_METRICS.contains(metric)) =>
        {
            Some(mean(vec!["slices", tag, metric]))
        }
        ["slices", tag, ..] if top && !tag.is_empty() && parts.len() > 2 => {
            let inner = parse_head(&parts[2..].join("."), false)?;
            let mut steps = vec!["slices".to_string(), tag.to_string()];
            steps.extend(inner.steps);
            Some(MetricPath { steps, kind: inner.kind })
        }
        _ => None,
    }
}

/// A committed baseline file.
#[derive(Debug, Clone, PartialEq)]
pub struct Baselines {
    pub run: RunStamp,
    /// Metric path → entry, in path order.
    pub entries: BTreeMap<String, Entry>,
}

impl Baselines {
    /// Parse and validate a baseline file. Every refusal needs no report: the run block,
    /// each entry's form, the path grammar, and each value and band.
    pub fn parse(raw: &str) -> Result<Baselines, EvalError> {
        let root: Value = serde_json::from_str(raw).map_err(|e| EvalError::unresolved("", format!("not JSON: {e}")))?;
        let Value::Object(map) = root else {
            return Err(EvalError::unresolved("", "a baseline file is a JSON object"));
        };
        // A parsed map keeps the last of a repeated key, at any depth; the raw document
        // shows every one.
        let Repeat(twice) = serde_json::from_str(raw).map_err(|e| EvalError::unresolved("", format!("not JSON: {e}")))?;
        if let Some(path) = twice {
            return Err(EvalError::unresolved(&path, "the key appears twice in the file"));
        }
        let run = map
            .get(RUN_KEY)
            .ok_or_else(|| EvalError::unresolved(RUN_KEY, "the file carries no run block"))
            .and_then(|v| {
                serde_json::from_value::<RunStamp>(v.clone()).map_err(|e| {
                    EvalError::unresolved(RUN_KEY, format!("expected {{k, tier, model, samples}}: {e}"))
                })
            })?;
        let mut entries = BTreeMap::new();
        for (key, value) in &map {
            if key == RUN_KEY {
                continue;
            }
            entries.insert(key.clone(), parse_entry(key, value)?);
        }
        let b = Baselines { run, entries };
        b.validate()?;
        Ok(b)
    }

    /// Render the file: the run block, then entries in path order, with a trailing newline.
    pub fn to_json(&self) -> String {
        let mut map = Map::new();
        map.insert(RUN_KEY.into(), serde_json::to_value(&self.run).expect("a run block serializes"));
        for (path, e) in &self.entries {
            let v = match e.band {
                None => serde_json::json!(e.value),
                Some(band) => serde_json::json!({ "value": e.value, "band": band }),
            };
            map.insert(path.clone(), v);
        }
        let mut s = serde_json::to_string_pretty(&Value::Object(map)).expect("a baseline file serializes");
        s.push('\n');
        s
    }

    fn validate(&self) -> Result<(), EvalError> {
        for (path, e) in &self.entries {
            let p = MetricPath::parse(path)?;
            if !e.value.is_finite() {
                return Err(EvalError::unresolved(path, "the value is not a finite number"));
            }
            match (p.kind, e.band) {
                (PathKind::Count, Some(_)) => {
                    return Err(EvalError::unresolved(path, "a count is pinned at band zero and carries no band"))
                }
                (_, Some(band)) if !band.is_finite() || band < 0.0 => {
                    return Err(EvalError::unresolved(path, format!("the band {band} is not a finite number at or above zero")))
                }
                (PathKind::Mean { .. }, None) if p.metric() == "latency_ms" => {
                    return Err(EvalError::unresolved(path, "latency carries its own band, in milliseconds"))
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Refuse a run whose configuration differs from the one these figures were
    /// recorded under. The run's configuration is known before the suite runs.
    pub fn check_run(&self, run: &RunStamp) -> Result<(), EvalError> {
        if &self.run == run {
            return Ok(());
        }
        Err(EvalError::BaselineRunStampMismatch { recorded: self.run.to_string(), run: run.to_string() })
    }

    /// The band `path`'s comparison applies.
    pub fn band(&self, path: &str) -> Result<f64, EvalError> {
        let p = MetricPath::parse(path)?;
        let entry = self.entries.get(path).ok_or_else(|| EvalError::unresolved(path, "the file carries no such entry"))?;
        Ok(match (p.kind, entry.band) {
            (PathKind::Count, _) => 0.0,
            (_, Some(band)) => band,
            (PathKind::Mean { ranked_quality: true, .. }, None) => RANK_DEAD_BAND,
            (PathKind::Mean { .. }, None) => DEFAULT_DEAD_BAND,
        })
    }

    /// Move each improved entry to its measured value and return the moved paths. The
    /// update runs only on a verdict whose every gate and floor passes, and returns `None`
    /// otherwise. No entry moves in the worse direction and no path is added.
    pub fn raise(&mut self, verdict: &Verdict) -> Option<Vec<String>> {
        if !verdict.passed() {
            return None;
        }
        let mut moved = Vec::new();
        for c in &verdict.comparisons {
            if c.outcome != Outcome::Improved {
                continue;
            }
            if let Some(e) = self.entries.get_mut(&c.path) {
                e.value = c.current;
                moved.push(c.path.clone());
            }
        }
        Some(moved)
    }
}

/// The dotted path of the first key an object in the document names twice, at any depth.
struct Repeat(Option<String>);

impl<'de> Deserialize<'de> for Repeat {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Repeat, D::Error> {
        struct Visit;
        impl<'de> serde::de::Visitor<'de> for Visit {
            type Value = Repeat;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON value")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Repeat, A::Error> {
                let mut seen = std::collections::BTreeSet::new();
                let mut first = None;
                while let Some(k) = map.next_key::<String>()? {
                    let Repeat(inner) = map.next_value::<Repeat>()?;
                    if first.is_none() {
                        first = if seen.contains(&k) { Some(k.clone()) } else { inner.map(|p| format!("{k}.{p}")) };
                    }
                    seen.insert(k);
                }
                Ok(Repeat(first))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Repeat, A::Error> {
                let mut first = None;
                let mut i = 0usize;
                while let Some(Repeat(inner)) = seq.next_element::<Repeat>()? {
                    if first.is_none() {
                        first = inner.map(|p| format!("{i}.{p}"));
                    }
                    i += 1;
                }
                Ok(Repeat(first))
            }
            fn visit_bool<E>(self, _: bool) -> Result<Repeat, E> {
                Ok(Repeat(None))
            }
            fn visit_i64<E>(self, _: i64) -> Result<Repeat, E> {
                Ok(Repeat(None))
            }
            fn visit_u64<E>(self, _: u64) -> Result<Repeat, E> {
                Ok(Repeat(None))
            }
            fn visit_f64<E>(self, _: f64) -> Result<Repeat, E> {
                Ok(Repeat(None))
            }
            fn visit_str<E>(self, _: &str) -> Result<Repeat, E> {
                Ok(Repeat(None))
            }
            fn visit_unit<E>(self) -> Result<Repeat, E> {
                Ok(Repeat(None))
            }
        }
        d.deserialize_any(Visit)
    }
}

fn parse_entry(key: &str, value: &Value) -> Result<Entry, EvalError> {
    match value {
        Value::Number(n) => Ok(Entry { value: n.as_f64().unwrap_or(f64::NAN), band: None }),
        Value::Object(o) => {
            if let Some(extra) = o.keys().find(|k| *k != "value" && *k != "band") {
                return Err(EvalError::unresolved(key, format!("unknown field `{extra}`; the object form is {{value, band}}")));
            }
            let value = o.get("value").and_then(Value::as_f64);
            let band = o.get("band").and_then(Value::as_f64);
            match (value, band) {
                (Some(value), Some(band)) => Ok(Entry { value, band: Some(band) }),
                _ => Err(EvalError::unresolved(key, "the object form carries a numeric `value` and a numeric `band`")),
            }
        }
        _ => Err(EvalError::unresolved(key, "an entry is a number or {value, band}")),
    }
}

/// How one figure compares with its committed value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Improved,
    Stable,
    Regressed,
}

/// One entry's comparison.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Comparison {
    pub path: String,
    pub baseline: f64,
    pub current: f64,
    pub band: f64,
    pub outcome: Outcome,
}

/// Both verdicts over one run report: the baseline comparisons and the absolute floors.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Verdict {
    pub comparisons: Vec<Comparison>,
    pub floors: FloorVerdict,
}

impl Verdict {
    /// No entry regressed.
    pub fn baseline_passed(&self) -> bool {
        self.comparisons.iter().all(|c| c.outcome != Outcome::Regressed)
    }

    /// No entry regressed and every floor holds.
    pub fn passed(&self) -> bool {
        self.baseline_passed() && self.floors.passed
    }
}

/// Compare `current` with `baseline` within `band`.
pub fn compare(baseline: f64, current: f64, band: f64, direction: Direction) -> Outcome {
    let gain = match direction {
        Direction::HigherIsBetter => current - baseline,
        Direction::LowerIsBetter => baseline - current,
    };
    if gain.abs() <= band + BAND_EPSILON {
        Outcome::Stable
    } else if gain > 0.0 {
        Outcome::Improved
    } else {
        Outcome::Regressed
    }
}

fn walk<'a>(root: &'a Value, steps: &[String]) -> Option<&'a Value> {
    steps.iter().try_fold(root, |node, s| node.get(s))
}

/// Resolve `path` against `report` to the figure its comparison reads.
pub fn resolve(report: &Value, path: &str) -> Result<f64, EvalError> {
    let p = MetricPath::parse(path)?;
    let node = walk(report, &p.steps).ok_or_else(|| EvalError::unresolved(path, "the report carries no such field"))?;
    match p.kind {
        PathKind::Count => node.as_f64().ok_or_else(|| EvalError::unresolved(path, "the report field is not a count")),
        PathKind::Mean { .. } => {
            let n = node.get("n").and_then(Value::as_u64).unwrap_or(0);
            if n < GATED_MEAN_CASES {
                return Err(EvalError::unresolved(path, format!("a mean over {n} cases, below {GATED_MEAN_CASES}")));
            }
            node.get("mean")
                .and_then(Value::as_f64)
                .filter(|m| m.is_finite())
                .ok_or_else(|| EvalError::unresolved(path, "the report field carries no finite mean"))
        }
    }
}

/// Hold `report` against `baselines` and the absolute floors.
///
/// The report's run block must equal the file's; every entry resolves before any is
/// compared, so one unresolvable entry refuses the run and yields no partial verdict.
/// The floors gate independently of every committed value.
pub fn gate(report: &Value, baselines: &Baselines) -> Result<Verdict, EvalError> {
    let mismatch = |run: String| EvalError::BaselineRunStampMismatch { recorded: baselines.run.to_string(), run };
    let block = report.get(REPORT_RUN_KEY).ok_or_else(|| mismatch("a report with no run block".into()))?;
    let run = serde_json::from_value::<RunStamp>(block.clone())
        .map_err(|e| mismatch(format!("a report whose run block does not parse: {e}")))?;
    baselines.check_run(&run)?;

    let mut resolved = Vec::with_capacity(baselines.entries.len());
    for (path, entry) in &baselines.entries {
        resolved.push((path, entry, resolve(report, path)?));
    }
    let mut comparisons = Vec::with_capacity(resolved.len());
    for (path, entry, current) in resolved {
        let band = baselines.band(path)?;
        let direction = match MetricPath::parse(path)?.kind {
            PathKind::Mean { direction, .. } => direction,
            PathKind::Count => Direction::HigherIsBetter,
        };
        comparisons.push(Comparison {
            path: path.clone(),
            baseline: entry.value,
            current,
            band,
            outcome: compare(entry.value, current, band, direction),
        });
    }
    Ok(Verdict { comparisons, floors: floors::check(report) })
}
