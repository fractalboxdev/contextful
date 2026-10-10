//! `run.model` and `run.publish` over the store: a model build materialized into
//! staging and published by the table pointer with its manifest section, holds on
//! published builds, and the history logs derived from committed manifests.

use crate::error::{ContextError, IoPath, Result};
use crate::store::{sorted_dirs, FileLock, Store, LOCK_WAIT_SECS};
use contextful_core::pipeline::model::{
    build_entries, contract_history, regenerate, BuildEntry, ContractHistoryEntry, HoldRecord, PublishSection, Receipt,
    BUILDS_LOG, CONTRACT_HISTORY_LOG, HOLDS_DIR, HOLDS_LOG,
};
use contextful_core::run::RunError;
use contextful_core::store::lay_out::{SnapshotManifest, MANIFEST_FILE, STAGING_SUFFIX};
use contextful_core::time::Instant;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(feature = "read")]
pub use materialize::{admit_statements, build, build_guarded, BuildRequest, Built};

/// The section of the snapshot the model's pointer names, when that build published one.
pub fn current_section(store: &Store, model: &str) -> Result<Option<PublishSection>> {
    let (chain, _) = store.chain(model)?;
    Ok(chain.into_iter().next().and_then(|m| m.publish))
}

const ATTEMPTS_DIR: &str = "build-attempts";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BuildAttempt {
    build_id: String,
    started_at: Instant,
    contract_version: String,
    schema_fingerprint: String,
    disclosure_digest: String,
    #[serde(default)]
    status: Option<contextful_core::pipeline::model::BuildStatus>,
    #[serde(default)]
    completed_at: Option<Instant>,
}

fn attempts(store: &Store, model: &str) -> Result<Vec<BuildAttempt>> {
    let dir = store.table_dir(model)?.join(ATTEMPTS_DIR);
    let mut out = Vec::new();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(ContextError::Io { path: dir, source: e }),
    };
    for entry in entries {
        let path = entry.at(&dir)?.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            let bytes = store.metadata_files().read(&path)?;
            out.push(serde_json::from_slice(&bytes).map_err(|e| {
                contextful_core::store::StoreError::StoreManifestUnreadable(format!("file `{}`: {e}", path.display()))
            })?);
        }
    }
    out.sort_by(|a: &BuildAttempt, b: &BuildAttempt| (a.started_at, &a.build_id).cmp(&(b.started_at, &b.build_id)));
    Ok(out)
}

fn record_attempt(store: &Store, model: &str, attempt: &BuildAttempt) -> Result<()> {
    let dir = store.table_dir(model)?.join(ATTEMPTS_DIR);
    fs::create_dir_all(&dir).at(&dir)?;
    let path = dir.join(format!("{}.json", attempt.build_id));
    store.metadata_files().replace(&path, serde_json::to_vec_pretty(attempt).expect("a build attempt serializes").as_slice())
}

fn refuse_attempt(store: &Store, model: &str, build_id: &str, completed_at: Instant) -> Result<()> {
    if let Some(mut attempt) = attempts(store, model)?.into_iter().find(|a| a.build_id == build_id) {
        attempt.status = Some(contextful_core::pipeline::model::BuildStatus::Refused);
        attempt.completed_at = Some(completed_at);
        record_attempt(store, model, &attempt)?;
    }
    Ok(())
}

fn attempt_running(store: &Store, model: &str, attempt: &BuildAttempt) -> Result<bool> {
    let dir = store.table_dir(model)?.join(contextful_core::store::lay_out::SNAPSHOTS_DIR)
        .join(format!("{}{}", attempt.build_id, STAGING_SUFFIX));
    if !dir.exists() {
        return Ok(false);
    }
    let lock = dir.join(crate::fold::STAGING_LOCK);
    Ok(FileLock::try_acquire(&lock)?.is_none())
}

