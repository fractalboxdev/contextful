//! `disclosure.release`: one derived table from a governed model's per-contributor
//! breakdown.
//!
//! The order is fixed. The breakdown's shape passes the contributor-key, group-key,
//! forbidden-column, metric-bounds and grain checks; then every contributing unit's
//! per-run spend is reserved against its lifetime cap through the [`BudgetLedger`] port,
//! all units or none; only then are figures computed. Partitions are selected by a noised
//! distinct-contributor count under [`super::suppress`], and each published sum folds,
//! clips and quantizes every contributor before its noise is drawn. No refusal publishes
//! a row, and no published row or envelope field carries a contributor key.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Mutex;

use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::noise::{statistic_noise, Entropy, Spend};
use super::suppress::{suppress, GroupStats, SuppressPolicy, DISCLOSURE_SENTINEL};
use super::DisclosureError;
use crate::pipeline::canonical::canonical_json;
use crate::pipeline::model::{DisclosureDecl, MetricBounds};
use crate::run::Failure;

/// Why a release published nothing.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ReleaseError {
    /// A disclosure refusal.
    #[error(transparent)]
    Refused(#[from] DisclosureError),
    /// The budget ledger did not answer; no unit was charged.
    #[error("budget ledger: {0}")]
    Ledger(Failure),
}

/// One contributing unit's lifetime cap and the spend reserved against it, in
/// micro-epsilon.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct UnitBudget {
    pub cap: u64,
    pub spent: u64,
}

impl UnitBudget {
    pub fn remaining(&self) -> u64 {
        self.cap.saturating_sub(self.spent)
    }
}

/// Reserves `spend` for every unit in `units` against `balances`, all or none. A unit
/// absent from `balances` holds no cap. On refusal `balances` is unchanged and the error
/// counts the short units without naming one.
pub fn reserve_all(balances: &mut BTreeMap<String, UnitBudget>, units: &BTreeSet<String>, spend: Spend) -> Result<(), DisclosureError> {
    let short = units.iter().filter(|u| balances.get(*u).map_or(0, UnitBudget::remaining) < spend.micros()).count();
    if short > 0 {
        return Err(DisclosureError::UnitBudgetExhausted(format!(
            "{short} of {} contributing units cannot cover a per-run spend of {} micro-epsilon",
            units.len(),
            spend.micros()
        )));
    }
    for unit in units {
        balances.entry(unit.clone()).or_default().spent += spend.micros();
    }
    Ok(())
}

/// The per-unit budget ledger. An adapter applies [`reserve_all`] to the balances it
/// reads inside one transaction and writes them back in that transaction, so two
/// concurrent releases never both spend the same remaining budget.
///
/// A `release_id` that already holds a reservation returns `Ok` and charges nothing, so
/// a caller retrying after a lost acknowledgement never pays twice. A refused
/// reservation records nothing, so its retry is evaluated afresh.
pub trait BudgetLedger {
    fn reserve(&self, release_id: &str, units: &BTreeSet<String>, spend: Spend) -> Result<(), ReleaseError>;
}

/// A ledger in process memory, one lock around every balance.
#[derive(Debug, Default)]
pub struct MemoryLedger {
    state: Mutex<MemoryState>,
}

#[derive(Debug, Default)]
struct MemoryState {
    balances: BTreeMap<String, UnitBudget>,
    releases: BTreeSet<String>,
}

impl MemoryLedger {
    /// Sets `unit`'s lifetime cap to `cap` micro-epsilon, keeping its reserved spend.
    pub fn grant(&self, unit: &str, cap: u64) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).balances.entry(unit.to_string()).or_default().cap = cap;
    }

    pub fn budget(&self, unit: &str) -> Option<UnitBudget> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).balances.get(unit).copied()
    }
}

impl BudgetLedger for MemoryLedger {
    fn reserve(&self, release_id: &str, units: &BTreeSet<String>, spend: Spend) -> Result<(), ReleaseError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.releases.contains(release_id) {
            return Ok(());
        }
        reserve_all(&mut state.balances, units, spend)?;
        state.releases.insert(release_id.to_string());
        Ok(())
    }
}

/// A governed statement's output: one row per group and contributor.
#[derive(Debug, Clone, PartialEq)]
pub struct Breakdown {
    /// The statement's columns, in output order.
    pub columns: Vec<String>,
    /// The model's grain: the group key plus the contributor key.
    pub grain: Vec<String>,
    pub rows: Vec<Map<String, Value>>,
}

/// What a release publishes under, and the spend it reserves per contributing unit.
#[derive(Debug, Clone, PartialEq)]
pub struct ReleaseRequest {
    pub release_id: String,
    pub table: String,
    pub spend: Spend,
}

