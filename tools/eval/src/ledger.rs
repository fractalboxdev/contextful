//! The target ledger (`assurance.measure.ledger`): one entry per tracked target, keyed to
//! the clause it serves, naming its metric, tier, method and threshold.
//!
//! The ledger is `evals/ledger.toml`; the caller decodes it into [`Ledger`] and resolves
//! it against a [`World`] before any measure runs, so an entry naming nothing refuses the
//! run rather than dropping out of it (`assurance.measure.unresolved-entry`).

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::baseline::{Direction, MetricPath};
use crate::error::EvalError;

/// The ledger, under the workspace root.
pub const LEDGER_FILE: &str = "evals/ledger.toml";

/// Every entry's computed status, generated from [`LEDGER_FILE`].
pub const STATUS_FILE: &str = "evals/ledger.md";

/// The ledger: entries by id.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ledger {
    #[serde(default)]
    pub entry: BTreeMap<String, Entry>,
}

/// What a measure reports: a test's count, a case set's score, a size, a timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Test,
    Eval,
    Size,
    Bench,
}

/// Where an entry decides (`assurance.measure.tier`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Decides the evaluate stage.
    Gate,
    /// Records on every run and decides nothing.
    Trend,
    /// Runs on the scheduled job alone.
    Scheduled,
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Tier::Gate => "gate",
            Tier::Trend => "trend",
            Tier::Scheduled => "scheduled",
        })
    }
}

/// The method table as written: exactly one key is set.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MethodSpec {
    /// An integration test, `<crate>::<module path>::<function>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<String>,
    /// A case set under `evals/cases/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cases: Option<String>,
    /// A probe binary of `tools/probe`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<String>,
    /// The issue whose change brings the method; the entry reports open and gates nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<u64>,
}

/// An entry's method, once exactly one key is known to be set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method<'a> {
    Test(&'a str),
    Cases(&'a str),
    Probe(&'a str),
    Issue(u64),
}

/// A comparison against an absolute figure of this system's own measure
/// (`assurance.measure.absolute-threshold`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    #[serde(rename = "==")]
    Eq,
    #[serde(rename = "<=")]
    Le,
    #[serde(rename = ">=")]
    Ge,
    #[serde(rename = "<")]
    Lt,
    #[serde(rename = ">")]
    Gt,
}

/// An entry's threshold.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub op: Op,
    pub value: f64,
}

impl Target {
    /// Whether `value` meets the threshold. An equality compares within 1e-9, below any
    /// count's or ratio's resolution.
    pub fn holds(&self, value: f64) -> bool {
        match self.op {
            Op::Eq => (value - self.value).abs() <= 1e-9,
            Op::Le => value <= self.value,
            Op::Ge => value >= self.value,
            Op::Lt => value < self.value,
            Op::Gt => value > self.value,
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let op = match self.op {
            Op::Eq => "==",
            Op::Le => "<=",
            Op::Ge => ">=",
            Op::Lt => "<",
            Op::Gt => ">",
        };
        write!(f, "{op} {}", self.value)
    }
}

/// One tracked target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// The owning clause id; the ledger never copies its text.
    pub clause: String,
    /// The run-report field path the value fills.
    pub metric: String,
    pub kind: Kind,
    pub tier: Tier,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
    pub method: MethodSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
    /// The seed the method's fixtures derive from (`assurance.measure.seeded`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// The clause whose bound this target restates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirrors: Option<String>,
}

/// An entry's computed status (`assurance.measure.open-entry`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Its method arrives with the issue; it gates nothing.
    Open(u64),
    /// Its threshold decides the evaluate stage on every change.
    Gated,
    /// It records on every run and decides nothing.
    Recorded,
    /// It runs on the scheduled job alone.
    Scheduled,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Status::Open(n) => write!(f, "open (issue {n})"),
            Status::Gated => f.write_str("gated"),
            Status::Recorded => f.write_str("recorded"),
            Status::Scheduled => f.write_str("scheduled"),
        }
    }
}

impl Entry {
    /// The one method key set, or `None` when zero or several are.
    pub fn method(&self) -> Option<Method<'_>> {
        let m = &self.method;
        let set = [m.test.is_some(), m.cases.is_some(), m.probe.is_some(), m.issue.is_some()];
        if set.iter().filter(|s| **s).count() != 1 {
            return None;
        }
        m.test
            .as_deref()
            .map(Method::Test)
            .or(m.cases.as_deref().map(Method::Cases))
            .or(m.probe.as_deref().map(Method::Probe))
            .or(m.issue.map(Method::Issue))
    }

    pub fn status(&self) -> Status {
        match (self.method(), self.tier) {
            (Some(Method::Issue(n)), _) => Status::Open(n),
            (_, Tier::Gate) => Status::Gated,
            (_, Tier::Trend) => Status::Recorded,
            (_, Tier::Scheduled) => Status::Scheduled,
        }
    }
}

