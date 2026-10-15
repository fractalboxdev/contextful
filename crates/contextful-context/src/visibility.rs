//! The mirrored access tables (`disclosure.mirror.grants-table`): one sweep run of a source
//! read against its current rows under `_visibility/<source>/`, then landed through the
//! land path as engine-owned runs, `freshness` last.
//!
//! A run is committed once its `freshness` row lands. The `freshness` row with the
//! greatest epoch names the run whose `resources`, `grants` and `principals` rows are
//! current; `tombstones` accumulate across committed runs alone. A row whose `_run_id`
//! names no committed run joins nothing, so a run that stops before `freshness` lands
//! changes nothing a reader resolves, and its retry lands its tombstones once.

use crate::error::{ContextError, Result};
use crate::land::{land_engine_owned, Batch, RunContext};
use crate::rows::batch_rows;
use crate::scan::scan;
use crate::store::Store;
use contextful_core::disclosure::declare::Binding;
use contextful_core::disclosure::sweep::{
    self, commit, Commit, Freshness, GrantLine, GrantRow, Governed, Observation, PrincipalRow, ResourceRow, SourceState, SweepBlock, SweepKind, Tombstone,
};
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::{NodeId, RUNS_DIR};
use contextful_core::store::reconcile::ColumnType;
use contextful_core::store::reserve::{Injection, RUN_ID};
use contextful_core::time::Instant;
use serde_json::{Map, Value};
use std::collections::{BTreeSet, HashMap};

/// Where a sweep run lands: the node and site its runs carry.
#[derive(Debug, Clone)]
pub struct Landing {
    pub node: NodeId,
    pub site_id: String,
}

/// The rows of `table` the store holds, each with its `_run_id`; an absent table holds none.
/// With `runs`, only rows of those runs: a run's own files are read and other runs' skipped,
/// and a folded file is read and filtered by `_run_id`.
fn rows_of(store: &Store, table: &str, columns: &[&str], runs: Option<&BTreeSet<String>>) -> Result<Vec<Map<String, Value>>> {
    if store.try_schema(table)?.is_none() {
        return Ok(Vec::new());
    }
    let mut wanted: Vec<&str> = columns.to_vec();
    wanted.push(RUN_ID);
    let runs_dir = format!("/{RUNS_DIR}/");
    let mut out = Vec::new();
    for f in scan(store, &TableDecl::named(table), Bounds::default())?.files {
        if let Some(runs) = runs {
            let run = f.split_once(&runs_dir).and_then(|(_, rest)| rest.split('/').next());
            if run.is_some_and(|r| !runs.contains(r)) {
                continue;
            }
        }
        for batch in store.read_parquet(&store.logical_path(&f)?)? {
            out.extend(batch_rows(&batch, &wanted)?);
        }
    }
    if let Some(runs) = runs {
        out.retain(|r| r.get(RUN_ID).and_then(Value::as_str).is_some_and(|id| runs.contains(id)));
    }
    Ok(out)
}

fn rows(store: &Store, table: &str, columns: &[&str]) -> Result<Vec<Map<String, Value>>> {
    rows_of(store, table, columns, None)
}

/// The sweep ids of `source`'s committed runs: each one a `freshness` row names.
pub fn committed(store: &Store, source: &str) -> Result<BTreeSet<String>> {
    rows(store, &sweep::table(source, "freshness"), &["sweep_id"])?.iter().map(|r| text(r, "sweep_id")).collect()
}

fn text(row: &Map<String, Value>, column: &str) -> Result<String> {
    row.get(column).and_then(Value::as_str).map(str::to_string).ok_or_else(|| ContextError::Invalid(format!("access row lacks `{column}`")))
}

fn instant(row: &Map<String, Value>, column: &str) -> Result<Option<Instant>> {
    match row.get(column) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Instant::parse(s).map(Some).map_err(|e| ContextError::Invalid(format!("access row `{column}`: {e}"))),
        Some(other) => Err(ContextError::Invalid(format!("access row `{column}` holds {other}"))),
    }
}

fn required(row: &Map<String, Value>, column: &str) -> Result<Instant> {
    instant(row, column)?.ok_or_else(|| ContextError::Invalid(format!("access row lacks `{column}`")))
}