/// The published rows: the breakdown's columns less the contributor key.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DerivedTable {
    pub name: String,
    pub columns: Vec<String>,
    pub rows: Vec<Map<String, Value>>,
}

/// What a release reports beside its table (`disclosure.release.release-envelope`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseEnvelope {
    pub table: String,
    pub policy_hash: String,
    /// Micro-epsilon reserved from each contributing unit.
    pub reserved_spend: u64,
    pub suppressed_present: bool,
    pub row_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub table: DerivedTable,
    pub envelope: ReleaseEnvelope,
}

/// The policy's digest: SHA-256 over its canonical JSON, set-valued fields sorted.
pub fn policy_hash(policy: &DisclosureDecl) -> String {
    let mut p = policy.clone();
    p.grouping_allowlist.sort();
    p.forbidden_columns.sort();
    let value = serde_json::to_value(&p).unwrap_or(Value::Null);
    format!("sha256:{:x}", Sha256::digest(canonical_json(&value).as_bytes()))
}

/// A metric's declared bounds in quantum units.
struct Metric {
    name: String,
    lower: f64,
    upper: f64,
    quantum: f64,
    /// The most one contributor's quantized total moves the sum.
    sensitivity: u64,
}

fn metric(name: &str, bounds: Option<&MetricBounds>) -> Result<Metric, DisclosureError> {
    let absent = |why: &str| DisclosureError::MetricBoundsAbsent(format!("metric `{name}` {why}"));
    let b = bounds.ok_or_else(|| absent("declares no lower bound, upper bound and quantum"))?;
    if !(b.lower.is_finite() && b.upper.is_finite() && b.lower <= b.upper) {
        return Err(absent("declares no finite bounds with lower at most upper"));
    }
    if !(b.quantum.is_finite() && b.quantum > 0.0) {
        return Err(absent("declares no positive quantum"));
    }
    let reach = (b.lower / b.quantum).round().abs().max((b.upper / b.quantum).round().abs());
    Ok(Metric { name: name.into(), lower: b.lower, upper: b.upper, quantum: b.quantum, sensitivity: (reach as u64).max(1) })
}

fn unit_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => "null".into(),
    }
}

/// A contributor's metric cell as a number; null contributes zero.
fn number(model_metric: &str, v: Option<&Value>) -> Result<f64, DisclosureError> {
    match v {
        None | Some(Value::Null) => Ok(0.0),
        Some(Value::Number(n)) => n.as_f64().ok_or_else(|| DisclosureError::MetricBoundsAbsent(format!("metric `{model_metric}` holds a value outside f64"))),
        Some(_) => Err(DisclosureError::MetricBoundsAbsent(format!("metric `{model_metric}` holds a non-numeric value"))),
    }
}

struct Group {
    key: Map<String, Value>,
    carried: Map<String, Value>,
    /// Per contributor, each metric's folded total.
    contributors: BTreeMap<String, Vec<f64>>,
}