/// The latest attempt and the latest committed build are separate so an interrupted
/// materialization cannot change the build a reader sees.
pub fn status(store: &Store, model: &str, now: Instant) -> Result<serde_json::Value> {
    let section = current_section(store, model)?;
    let mut latest = attempts(store, model)?.into_iter().last();
    if let Some(ref mut attempt) = latest {
        if attempt.status.is_none()
            && section.as_ref().is_none_or(|s| s.build_id != attempt.build_id)
            && !attempt_running(store, model, attempt)?
        {
            attempt.status = Some(contextful_core::pipeline::model::BuildStatus::Failed);
            attempt.completed_at = Some(now);
            record_attempt(store, model, attempt)?;
        }
    }
    let (last_build_id, last_build_status) = match latest {
        Some(ref attempt) if section.as_ref().is_some_and(|s| s.build_id == attempt.build_id) =>
            (Some(attempt.build_id.clone()), "published"),
        Some(ref attempt) if section.as_ref().is_some_and(|s| s.build_started_at > attempt.started_at) =>
            (section.as_ref().map(|s| s.build_id.clone()), "published"),
        Some(ref attempt) if attempt.status == Some(contextful_core::pipeline::model::BuildStatus::Refused) =>
            (Some(attempt.build_id.clone()), "refused"),
        Some(ref attempt) if attempt.status == Some(contextful_core::pipeline::model::BuildStatus::Failed) =>
            (Some(attempt.build_id.clone()), "failed"),
        Some(ref attempt) if attempt_running(store, model, attempt)? =>
            (Some(attempt.build_id.clone()), "building"),
        Some(attempt) => (Some(attempt.build_id), "failed"),
        None => (section.as_ref().map(|s| s.build_id.clone()), if section.is_some() { "published" } else { "absent" }),
    };
    Ok(serde_json::json!({
        "model": model,
        "last_build_id": last_build_id,
        "last_build_status": last_build_status,
        "published_build_id": section.as_ref().map(|s| &s.build_id),
        "freshness": section.as_ref().map(PublishSection::freshness),
        "stale": section.as_ref().map(|s| s.freshness().stale(now)),
    }))
}

/// Every snapshot manifest on disk for the model, staging excluded, oldest first: the
/// pointer's chain and any snapshot a hold kept beyond it.
pub fn manifests(store: &Store, model: &str) -> Result<Vec<SnapshotManifest>> {
    let dir = store.table_dir(model)?.join(contextful_core::store::lay_out::SNAPSHOTS_DIR);
    let mut out = Vec::new();
    for d in sorted_dirs(&dir)? {
        if d.file_name().is_some_and(|n| n.to_string_lossy().ends_with(STAGING_SUFFIX)) {
            continue;
        }
        let path = d.join(MANIFEST_FILE);
        let Some(bytes) = store.metadata_files().read_optional(&path)? else { continue };
        let m: SnapshotManifest = serde_json::from_slice(&bytes).map_err(|e| {
            contextful_core::store::StoreError::StoreManifestUnreadable(format!("table `{model}`: file `{}`: {e}", path.display()))
        })?;
        out.push(m);
    }
    out.sort_by(|a, b| a.snapshot_id.cmp(&b.snapshot_id));
    Ok(out)
}

fn holds_dir(store: &Store, model: &str) -> Result<PathBuf> {
    Ok(store.table_dir(model)?.join(HOLDS_DIR))
}

/// Every hold manifest of the model (`run.model.hold-manifest`), ordered by build id.
pub fn holds(store: &Store, model: &str) -> Result<Vec<HoldRecord>> {
    let dir = holds_dir(store, model)?;
    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(ContextError::Io { path: dir, source: e }),
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry.at(&dir)?.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let bytes = store.metadata_files().read(&path)?;
        let record: HoldRecord = serde_json::from_slice(&bytes).map_err(|e| {
            contextful_core::store::StoreError::StoreManifestUnreadable(format!("table `{model}`: file `{}`: {e}", path.display()))
        })?;
        out.push(record);
    }
    out.sort_by(|a, b| a.build_id.cmp(&b.build_id));
    Ok(out)
}

/// The build ids an unexpired hold covers at `now`.
pub fn held(store: &Store, model: &str, now: Instant) -> Result<BTreeSet<String>> {
    Ok(holds(store, model)?.into_iter().filter(|h| h.active(now)).map(|h| h.build_id).collect())
}

/// Hold one published build for `secs` from `now` (`run.model.hold-verb`): a build no
/// committed manifest records refuses (`run.model.hold-unknown-build`); an unexpired hold
/// on it is renewed, anything else is held anew.
pub fn hold(store: &Store, model: &str, build_id: &str, principal: &str, secs: u64, now: Instant) -> Result<(Receipt, HoldRecord)> {
    store.check_writable("build hold")?;
    let published: Vec<String> = manifests(store, model)?.into_iter().filter_map(|m| m.publish.map(|s| s.build_id)).collect();
    if !published.iter().any(|b| b == build_id) {
        return Err(RunError::ModelBuildUnknown(format!(
            "model `{model}` holds no published build `{build_id}`; its committed builds are [{}]",
            published.join(", ")
        ))
        .into());
    }
    let dir = holds_dir(store, model)?;
    fs::create_dir_all(&dir).at(&dir)?;
    let _lock = FileLock::acquire(&dir.join(".lock"), std::time::Duration::from_secs(LOCK_WAIT_SECS))?;
    let path = dir.join(format!("{build_id}.json"));
    let prior: Option<HoldRecord> = store.metadata_files().read_optional(&path)?.and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let receipt = if prior.is_some_and(|p| p.active(now)) { Receipt::Renewed } else { Receipt::Held };
    let record = HoldRecord { build_id: build_id.to_string(), principal: principal.to_string(), placed_at: now, expires_at: now.plus_secs(secs) };
    store.metadata_files().replace(&path, serde_json::to_string_pretty(&record).expect("a hold serializes").as_bytes())?;
    write_logs(store, model)?;
    Ok((receipt, record))
}