/// The freshness row of `source` carrying the greatest epoch, if any run committed.
pub fn freshness(store: &Store, source: &str) -> Result<Option<Freshness>> {
    let cols = ["source", "epoch", "sweep_id", "sweep_kind", "watermark_at", "last_full_sweep_at", "last_incremental_at", "budget_seconds"];
    let mut best: Option<Freshness> = None;
    for r in rows(store, &sweep::table(source, "freshness"), &cols)? {
        let epoch = r.get("epoch").and_then(Value::as_i64).and_then(|e| u64::try_from(e).ok()).ok_or_else(|| ContextError::Invalid("freshness row lacks `epoch`".into()))?;
        if best.as_ref().is_some_and(|b| b.epoch >= epoch) {
            continue;
        }
        best = Some(Freshness {
            source: text(&r, "source")?,
            epoch,
            sweep_id: text(&r, "sweep_id")?,
            sweep_kind: text(&r, "sweep_kind")?,
            watermark_at: instant(&r, "watermark_at")?,
            last_full_sweep_at: instant(&r, "last_full_sweep_at")?,
            last_incremental_at: instant(&r, "last_incremental_at")?,
            budget_seconds: r.get("budget_seconds").and_then(Value::as_i64).and_then(|b| u64::try_from(b).ok()),
        });
    }
    Ok(best)
}

/// The current rows of `source`: its latest freshness row and the `resources` and `grants`
/// rows that row's run landed.
pub fn state(store: &Store, source: &str) -> Result<SourceState> {
    let Some(fresh) = freshness(store, source)? else { return Ok(SourceState::default()) };
    let current = BTreeSet::from([fresh.sweep_id.clone()]);
    let mut resources = Vec::new();
    for r in rows_of(store, &sweep::table(source, "resources"), &["resource_id", "resource_kind", "visibility_class", "acl_observed_at", "acl_sweep_kind", "acl_sweep_id"], Some(&current))? {
        resources.push(ResourceRow {
            resource_id: text(&r, "resource_id")?,
            resource_kind: text(&r, "resource_kind")?,
            visibility_class: text(&r, "visibility_class")?,
            observed_at: required(&r, "acl_observed_at")?,
            sweep_kind: text(&r, "acl_sweep_kind")?,
            sweep_id: text(&r, "acl_sweep_id")?,
        });
    }
    let mut grants = Vec::new();
    for r in rows_of(store, &sweep::table(source, "grants"), &["resource_id", "principal", "principal_kind", "level", "acl_observed_at", "acl_sweep_kind", "acl_sweep_id"], Some(&current))? {
        grants.push(GrantRow {
            resource_id: text(&r, "resource_id")?,
            principal: text(&r, "principal")?,
            principal_kind: text(&r, "principal_kind")?,
            level: text(&r, "level")?,
            observed_at: required(&r, "acl_observed_at")?,
            sweep_kind: text(&r, "acl_sweep_kind")?,
            sweep_id: text(&r, "acl_sweep_id")?,
        });
    }
    Ok(SourceState { resources, grants, freshness: Some(fresh) })
}

/// The principals of `source`'s current run.
pub fn principals(store: &Store, source: &str) -> Result<Vec<PrincipalRow>> {
    let Some(fresh) = freshness(store, source)? else { return Ok(Vec::new()) };
    let current = BTreeSet::from([fresh.sweep_id]);
    let mut out: Vec<PrincipalRow> = rows_of(store, &sweep::table(source, "principals"), &["principal", "principal_kind"], Some(&current))?
        .iter()
        .map(|r| Ok(PrincipalRow { principal: text(r, "principal")?, principal_kind: text(r, "principal_kind")? }))
        .collect::<Result<_>>()?;
    out.sort();
    Ok(out)
}

/// Every tombstone `source`'s committed runs landed, in revocation order.
pub fn tombstones(store: &Store, source: &str) -> Result<Vec<Tombstone>> {
    let runs = committed(store, source)?;
    if runs.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for r in rows_of(store, &sweep::table(source, "tombstones"), &["scope", "resource_id", "principal", "level", "revoked_at"], Some(&runs))? {
        let scope = match text(&r, "scope")?.as_str() {
            "grant" => "grant",
            "resource" => "resource",
            other => return Err(ContextError::Invalid(format!("tombstone scope `{other}`"))),
        };
        let opt = |c: &str| r.get(c).and_then(Value::as_str).map(str::to_string);
        out.push(Tombstone { scope, resource_id: text(&r, "resource_id")?, principal: opt("principal"), level: opt("level"), revoked_at: required(&r, "revoked_at")? });
    }
    out.sort_by(|a, b| (a.revoked_at, &a.resource_id, &a.principal).cmp(&(b.revoked_at, &b.resource_id, &b.principal)));
    Ok(out)
}