/// What the tree holds, as the resolver asks it.
pub trait World {
    /// Whether the lock file carries clause `id`.
    fn clause(&self, id: &str) -> bool;
    /// Why the integration test at `path` resolves to nothing, or `Ok` when it names one.
    fn test(&self, path: &str) -> Result<(), String>;
    /// Whether the case set at `path` exists.
    fn cases(&self, path: &str) -> bool;
    /// Whether a probe binary named `name` exists.
    fn probe(&self, name: &str) -> bool;
}

/// A test path's package and its name inside the package's integration binary:
/// `contextful_engine::journal::races` is `contextful-engine` and `journal::races`.
pub fn test_target(path: &str) -> Option<(String, String)> {
    let (krate, rest) = path.split_once("::")?;
    let ident = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if !ident(krate) || !rest.split("::").all(ident) {
        return None;
    }
    Some((krate.replace('_', "-"), rest.to_string()))
}

fn slug(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn metric_path(s: &str) -> bool {
    !s.is_empty() && s.split('.').all(|seg| !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
}

impl Ledger {
    /// Every entry that names no clause, method or metric path, one error each, in id
    /// order. An empty result means every entry resolves.
    pub fn unresolved(&self, world: &dyn World) -> Vec<EvalError> {
        let mut out = Vec::new();
        for (id, e) in &self.entry {
            if let Err(reason) = resolve(id, e, world) {
                out.push(EvalError::entry_unresolved(id, reason));
            }
        }
        out
    }

    /// Entries of `tier` whose method is known, in id order.
    pub fn runnable(&self, tier: Tier) -> impl Iterator<Item = (&String, &Entry)> {
        self.entry.iter().filter(move |(_, e)| e.tier == tier && !matches!(e.method(), Some(Method::Issue(_)) | None))
    }

    /// `evals/ledger.md`: every entry with its clause, metric, tier, method, threshold and
    /// computed status.
    pub fn render(&self) -> String {
        let mut s = String::from(
            "<!-- Generated by `contextful-ci measure --status` from evals/ledger.toml; the schema stage diffs this file. -->\n\n# Target ledger\n\n",
        );
        let count = |f: &dyn Fn(&Status) -> bool| self.entry.values().filter(|e| f(&e.status())).count();
        s.push_str(&format!(
            "{} entries: {} gated, {} recorded, {} scheduled, {} open.\n\n",
            self.entry.len(),
            count(&|s| *s == Status::Gated),
            count(&|s| *s == Status::Recorded),
            count(&|s| *s == Status::Scheduled),
            count(&|s| matches!(s, Status::Open(_))),
        ));
        s.push_str("| Entry | Clause | Metric | Tier | Method | Target | Status |\n| --- | --- | --- | --- | --- | --- | --- |\n");
        for (id, e) in &self.entry {
            let method = match e.method() {
                Some(Method::Test(t)) => format!("test `{t}`"),
                Some(Method::Cases(c)) => format!("cases `{c}`"),
                Some(Method::Probe(p)) => format!("probe `{p}`"),
                Some(Method::Issue(n)) => format!("issue {n}"),
                None => "—".into(),
            };
            let target = e.target.map(|t| format!("`{t}`")).unwrap_or_else(|| "—".into());
            s.push_str(&format!("| `{id}` | `{}` | `{}` | {} | {method} | {target} | {} |\n", e.clause, e.metric, e.tier, e.status()));
        }
        s
    }
}

fn resolve(id: &str, e: &Entry, world: &dyn World) -> Result<(), String> {
    if !slug(id) {
        return Err("an entry id is a slug of lowercase letters, digits and hyphens".into());
    }
    if !world.clause(&e.clause) {
        return Err(format!("clause `{}` is no clause of spec/spec.lock.json", e.clause));
    }
    if let Some(m) = e.mirrors.as_deref().filter(|m| !world.clause(m)) {
        return Err(format!("mirrors `{m}`, which is no clause of spec/spec.lock.json"));
    }
    if !metric_path(&e.metric) {
        return Err(format!("metric `{}` is no dotted path of lowercase segments", e.metric));
    }
    if e.kind == Kind::Eval {
        MetricPath::parse(&e.metric).map_err(|err| format!("metric `{}`: {err}", e.metric))?;
    }
    let method = e.method().ok_or("a method names exactly one of `test`, `cases`, `probe` or `issue`")?;
    if !matches!(method, Method::Issue(_)) && e.tier == Tier::Gate && e.target.is_none() {
        return Err("a gate-tier entry carries a target".into());
    }
    if e.tier == Tier::Trend && e.direction.is_none() {
        return Err("a trend-tier entry declares a direction".into());
    }
    match method {
        Method::Test(t) => world.test(t).map_err(|r| format!("test `{t}`: {r}")),
        Method::Cases(c) if !world.cases(c) => Err(format!("case set `{c}` does not exist")),
        Method::Probe(p) if !world.probe(p) => Err(format!("probe `{p}` is no binary of tools/probe")),
        Method::Issue(0) => Err("issue 0 names no issue".into()),
        _ => Ok(()),
    }
}