fn read_log<T: DeserializeOwned>(store: &Store, path: &Path) -> Result<Vec<T>> {
    match store.metadata_files().read_optional(path)? {
        // A line no entry type reads disagrees with every manifest, so regeneration drops it.
        Some(bytes) => {
            let text = String::from_utf8(bytes).map_err(|e| ContextError::Invalid(format!("{}: {e}", path.display())))?;
            Ok(text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
        }
        None => Ok(Vec::new()),
    }
}

fn write_log<T: Serialize>(store: &Store, path: &Path, entries: &[T]) -> Result<()> {
    let mut text = String::new();
    for e in entries {
        text.push_str(&serde_json::to_string(e).expect("a log entry serializes"));
        text.push('\n');
    }
    store.metadata_files().replace(path, text.as_bytes())
}

/// Regenerate `builds.jsonl`, `holds.jsonl` and `contract-history.jsonl` in the model's
/// table directory from its committed snapshot and hold manifests
/// (`run.publish.history-logs`, `run.model.log-regeneration`).
pub fn write_logs(store: &Store, model: &str) -> Result<()> {
    let dir = store.table_dir(model)?;
    fs::create_dir_all(&dir).at(&dir)?;
    let committed = manifests(store, model)?;

    let path = dir.join(BUILDS_LOG);
    let existing = read_log::<BuildEntry>(store, &path)?;
    let mut derived = build_entries(&committed);
    for attempt in attempts(store, model)? {
        if derived.iter().any(|entry| entry.build_id == attempt.build_id)
            || (attempt.status.is_none() && existing.iter().any(|entry| entry.build_id == attempt.build_id && matches!(entry.status,
                contextful_core::pipeline::model::BuildStatus::Published | contextful_core::pipeline::model::BuildStatus::Partial)))
            || (attempt.status.is_none() && attempt_running(store, model, &attempt)?) {
            continue;
        }
        derived.push(BuildEntry {
            build_id: attempt.build_id,
            started_at: attempt.started_at,
            completed_at: attempt.completed_at.unwrap_or(attempt.started_at),
            status: attempt.status.unwrap_or(contextful_core::pipeline::model::BuildStatus::Failed),
            contract_version: attempt.contract_version,
            schema_fingerprint: attempt.schema_fingerprint,
            partitions_unfilled: Vec::new(),
            disclosure_digest: attempt.disclosure_digest,
        });
    }
    let builds = regenerate(&existing, &derived, |e| e.build_id.clone(), |e| e.completed_at);
    write_log(store, &path, &builds)?;

    let path = dir.join(CONTRACT_HISTORY_LOG);
    let history =
        regenerate(&read_log::<ContractHistoryEntry>(store, &path)?, &contract_history(&committed), |e| e.build_id.clone(), |e| e.built_at);
    write_log(store, &path, &history)?;

    let path = dir.join(HOLDS_LOG);
    let key = |h: &HoldRecord| format!("{}@{}", h.build_id, h.placed_at.to_rfc3339_nanos());
    let placed = regenerate(&read_log::<HoldRecord>(store, &path)?, &holds(store, model)?, key, |h| h.placed_at);
    write_log(store, &path, &placed)
}

#[cfg(feature = "read")]
mod materialize {
    use super::*;
    use crate::fold::{claim, commit, Committed, Staged, STAGING_LOCK};
    use crate::parquet_io;
    use crate::read::engine::SqlEngine;
    use crate::read::{Face, ReadFault};
    use arrow_array::{Array, ArrayRef, Int64Array, RecordBatch, StringArray, TimestampNanosecondArray};
    use arrow_row::{RowConverter, SortField};
    use arrow_schema::{Field, Schema as ArrowSchema};
    use contextful_core::pipeline::model::{
        disclosure_digest, withholds_cells, BuildStatus, InputFrontier, ModelSpec, Watermark, FINGERPRINT_RECIPE, SEMANTICS_VERSION,
    };
    use contextful_core::read::guard::{admit, Admitted};
    use contextful_core::read::template::Bindings;
    use contextful_core::store::declare::TableDecl;
    use contextful_core::store::lay_out::{part_name, PartEntry, SnapshotId};
    use contextful_core::store::reconcile::{Column, ColumnType, FloatItem, Schema};
    use contextful_core::store::relation::{ident, literal};
    use contextful_core::store::reserve::{is_injected, COMMIT_SEQ, INGESTED_AT, ROW_SEQ, RUN_ID, SITE_ID};
    use duckdb::types::Value as Engine;
    use std::sync::Arc;

    /// One build of one model.
    #[derive(Debug, Clone)]
    pub struct BuildRequest<'a> {
        pub model: &'a ModelSpec,
        pub site_id: &'a str,
        /// The build's start: its snapshot id's instant and every row's `_ingested_at`.
        pub started_at: Instant,
        /// The instant the build commits as `last_built_at`.
        pub completed_at: Instant,
    }

    /// What a published build committed.
    #[derive(Debug, Clone, PartialEq)]
    pub struct Built {
        pub model: String,
        pub build_id: String,
        pub rows: u64,
        pub watermark: Watermark,
        pub section: Option<PublishSection>,
    }

    fn mismatch(why: String) -> ReadFault {
        ContextError::from(RunError::PipelineContractMismatch(why)).into()
    }

    fn invalid(why: String) -> ReadFault {
        ContextError::Invalid(why).into()
    }

    /// The store type an engine result type lands as, where one does.
    fn engine_type(s: &str) -> Option<ColumnType> {
        let s = s.trim().to_ascii_uppercase();
        if let Some(n) = s.strip_prefix("FLOAT[").and_then(|r| r.strip_suffix(']')) {
            return n.parse().ok().map(|n| ColumnType::FixedSizeList(FloatItem::Float32, n));
        }
        Some(match s.as_str() {
            "BOOLEAN" => ColumnType::Boolean,
            "INTEGER" => ColumnType::Int32,
            "BIGINT" => ColumnType::Int64,
            "DOUBLE" => ColumnType::Float64,
            "VARCHAR" => ColumnType::Utf8,
            "JSON" => ColumnType::Json,
            "TIMESTAMP WITH TIME ZONE" | "TIMESTAMPTZ" | "TIMESTAMP" | "TIMESTAMP_NS" | "TIMESTAMP_MS" | "TIMESTAMP_S" => ColumnType::Timestamp,
            "BLOB" => ColumnType::Binary,
            _ => return None,
        })
    }

    /// Whether an engine result type satisfies a declared type: it lands as that type, or
    /// as the engine's one spelling of a fixed-width byte or half-float vector type.
    fn satisfies(engine: &str, declared: &ColumnType) -> bool {
        match (engine_type(engine), declared) {
            (Some(t), d) if t == *d => true,
            (Some(ColumnType::Binary), ColumnType::FixedSizeBinary(_)) => true,
            (Some(ColumnType::FixedSizeList(FloatItem::Float32, n)), ColumnType::FixedSizeList(FloatItem::Float16, m)) => n == *m,
            _ => false,
        }
    }

    fn text(v: &Engine) -> String {
        match v {
            Engine::Text(s) => s.clone(),
            other => format!("{other:?}"),
        }
    }

    /// The injected columns a build lands on every row (`run.model.injected-columns`).
    fn injected_columns() -> Vec<Column> {
        vec![
            Column::new(INGESTED_AT, ColumnType::Timestamp, false),
            Column::new(RUN_ID, ColumnType::Utf8, false),
            Column::new(ROW_SEQ, ColumnType::Int64, false),
            Column::new(COMMIT_SEQ, ColumnType::Int64, false),
            Column::new(SITE_ID, ColumnType::Utf8, false),
        ]
    }

    /// One input's frontier: its current snapshot and the committed runs it omits, with
    /// the newest commit instant among them (`run.model.watermark`).
    fn frontier(store: &Store, decl: &TableDecl) -> Result<(InputFrontier, Option<Instant>)> {
        if store.try_schema(&decl.name)?.is_none() {
            return Ok((InputFrontier::default(), None));
        }
        let state = store.state(decl)?;
        let snapshot = state.chain.first();
        let runs = state.unfolded_runs();
        let at = snapshot.map(|s| s.created_at).into_iter().chain(runs.iter().map(|r| r.committed_at)).max();
        Ok((InputFrontier { snapshot_id: snapshot.map(|s| s.snapshot_id.to_string()), runs: runs.iter().map(|r| r.key()).collect() }, at))
    }

    /// The model's columns: its contract's, checked against the engine's description of
    /// its rows, or, undeclared, the rows' own (`run.publish.contract-mismatch`).
    fn columns(spec: &ModelSpec, described: &[(String, String)]) -> std::result::Result<Vec<Column>, ReadFault> {
        let Some(contract) = &spec.contract else {
            return described
                .iter()
                .map(|(name, ty)| {
                    engine_type(ty).map(|t| Column::new(name, t, true)).ok_or_else(|| {
                        invalid(format!("model `{}`: column `{name}` is {ty}, which no store type holds; cast it in the model's SQL", spec.id))
                    })
                })
                .collect();
        };
        let version = contract.version.as_str();
        if let Some((name, ty)) = described.iter().find(|(n, _)| !contract.columns.iter().any(|c| &c.name == n)) {
            return Err(mismatch(format!("model `{}`: column `{name}` ({ty}) is outside contract version {version}", spec.id)));
        }
        let mut out = Vec::new();
        for c in &contract.columns {
            let declared = c.column_type();
            match described.iter().find(|(n, _)| n == &c.name) {
                None => {
                    return Err(mismatch(format!(
                        "model `{}`: contract version {version} declares column `{}` as {}, which the model's rows lack",
                        spec.id, c.name, c.ty
                    )))
                }
                Some((_, ty)) if !satisfies(ty, &declared) => {
                    return Err(mismatch(format!(
                        "model `{}`: column `{}` is {ty}; contract version {version} declares {}",
                        spec.id, c.name, c.ty
                    )))
                }
                Some(_) => out.push(Column::new(&c.name, declared, c.nullable())),
            }
        }
        Ok(out)
    }

    /// Refuse a model whose table a landing wrote: a committed run, or a snapshot folding
    /// one, names it (`run.model.model-id`). A model build commits neither.
    fn check_owned(store: &Store, model: &str) -> std::result::Result<(), ReadFault> {
        let runs = store.committed_runs(model)?.len();
        let (chain, _) = store.chain(model)?;
        let folded = chain.iter().any(|s| !s.includes_runs.is_empty());
        if runs > 0 || folded {
            return Err(ContextError::from(RunError::PipelineTableNameCollision(format!(
                "model `{model}` builds table `{model}`, which a landing wrote ({runs} committed runs{}); name the model apart",
                if folded { ", folded into its snapshots" } else { "" }
            )))
            .into());
        }
        Ok(())
    }

    /// Refuse an input declaring a disclosure key (`run.model.restricted-input`): the build
    /// reads it unmasked, so the model's table serves its cells to every reader.
    fn check_unrestricted(model: &str, input: &TableDecl) -> std::result::Result<(), ReadFault> {
        let keys: Vec<&str> = [("class", input.class.is_some()), ("policy", input.policy.is_some()), ("visibility", input.visibility.is_some())]
            .into_iter()
            .filter_map(|(k, set)| set.then_some(k))
            .collect();
        if keys.is_empty() {
            return Ok(());
        }
        Err(ContextError::from(RunError::ModelInputRestricted(format!(
            "model `{model}` reads table `{}`, which declares {}; a model reads only tables declaring no class, policy or visibility",
            input.name,
            keys.join(", ")
        )))
        .into())
    }

    /// Hold the rows to declared nullability and the grain (`run.model.unique-key`).
    fn check_rows(spec: &ModelSpec, cols: &[Column], rows: &RecordBatch) -> std::result::Result<(), ReadFault> {
        for c in cols.iter().filter(|c| !c.nullable) {
            let nulls = rows.column_by_name(&c.name).map_or(0, |a| a.null_count());
            if nulls > 0 {
                return Err(mismatch(format!("model `{}`: column `{}` holds {nulls} nulls; its contract declares it non-null", spec.id, c.name)));
            }
        }
        let grain = spec.grain();
        if grain.is_empty() {
            return Ok(());
        }
        let mut keys: Vec<ArrayRef> = Vec::new();
        for k in grain {
            let a = rows.column_by_name(k).ok_or_else(|| mismatch(format!("model `{}`: grain column `{k}` is absent from its rows", spec.id)))?;
            if a.null_count() > 0 {
                return Err(mismatch(format!("model `{}`: grain column `{k}` holds {} nulls", spec.id, a.null_count())));
            }
            keys.push(a.clone());
        }
        let conv = RowConverter::new(keys.iter().map(|c| SortField::new(c.data_type().clone())).collect())
            .map_err(|e| invalid(format!("model `{}`: the grain: {e}", spec.id)))?;
        let converted = conv.convert_columns(&keys).map_err(|e| invalid(format!("model `{}`: the grain: {e}", spec.id)))?;
        let mut seen = std::collections::HashSet::new();
        let repeated = (0..rows.num_rows()).filter(|i| !seen.insert(converted.row(*i).as_ref().to_vec())).count();
        if repeated > 0 {
            return Err(mismatch(format!(
                "model `{}`: {repeated} rows repeat a value of the grain ({})",
                spec.id,
                grain.join(", ")
            )));
        }
        Ok(())
    }

    /// Admit a model's `sql` (`run.model.sql`); the model's own id never names an input.
    fn admit_sql(engine: &SqlEngine, spec: &ModelSpec, registered: impl Fn(&str) -> bool) -> std::result::Result<Admitted, ReadFault> {
        let sql = spec.sql.trim().trim_end_matches(';');
        Ok(admit(&engine.serialize(sql)?, |n| n != spec.id && registered(n))?)
    }

    /// Admit one `[[model.test]]` statement (`run.model.test-block`): the model's id names
    /// its staged rows.
    fn admit_test(engine: &SqlEngine, spec: &ModelSpec, sql: &str, registered: impl Fn(&str) -> bool) -> std::result::Result<Admitted, ReadFault> {
        Ok(admit(&engine.serialize(sql)?, |n| n == spec.id || registered(n))?)
    }

    /// The statement checks a build runs, over no store (`run.model.validate-statements`):
    /// the model's `sql`, every relation but the model's own id counting as registered, then
    /// each input `decl` declares against `run.model.restricted-input`, then each test's
    /// statement. A refusal names the statement it refuses — `model `<id>`` or
    /// `model `<id>` test `<name>`` — beside the fault.
    pub fn admit_statements(spec: &ModelSpec, decl: impl Fn(&str) -> Option<TableDecl>) -> std::result::Result<BTreeSet<String>, (String, ReadFault)> {
        let model = format!("model `{}`", spec.id);
        let engine = SqlEngine::raw().map_err(|e| (model.clone(), e))?;
        let admitted = admit_sql(&engine, spec, |_| true).map_err(|e| (model.clone(), e))?;
        for input in admitted.relations.iter().filter_map(|t| decl(t)) {
            check_unrestricted(&spec.id, &input).map_err(|e| (model.clone(), e))?;
        }
        let mut reads = admitted.relations;
        for t in &spec.tests {
            let checked = admit_test(&engine, spec, t.sql.trim().trim_end_matches(';'), |_| true)
                .map_err(|e| (format!("{model} test `{}`", t.name), e))?;
            reads.extend(checked.relations.into_iter().filter(|name| name != &spec.id));
        }
        Ok(reads)
    }

    /// Build one model (`run.model.build-verb`): admit its SQL over the store's tables,
    /// materialize its rows into staging, hold them to the contract, run its tests, then
    /// publish the snapshot and, for a published model, its manifest section through the
    /// pointer (`run.publish.staging`). A refusal at any step removes the staging directory
    /// and leaves the last published build serving.
    pub fn build(face: &Face, req: &BuildRequest<'_>) -> std::result::Result<Built, ReadFault> {
        build_guarded(face, req, &|| Ok(()))
    }

    /// Admit publication under the caller's execution boundary after staging, before
    /// changing persistent schema or the published pointer. This is not host fencing.
    pub fn build_guarded(face: &Face, req: &BuildRequest<'_>, boundary: &dyn Fn() -> Result<()>) -> std::result::Result<Built, ReadFault> {
        let store = face.store();
        let spec = req.model;
        store.check_writable("build")?;
        spec.validate().map_err(ContextError::from)?;
        let publishes = spec.publishes();
        let fingerprint = spec.schema_fingerprint();
        if let (true, Some(contract), Some(fp), Some(prior)) = (publishes, &spec.contract, &fingerprint, current_section(store, &spec.id)?) {
            let major = contract.version.major();
            if contextful_core::pipeline::model::major(&prior.contract_version) == Some(major) && &prior.schema_fingerprint != fp {
                return Err(mismatch(format!(
                    "model `{}`: the schema fingerprint moves from {} (build {}) to {fp} under major version {major}; bump the contract to {}.0.0",
                    spec.id,
                    prior.schema_fingerprint,
                    prior.build_id,
                    major + 1
                )));
            }
        }

        check_owned(store, &spec.id)?;
        // The model's table reads, and collects, under its own declaration.
        let own = face.decl(&spec.id);

        let engine = SqlEngine::raw_with_key(store.parquet_key())?;
        let tables = face.register_operator(&engine)?;
        let sql = spec.sql.trim().trim_end_matches(';');
        let admitted = admit_sql(&engine, spec, |n| tables.iter().any(|t| t == n))?;
        for t in &admitted.relations {
            check_unrestricted(&spec.id, &face.decl(t))?;
        }
        let mut watermark = Watermark::default();
        for t in &admitted.relations {
            let (f, at) = frontier(store, &face.decl(t))?;
            watermark.at = watermark.at.max(at);
            watermark.inputs.insert(t.clone(), f);
        }

        let (_, described) = engine.run_values(&format!("DESCRIBE {sql}"), &Bindings::default(), None)?;
        let described: Vec<(String, String)> =
            described.iter().map(|r| (text(&r[0]), text(&r[1]))).filter(|(n, _)| !is_injected(n)).collect();
        let mut names = BTreeSet::new();
        if let Some((dup, _)) = described.iter().find(|(n, _)| !names.insert(n.clone())) {
            return Err(invalid(format!("model `{}`: its rows carry column `{dup}` twice", spec.id)));
        }
        let cols = columns(spec, &described)?;
        if cols.is_empty() {
            return Err(invalid(format!("model `{}`: its rows carry no column outside the injected ones", spec.id)));
        }

        let (chain, _) = store.chain(&spec.id)?;
        let parent = chain.first().map(|s| s.snapshot_id.clone());
        let etag = store.pointer_etag(&spec.id)?;
        // The build commits like a landing: its table's commits serialize from the sequence
        // value to the pointer (`store.reserve.commit-order`).
        let _commit_lock = store.lock_commit(&spec.id)?;
        let commit_seq = store.assign_commit_seq(&spec.id)?;
        let last_attempt = attempts(store, &spec.id)?
            .into_iter()
            .filter_map(|attempt| SnapshotId::try_from(attempt.build_id).ok())
            .max();
        let previous = parent.as_ref().into_iter().chain(last_attempt.as_ref()).max();
        let (snapshot_id, staging, in_flight) = claim(store, &spec.id, SnapshotId::next(req.started_at, previous), req.started_at)?;
        let build_id = snapshot_id.to_string();
        if publishes {
            let attempt = BuildAttempt {
                build_id: build_id.clone(),
                started_at: req.started_at,
                contract_version: spec.contract.as_ref().expect("a published model declares a contract").version.as_str().to_string(),
                schema_fingerprint: fingerprint.clone().expect("a published model has a fingerprint"),
                disclosure_digest: disclosure_digest(&own),
                status: None,
                completed_at: None,
            };
            record_attempt(store, &spec.id, &attempt)?;
        }
        let staged = (|| -> std::result::Result<(SnapshotManifest, Schema, u64), ReadFault> {
            let raw = staging.join("__model.parquet");
            let select: Vec<String> = cols.iter().map(|c| ident(&c.name)).collect();
            let selected = format!("SELECT {} FROM ({sql}) AS __model", select.join(", "));
            let loose = Arc::new(ArrowSchema::new(
                cols.iter().map(|c| Field::new(&c.name, parquet_io::data_type(&c.ty), true)).collect::<Vec<_>>(),
            ));
            let source = if store.encrypted() {
                engine.batches(&selected)?
            } else {
                engine.execute(&format!("COPY ({selected}) TO {} (FORMAT parquet, COMPRESSION zstd)", literal(&raw.to_string_lossy())))?;
                let batches = store.read_parquet(&raw)?;
                fs::remove_file(&raw).at(&raw)?;
                batches
            };
            let batches: Vec<RecordBatch> = source.iter().map(|b| parquet_io::conform(b, &loose)).collect::<Result<_>>()?;
            let rows = arrow_select::concat::concat_batches(&loose, &batches).map_err(|e| invalid(format!("model `{}`: {e}", spec.id)))?;
            check_rows(spec, &cols, &rows)?;

            let n = rows.num_rows();
            let mut schema = Schema { columns: cols.clone() };
            schema.columns.extend(injected_columns());
            let mut arrays: Vec<ArrayRef> = rows.columns().to_vec();
            let nanos = i64::try_from(req.started_at.unix_nanos()).map_err(|e| invalid(format!("the build instant: {e}")))?;
            arrays.push(Arc::new(TimestampNanosecondArray::from(vec![nanos; n]).with_timezone("UTC")));
            arrays.push(Arc::new(StringArray::from(vec![build_id.as_str(); n])));
            arrays.push(Arc::new(Int64Array::from_iter_values(0..n as i64)));
            arrays.push(Arc::new(Int64Array::from(vec![commit_seq; n])));
            arrays.push(Arc::new(StringArray::from(vec![req.site_id; n])));
            let batch = RecordBatch::try_new(parquet_io::arrow_schema(&schema), arrays).map_err(|e| invalid(format!("model `{}`: {e}", spec.id)))?;
            let part = part_name(0);
            store.write_parquet(&staging.join(&part), &batch)?;

            // The tests read the staged rows under the model's id, beside its inputs.
            let encrypted = store.parquet_key().map(|_| format!(", encryption_config = {{footer_key: {}}}", literal(crate::encrypt::PARQUET_KEY_NAME))).unwrap_or_default();
            engine.register(&spec.id, &format!("SELECT * FROM read_parquet({}{encrypted})", literal(&staging.join(&part).to_string_lossy())))?;
            for t in &spec.tests {
                let test_sql = t.sql.trim().trim_end_matches(';');
                admit_test(&engine, spec, test_sql, |n| tables.iter().any(|x| x == n))?;
                let (_, counted) = engine.run_values(&format!("SELECT count(*) FROM ({test_sql}) AS __test"), &Bindings::default(), None)?;
                let failing = match counted.first().and_then(|r| r.first()) {
                    Some(Engine::BigInt(c)) => *c,
                    Some(Engine::HugeInt(c)) => i64::try_from(*c).unwrap_or(i64::MAX),
                    _ => 0,
                };
                if failing > 0 {
                    return Err(ContextError::from(RunError::ModelTestFailed(format!(
                        "model `{}` test `{}` returned {failing} rows; the build publishes nothing",
                        spec.id, t.name
                    )))
                    .into());
                }
            }

            let section = match (&spec.contract, &fingerprint) {
                (Some(contract), Some(fp)) if publishes => Some(PublishSection {
                    contract_version: contract.version.as_str().to_string(),
                    schema_fingerprint: fp.clone(),
                    build_id: build_id.clone(),
                    build_started_at: req.started_at,
                    last_built_at: req.completed_at,
                    watermark: watermark.clone(),
                    max_lag: spec.max_lag().map(str::to_string),
                    last_build_status: BuildStatus::Published,
                    withheld_cells: withholds_cells(&own),
                    disclosure_digest: disclosure_digest(&own),
                    partitions_failed: None,
                    semantics_version: Some(SEMANTICS_VERSION),
                    fingerprint_recipe: Some(FINGERPRINT_RECIPE.to_string()),
                }),
                _ => None,
            };
            let manifest = SnapshotManifest {
                format_version: Default::default(),
                snapshot_id: snapshot_id.clone(),
                parent: parent.clone(),
                ancestors: SnapshotManifest::ancestors_after(chain.first()),
                table: spec.id.clone(),
                created_at: req.started_at,
                includes_runs: Vec::new(),
                primary_key: spec.grain().to_vec(),
                order_by: None,
                row_count: n as u64,
                valid_time: None,
                parts: vec![PartEntry { name: part, key_version: store.sealing().key_version() }],
                indexes: Vec::new(),
                fence: None,
                commit_seq: Some(commit_seq),
                publish: section,
            };
            let bytes = serde_json::to_vec_pretty(&manifest).expect("a manifest serializes");
            store.metadata_files().replace(&staging.join(MANIFEST_FILE), &bytes)?;
            Ok((manifest, schema, n as u64))
        })();
        let (manifest, schema, rows) = match staged {
            Ok(v) => v,
            Err(e) => {
                drop(in_flight);
                let _ = fs::remove_dir_all(&staging);
                if publishes {
                    refuse_attempt(store, &spec.id, &build_id, req.completed_at)?;
                    write_logs(store, &spec.id)?;
                }
                return Err(e);
            }
        };

        // The schema a reader resolves the new snapshot through goes in place under the
        // schema lock, and returns to its prior value when the pointer is lost.
        let _schema_lock = store.lock_schema(&spec.id)?;
        if let Err(error) = boundary() {
            drop(in_flight);
            let _ = fs::remove_dir_all(&staging);
            if publishes {
                refuse_attempt(store, &spec.id, &build_id, req.completed_at)?;
                write_logs(store, &spec.id)?;
            }
            return Err(error.into());
        }
        let prior_schema = store.try_schema(&spec.id)?;
        store.write_schema(&spec.id, &schema)?;
        let section = manifest.publish.clone();
        let staged = Staged {
            table: spec.id.clone(),
            manifest,
            staging: staging.clone(),
            etag,
            runs: 0,
            retention: None,
            warnings: Vec::new(),
            _in_flight: Some(Arc::new(in_flight)),
        };
        let restore = |store: &Store| -> Result<()> {
            match &prior_schema {
                Some(s) => store.write_schema(&spec.id, s),
                None => {
                    let path = store.table_dir(&spec.id)?.join(contextful_core::store::lay_out::SCHEMA_FILE);
                    fs::remove_file(&path).at(&path)
                }
            }
        };
        match commit(store, staged) {
            Ok(Committed::Published(_)) => {}
            Ok(Committed::Lost) => {
                restore(store)?;
                let _ = fs::remove_dir_all(&staging);
                return Err(invalid(format!("model `{}`: the pointer moved during the build; nothing was published", spec.id)));
            }
            Err(e) => {
                restore(store)?;
                let _ = fs::remove_file(staging.join(STAGING_LOCK));
                let _ = fs::remove_dir_all(&staging);
                return Err(e.into());
            }
        }
        drop(_schema_lock);

        if publishes {
            write_logs(store, &spec.id)?;
        }
        crate::fold::collect(store, &own, req.completed_at)?;
        Ok(Built { model: spec.id.clone(), build_id, rows, watermark, section })
    }
}