/// The governed resources of `binding`'s table: each distinct `resource_key` value its rows
/// hold, of the binding's `resource_kind` and the class its fidelity sets.
pub fn governed(store: &Store, binding: &Binding) -> Result<Vec<Governed>> {
    let mut ids = BTreeSet::new();
    for r in rows(store, &binding.table, &[binding.resource_key.as_str()])? {
        match r.get(&binding.resource_key) {
            Some(Value::String(s)) => {
                ids.insert(s.clone());
            }
            Some(Value::Null) | None => {}
            Some(other) => {
                ids.insert(other.to_string());
            }
        }
    }
    let kind = binding.resource_kind.clone().unwrap_or_else(|| binding.table.clone());
    let class = sweep::visibility_class(binding.fidelity).to_string();
    Ok(ids.into_iter().map(|resource_id| Governed { resource_id, resource_kind: kind.clone(), visibility_class: class.clone() }).collect())
}

fn ts(i: Instant) -> Value {
    Value::String(i.to_rfc3339_nanos())
}

fn opt_ts(i: Option<Instant>) -> Value {
    i.map_or(Value::Null, ts)
}

fn opt_text(s: &Option<String>) -> Value {
    s.as_ref().map_or(Value::Null, |s| Value::String(s.clone()))
}

fn object(pairs: Vec<(&str, Value)>) -> Map<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// The batches one commit lands, in [`sweep::TABLES`] order; an empty table lands no run.
fn batches(source: &str, c: &Commit, now: Instant) -> Vec<(&'static str, Batch)> {
    use ColumnType::{Int64, Timestamp, Utf8};
    let typed = |cols: &[(&str, ColumnType)]| cols.iter().map(|(c, t)| (c.to_string(), t.clone())).collect::<HashMap<_, _>>();
    let observed = [("acl_observed_at", Timestamp), ("acl_sweep_kind", Utf8), ("acl_sweep_id", Utf8)];
    let resources = c
        .resources
        .iter()
        .map(|r| {
            object(vec![
                ("resource_id", r.resource_id.clone().into()),
                ("source", source.into()),
                ("resource_kind", r.resource_kind.clone().into()),
                ("container_id", Value::Null),
                ("visibility_class", r.visibility_class.clone().into()),
                ("acl_observed_at", ts(r.observed_at)),
                ("acl_sweep_kind", r.sweep_kind.clone().into()),
                ("acl_sweep_id", r.sweep_id.clone().into()),
            ])
        })
        .collect();
    let grants = c
        .grants
        .iter()
        .map(|g| {
            object(vec![
                ("resource_id", g.resource_id.clone().into()),
                ("principal", g.principal.clone().into()),
                ("principal_kind", g.principal_kind.clone().into()),
                ("level", g.level.clone().into()),
                ("expires_at", Value::Null),
                ("acl_observed_at", ts(g.observed_at)),
                ("acl_sweep_kind", g.sweep_kind.clone().into()),
                ("acl_sweep_id", g.sweep_id.clone().into()),
            ])
        })
        .collect();
    let principals = c
        .principals
        .iter()
        .map(|p| object(vec![("principal", p.principal.clone().into()), ("principal_kind", p.principal_kind.clone().into()), ("source_label", Value::Null)]))
        .collect();
    let tombstones = c
        .tombstones
        .iter()
        .map(|t| {
            object(vec![
                ("scope", t.scope.into()),
                ("resource_id", t.resource_id.clone().into()),
                ("principal", opt_text(&t.principal)),
                ("level", opt_text(&t.level)),
                ("revoked_at", ts(t.revoked_at)),
            ])
        })
        .collect();
    let f = &c.freshness;
    let freshness = vec![object(vec![
        ("source", f.source.clone().into()),
        ("epoch", Value::from(f.epoch)),
        ("sweep_id", f.sweep_id.clone().into()),
        ("sweep_kind", f.sweep_kind.clone().into()),
        ("watermark_at", opt_ts(f.watermark_at)),
        ("last_full_sweep_at", opt_ts(f.last_full_sweep_at)),
        ("last_incremental_at", opt_ts(f.last_incremental_at)),
        ("lag_seconds", f.lag_secs(now).map_or(Value::Null, Value::from)),
        ("budget_seconds", f.budget_seconds.map_or(Value::Null, Value::from)),
    ])];
    let mut resource_types = vec![("resource_id", Utf8), ("source", Utf8), ("resource_kind", Utf8), ("container_id", Utf8), ("visibility_class", Utf8)];
    resource_types.extend(observed.iter().cloned());
    let mut grant_types = vec![("resource_id", Utf8), ("principal", Utf8), ("principal_kind", Utf8), ("level", Utf8), ("expires_at", Timestamp)];
    grant_types.extend(observed.iter().cloned());
    vec![
        ("resources", Batch { rows: resources, types: typed(&resource_types) }),
        ("grants", Batch { rows: grants, types: typed(&grant_types) }),
        ("principals", Batch { rows: principals, types: typed(&[("principal", Utf8), ("principal_kind", Utf8), ("source_label", Utf8)]) }),
        ("group_members", Batch { rows: Vec::new(), types: HashMap::new() }),
        ("tombstones", Batch { rows: tombstones, types: typed(&[("scope", Utf8), ("resource_id", Utf8), ("principal", Utf8), ("level", Utf8), ("revoked_at", Timestamp)]) }),
        (
            "freshness",
            Batch {
                rows: freshness,
                types: typed(&[
                    ("source", Utf8),
                    ("epoch", Int64),
                    ("sweep_id", Utf8),
                    ("sweep_kind", Utf8),
                    ("watermark_at", Timestamp),
                    ("last_full_sweep_at", Timestamp),
                    ("last_incremental_at", Timestamp),
                    ("lag_seconds", Int64),
                    ("budget_seconds", Int64),
                ]),
            },
        ),
    ]
}

