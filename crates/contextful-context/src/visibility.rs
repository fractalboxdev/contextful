//! The mirrored access tables (`disclosure.mirror.grants-table`): one sweep run of a source
//! read against its current rows under `_visibility/<source>/`, then landed through the
//! land path as engine-owned runs, `freshness` last.
//!
//! A run is committed once its `freshness` row lands. The `freshness` row with the
//! greatest epoch names the run whose `resources`, `grants`, `principals`, `group_members`
//! and `identity_links` rows are current; `tombstones` accumulate across committed runs alone. A row whose `_run_id`
//! names no committed run joins nothing, so a run that stops before `freshness` lands
//! changes nothing a reader resolves, and its retry lands its tombstones once.
//!
//! Each run carries its source's complete current state, so before it lands a run collects
//! every run of the current-state tables but the current one
//! (`disclosure.sweep.superseded-run`); `tombstones` and `freshness` keep every run.

use crate::error::{ContextError, Result};
use crate::land::{land_engine_owned, Batch, RunContext};
use crate::rows::batch_rows;
use crate::scan::scan;
use crate::store::Store;
use contextful_core::disclosure::declare::Binding;
use contextful_core::disclosure::sweep::{
    self, commit, Commit, Directory, Freshness, GrantRow, GrantsRead, Governed, MemberRow, Observation, PrincipalRow, ResourceRow, SourceState, SweepBlock,
    SweepKind, Tombstone,
};
use contextful_core::identify::{IdentityLink, LinkMethod, NormalizedSubject};
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
    for r in rows_of(store, &sweep::table(source, "grants"), &["resource_id", "principal", "principal_kind", "level", "expires_at", "acl_observed_at", "acl_sweep_kind", "acl_sweep_id"], Some(&current))? {
        grants.push(GrantRow {
            resource_id: text(&r, "resource_id")?,
            principal: text(&r, "principal")?,
            principal_kind: text(&r, "principal_kind")?,
            level: text(&r, "level")?,
            expires_at: instant(&r, "expires_at")?,
            observed_at: required(&r, "acl_observed_at")?,
            sweep_kind: text(&r, "acl_sweep_kind")?,
            sweep_id: text(&r, "acl_sweep_id")?,
        });
    }
    let mut members = Vec::new();
    for r in rows_of(store, &sweep::table(source, "group_members"), &["group", "member", "member_kind", "acl_observed_at"], Some(&current))? {
        members.push(MemberRow { group: text(&r, "group")?, member: text(&r, "member")?, member_kind: text(&r, "member_kind")?, observed_at: required(&r, "acl_observed_at")? });
    }
    members.sort();
    let mut links = Vec::new();
    for r in rows_of(store, &sweep::table(source, "identity_links"), &["source_principal", "subject", "method", "confidence"], Some(&current))? {
        let method = match text(&r, "method")?.as_str() {
            "scim_email" => LinkMethod::ScimEmail,
            "oidc_sub" => LinkMethod::OidcSub,
            "operator_asserted" => LinkMethod::OperatorAsserted,
            other => return Err(ContextError::Invalid(format!("identity link method `{other}`"))),
        };
        let confidence = r.get("confidence").and_then(Value::as_f64).ok_or_else(|| ContextError::Invalid("identity link lacks `confidence`".into()))?;
        links.push(IdentityLink { source: source.to_string(), source_principal: text(&r, "source_principal")?, on_behalf_of: text(&r, "subject")?, method, confidence });
    }
    links.sort_by(|a, b| (&a.source_principal, &a.on_behalf_of, a.method.as_str()).cmp(&(&b.source_principal, &b.on_behalf_of, b.method.as_str())));
    Ok(SourceState { resources, grants, members, links, freshness: Some(fresh) })
}