/// Releases `breakdown` under `policy`, reserving `request.spend` per contributing unit
/// from `ledger` before computing, and drawing every noise sample from `entropy`.
pub fn release(
    policy: &DisclosureDecl,
    breakdown: &Breakdown,
    request: &ReleaseRequest,
    ledger: &dyn BudgetLedger,
    entropy: &mut dyn Entropy,
) -> Result<Release, ReleaseError> {
    let contributor = policy.contributor_key.as_str();
    if !breakdown.columns.iter().any(|c| c == contributor) {
        return Err(DisclosureError::ContributorKeyAbsent(format!("the statement emits no `{contributor}` column")).into());
    }
    let group_key: Vec<&String> = breakdown.grain.iter().filter(|c| *c != contributor).collect();
    if let Some(c) = group_key.iter().find(|c| !policy.grouping_allowlist.contains(c) || !breakdown.columns.contains(c)) {
        return Err(DisclosureError::GroupKeyNotPermitted(format!("group key column `{c}` is not a permitted grouping column the statement emits")).into());
    }
    let published: Vec<String> = breakdown.columns.iter().filter(|c| *c != contributor).cloned().collect();
    if let Some(c) = published.iter().find(|c| policy.forbidden_columns.contains(c)) {
        return Err(DisclosureError::ForbiddenColumnPublished(format!("published column `{c}` is forbidden")).into());
    }
    let rest: Vec<&String> = published.iter().filter(|c| !group_key.contains(c)).collect();
    let mut metrics = Vec::new();
    let mut carried = Vec::new();
    for c in rest {
        let numeric = breakdown.rows.iter().any(|r| r.get(c.as_str()).is_some_and(Value::is_number));
        if policy.metric_bounds.contains_key(c.as_str()) || numeric {
            metrics.push(metric(c, policy.metric_bounds.get(c.as_str()))?);
        } else {
            carried.push(c.clone());
        }
    }
    let floor = SuppressPolicy::new(policy.min_group_size, policy.max_contributor_share)?;

    let mut groups: Vec<Group> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut units = BTreeSet::new();
    for row in &breakdown.rows {
        let key: Map<String, Value> = group_key.iter().map(|c| ((*c).clone(), row.get(c.as_str()).cloned().unwrap_or(Value::Null))).collect();
        let at = *index.entry(canonical_json(&Value::Object(key.clone()))).or_insert_with(|| {
            let carried = carried.iter().map(|c| (c.clone(), row.get(c).cloned().unwrap_or(Value::Null))).collect();
            groups.push(Group { key, carried, contributors: BTreeMap::new() });
            groups.len() - 1
        });
        let group = &mut groups[at];
        if let Some(c) = carried.iter().find(|c| group.carried.get(*c) != Some(row.get(c.as_str()).unwrap_or(&Value::Null))) {
            return Err(DisclosureError::DuplicateGrain(format!("carried column `{c}` is not constant within its group")).into());
        }
        let unit = unit_of(row.get(contributor));
        let totals = group.contributors.entry(unit.clone()).or_insert_with(|| vec![0.0; metrics.len()]);
        for (total, m) in totals.iter_mut().zip(&metrics) {
            *total += number(&m.name, row.get(&m.name))?;
        }
        units.insert(unit);
    }

    ledger.reserve(&request.release_id, &units, request.spend)?;

    // The count and every metric share the per-run spend, once per group a unit reaches:
    // a unit in g groups enters g counts and g sums, so each draws at ε / ((1 + m) · g).
    let mut reach: HashMap<&str, u64> = HashMap::new();
    for unit in groups.iter().flat_map(|g| g.contributors.keys()) {
        *reach.entry(unit.as_str()).or_default() += 1;
    }
    let g_max = reach.values().copied().max().unwrap_or(1);
    let statistics = (1 + metrics.len() as u64).saturating_mul(g_max);
    let mut candidates = Vec::new();
    for group in groups {
        let exact = group.contributors.len() as i64;
        let noised = exact.saturating_add(statistic_noise(entropy, 1, request.spend, statistics)).max(0) as u64;
        let clipped: Vec<Vec<f64>> = group.contributors.values().map(|t| t.iter().zip(&metrics).map(|(v, m)| v.clamp(m.lower, m.upper)).collect()).collect();
        let masses = (!metrics.is_empty()).then(|| clipped.iter().map(|t| t.iter().map(|v| v.abs()).sum()).collect());
        candidates.push(((group.key, group.carried, clipped), GroupStats { noised_contributors: noised, contributor_masses: masses }));
    }
    let outcome = suppress(&floor, candidates);

    let mut rows = Vec::new();
    for (key, carried_values, clipped) in outcome.published {
        let mut row = Map::new();
        for c in &published {
            if let Some(v) = key.get(c).or_else(|| carried_values.get(c)) {
                row.insert(c.clone(), v.clone());
            }
        }
        for (i, m) in metrics.iter().enumerate() {
            let units: i64 = clipped.iter().map(|t| (t[i] / m.quantum).round() as i64).fold(0i64, i64::saturating_add);
            let noised = units.saturating_add(statistic_noise(entropy, m.sensitivity, request.spend, statistics));
            let figure = noised as f64 * m.quantum;
            let value = if figure.fract() == 0.0 && figure.abs() < 9.0e15 { Value::from(figure as i64) } else { Value::from(figure) };
            row.insert(m.name.clone(), value);
        }
        rows.push(row);
    }
    let suppressed_present = outcome.sentinel.is_some();
    if suppressed_present && policy.emit_sentinel {
        // The sentinel marks the group-key columns; with no group key, the first column.
        let mut row = Map::new();
        for (i, c) in published.iter().enumerate() {
            let marked = if group_key.is_empty() { i == 0 } else { group_key.contains(&c) };
            row.insert(c.clone(), if marked { Value::from(DISCLOSURE_SENTINEL) } else { Value::Null });
        }
        rows.push(row);
    }
    let envelope = ReleaseEnvelope {
        table: request.table.clone(),
        policy_hash: policy_hash(policy),
        reserved_spend: request.spend.micros(),
        suppressed_present,
        row_count: rows.len(),
    };
    Ok(Release { table: DerivedTable { name: request.table.clone(), columns: published, rows }, envelope })
}