/// Run one sweep of `block`'s source from a `--grants` read, under the source's write lock:
/// read the governed resources and the current rows, compute the commit, and land each
/// non-empty access table as run `<sweep id>`, `freshness` last. A refusal lands nothing.
#[allow(clippy::too_many_arguments)]
pub fn sweep_grants(
    store: &Store,
    at: &Landing,
    block: &SweepBlock,
    bindings: &[Binding],
    kind: SweepKind,
    grants: Vec<GrantLine>,
    began_at: Instant,
    committed_at: Instant,
) -> Result<Commit> {
    store.check_writable("visibility sweep")?;
    let enumerated = block.enumerated(bindings).map_err(|e| ContextError::Invalid(e.to_string()))?;
    let _lock = store.lock_commit(&format!("{}/{}", sweep::NAMESPACE, block.source))?;
    let prior = state(store, &block.source)?;
    let epoch = prior.freshness.as_ref().map_or(0, |f| f.epoch) + 1;
    let mut nonce = [0u8; 4];
    getrandom::fill(&mut nonce).map_err(|e| ContextError::Invalid(format!("sweep nonce: {e}")))?;
    let sweep_id = format!("sweep-{epoch}-{}-{}", began_at.unix_nanos(), crate::store::hex(&nonce));
    let mut obs = Observation::from_grants_file(&block.source, kind, &sweep_id, began_at, governed(store, enumerated)?, grants);
    obs.budget_seconds = bindings.iter().filter(|b| b.source == block.source).filter_map(|b| b.max_acl_staleness_secs).min();
    let c = commit(&prior, &obs)?;
    for (name, batch) in batches(&block.source, &c, committed_at) {
        if batch.rows.is_empty() {
            continue;
        }
        let injection = Injection { run_id: sweep_id.clone(), site_id: at.site_id.clone(), batch_seq: None, authored_by: None, taint: None };
        land_engine_owned(store, &TableDecl::named(sweep::table(&block.source, name)), &batch, &RunContext { node: at.node.clone(), injection, committed_at })?;
    }
    Ok(c)
}