/// The resource ids of `source` on which a current grant reaches `subject` through the
/// source's current rows (`authority.identify.unlinked-principal`), resolved as a read
/// resolves them before the semi-join's level and expiry filter; see
/// [`reach::resolve`](contextful_core::disclosure::reach::resolve).
pub fn reach(store: &Store, source: &str, subject: &NormalizedSubject) -> Result<BTreeSet<String>> {
    use contextful_core::disclosure::reach::{resolve, ClosureBound, ReachError};
    match resolve(source, subject, &state(store, source)?, ClosureBound::default()) {
        Ok(set) => Ok(set.held.into_keys().collect()),
        Err(ReachError::Authority(a)) => Err(a.into()),
        Err(ReachError::Visibility(v)) => Err(v.into()),
    }
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
    use ColumnType::{Float64, Int64, Timestamp, Utf8};
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
                ("expires_at", opt_ts(g.expires_at)),
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
    let members = c
        .members
        .iter()
        .map(|m| {
            object(vec![
                ("group", m.group.clone().into()),
                ("member", m.member.clone().into()),
                ("member_kind", m.member_kind.clone().into()),
                ("acl_observed_at", ts(m.observed_at)),
            ])
        })
        .collect();
    let links = c
        .links
        .iter()
        .map(|l| {
            object(vec![
                ("source_principal", l.source_principal.clone().into()),
                ("subject", l.on_behalf_of.clone().into()),
                ("method", l.method.as_str().into()),
                ("confidence", Value::from(l.confidence)),
            ])
        })
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
        ("group_members", Batch { rows: members, types: typed(&[("group", Utf8), ("member", Utf8), ("member_kind", Utf8), ("acl_observed_at", Timestamp)]) }),
        (
            "identity_links",
            Batch { rows: links, types: typed(&[("source_principal", Utf8), ("subject", Utf8), ("method", Utf8), ("confidence", Float64)]) },
        ),
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

/// The access tables holding only the current run's rows; `tombstones` and `freshness`
/// accumulate across runs.
const CURRENT_STATE: [&str; 5] = ["resources", "grants", "principals", "group_members", "identity_links"];

/// Collect every run of `source`'s current-state tables other than `current`
/// (`disclosure.sweep.superseded-run`), returning the run directories removed.
fn retire(store: &Store, source: &str, current: Option<&str>) -> Result<Vec<String>> {
    let mut removed = Vec::new();
    for name in CURRENT_STATE {
        let table = sweep::table(source, name);
        if store.try_schema(&table)?.is_none() {
            continue;
        }
        let runs_dir = store.table_dir(&table)?.join(RUNS_DIR);
        for r in store.committed_runs(&table)?.into_iter().filter(|r| Some(r.run_id.as_str()) != current) {
            let dir = runs_dir.join(&r.run_id);
            let node = dir.join(&r.node_id);
            if node.exists() {
                std::fs::remove_dir_all(&node).map_err(|source| ContextError::Io { path: node.clone(), source })?;
                removed.push(format!("{table}/{RUNS_DIR}/{}/{}", r.run_id, r.node_id));
            }
            if std::fs::read_dir(&dir).is_ok_and(|mut d| d.next().is_none()) {
                std::fs::remove_dir(&dir).map_err(|source| ContextError::Io { path: dir.clone(), source })?;
            }
        }
    }
    Ok(removed)
}

/// Run one sweep of `block`'s source from a `--grants` read and an optional `--directory`
/// read, under the source's write lock: read the governed resources and the current rows,
/// compute the commit, collect the runs the current one superseded, and land each
/// non-empty access table as run `<sweep id>`, `freshness` last. A refusal lands and
/// collects nothing.
#[allow(clippy::too_many_arguments)]
pub fn sweep_read(
    store: &Store,
    at: &Landing,
    block: &SweepBlock,
    bindings: &[Binding],
    kind: SweepKind,
    read: GrantsRead,
    directory: Option<Directory>,
    began_at: Instant,
    committed_at: Instant,
) -> Result<Commit> {
    store.check_writable("visibility sweep")?;
    let enumerated = block.enumerated(bindings).map_err(|e| ContextError::Invalid(e.to_string()))?;
    // A webhook delivery naming no resource applies nothing and moves no watermark
    // (`disclosure.sweep.empty-delivery`).
    if kind == SweepKind::Webhook && read.named().is_empty() {
        return Err(ContextError::Invalid(format!("webhook sweep of `{}`: the delivery names no resource", block.source)));
    }
    let _lock = store.lock_commit(&format!("{}/{}", sweep::NAMESPACE, block.source))?;
    let prior = state(store, &block.source)?;
    let epoch = prior.freshness.as_ref().map_or(0, |f| f.epoch) + 1;
    let mut nonce = [0u8; 4];
    getrandom::fill(&mut nonce).map_err(|e| ContextError::Invalid(format!("sweep nonce: {e}")))?;
    let sweep_id = format!("sweep-{epoch}-{}-{}", began_at.unix_nanos(), crate::store::hex(&nonce));
    let mut obs = Observation::from_read(&block.source, kind, &sweep_id, began_at, governed(store, enumerated)?, read);
    obs.directory = directory;
    obs.events = block.events.clone();
    obs.budget_seconds = bindings.iter().filter(|b| b.source == block.source).filter_map(|b| b.max_acl_staleness_secs).min();
    let c = commit(&prior, &obs)?;
    retire(store, &block.source, prior.freshness.as_ref().map(|f| f.sweep_id.as_str()))?;
    for (name, batch) in batches(&block.source, &c, committed_at) {
        if batch.rows.is_empty() {
            continue;
        }
        let injection = Injection { run_id: sweep_id.clone(), site_id: at.site_id.clone(), batch_seq: None, authored_by: None, authored_agent: None, taint: None };
        land_engine_owned(store, &TableDecl::named(sweep::table(&block.source, name)), &batch, &RunContext { node: at.node.clone(), injection, committed_at })?;
    }
    Ok(c)
}
