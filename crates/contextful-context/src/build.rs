//! `run.model` and `run.publish` over the store: a model build materialized into
//! staging and published by the table pointer with its manifest section, holds on
//! published builds, and the history logs derived from committed manifests.

use crate::error::{ContextError, IoPath, Result};
use crate::store::{replace_file, sorted_dirs, FileLock, Store, LOCK_WAIT_SECS};
use contextful_core::pipeline::model::{
    build_entries, contract_history, regenerate, BuildEntry, ContractHistoryEntry, HoldRecord, PublishSection, Receipt,
    BUILDS_LOG, CONTRACT_HISTORY_LOG, HOLDS_DIR, HOLDS_LOG,
};
use contextful_core::run::RunError;
use contextful_core::store::lay_out::{SnapshotManifest, MANIFEST_FILE, STAGING_SUFFIX};
use contextful_core::time::Instant;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(feature = "read")]
pub use materialize::{build, BuildRequest, Built};

/// The section of the snapshot the model's pointer names, when that build published one.
pub fn current_section(store: &Store, model: &str) -> Result<Option<PublishSection>> {
    let (chain, _) = store.chain(model)?;
    Ok(chain.into_iter().next().and_then(|m| m.publish))
}

/// Every snapshot manifest on disk for the model, staging excluded, oldest first: the
/// pointer's chain and any snapshot a hold kept beyond it.
pub fn manifests(store: &Store, model: &str) -> Result<Vec<SnapshotManifest>> {
    let dir = store.table_dir(model)?.join("data").join("snapshots");
    let mut out = Vec::new();
    for d in sorted_dirs(&dir)? {
        if d.file_name().is_some_and(|n| n.to_string_lossy().ends_with(STAGING_SUFFIX)) {
            continue;
        }
        let path = d.join(MANIFEST_FILE);
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(ContextError::Io { path, source: e }),
        };
        let m: SnapshotManifest = serde_json::from_str(&text).map_err(|e| {
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
        let text = fs::read_to_string(&path).at(&path)?;
        let record: HoldRecord = serde_json::from_str(&text).map_err(|e| {
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
    let prior: Option<HoldRecord> = match fs::read_to_string(&path) {
        Ok(t) => serde_json::from_str(&t).ok(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(ContextError::Io { path, source: e }),
    };
    let receipt = if prior.is_some_and(|p| p.active(now)) { Receipt::Renewed } else { Receipt::Held };
    let record = HoldRecord { build_id: build_id.to_string(), principal: principal.to_string(), placed_at: now, expires_at: now.plus_secs(secs) };
    replace_file(&path, serde_json::to_string_pretty(&record).expect("a hold serializes").as_bytes())?;
    write_logs(store, model)?;
    Ok((receipt, record))
}

fn read_log<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    match fs::read_to_string(path) {
        // A line no entry type reads disagrees with every manifest, so regeneration drops it.
        Ok(text) => Ok(text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(ContextError::Io { path: path.to_path_buf(), source: e }),
    }
}

fn write_log<T: Serialize>(path: &Path, entries: &[T]) -> Result<()> {
    let mut text = String::new();
    for e in entries {
        text.push_str(&serde_json::to_string(e).expect("a log entry serializes"));
        text.push('\n');
    }
    replace_file(path, text.as_bytes())
}

/// Regenerate `builds.jsonl`, `holds.jsonl` and `contract-history.jsonl` in the model's
/// table directory from its committed snapshot and hold manifests
/// (`run.publish.history-logs`, `run.model.log-regeneration`).
pub fn write_logs(store: &Store, model: &str) -> Result<()> {
    let dir = store.table_dir(model)?;
    fs::create_dir_all(&dir).at(&dir)?;
    let committed = manifests(store, model)?;

    let path = dir.join(BUILDS_LOG);
    let builds = regenerate(&read_log::<BuildEntry>(&path)?, &build_entries(&committed), |e| e.build_id.clone(), |e| e.completed_at);
    write_log(&path, &builds)?;

    let path = dir.join(CONTRACT_HISTORY_LOG);
    let history =
        regenerate(&read_log::<ContractHistoryEntry>(&path)?, &contract_history(&committed), |e| e.build_id.clone(), |e| e.built_at);
    write_log(&path, &history)?;

    let path = dir.join(HOLDS_LOG);
    let key = |h: &HoldRecord| format!("{}@{}", h.build_id, h.placed_at.to_rfc3339_nanos());
    let placed = regenerate(&read_log::<HoldRecord>(&path)?, &holds(store, model)?, key, |h| h.placed_at);
    write_log(&path, &placed)
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
        BuildStatus, InputFrontier, ModelSpec, Watermark, FINGERPRINT_RECIPE, SEMANTICS_VERSION,
    };
    use contextful_core::read::guard::admit;
    use contextful_core::read::template::Bindings;
    use contextful_core::store::declare::TableDecl;
    use contextful_core::store::lay_out::{part_name, PartEntry, SnapshotId};
    use contextful_core::store::reconcile::{Column, ColumnType, FloatItem, Schema};
    use contextful_core::store::relation::{ident, literal};
    use contextful_core::store::reserve::{is_injected, INGESTED_AT, ROW_SEQ, RUN_ID, SITE_ID};
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
    fn satisfies(engine: &str, declared: ColumnType) -> bool {
        match (engine_type(engine), declared) {
            (Some(t), d) if t == d => true,
            (Some(ColumnType::Binary), ColumnType::FixedSizeBinary(_)) => true,
            (Some(ColumnType::FixedSizeList(FloatItem::Float32, n)), ColumnType::FixedSizeList(FloatItem::Float16, m)) => n == m,
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
                Some((_, ty)) if !satisfies(ty, declared) => {
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

    /// Build one model (`run.model.build-verb`): admit its SQL over the store's tables,
    /// materialize its rows into staging, hold them to the contract, run its tests, then
    /// publish the snapshot and, for a published model, its manifest section through the
    /// pointer (`run.publish.staging`). A refusal at any step removes the staging directory
    /// and leaves the last published build serving.
    pub fn build(face: &Face, req: &BuildRequest<'_>) -> std::result::Result<Built, ReadFault> {
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

        let engine = SqlEngine::raw()?;
        let tables = face.register_operator(&engine)?;
        let sql = spec.sql.trim().trim_end_matches(';');
        let admitted = admit(&engine.serialize(sql)?, |n| n != spec.id && tables.iter().any(|t| t == n))?;
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
        let (snapshot_id, staging, in_flight) = claim(store, &spec.id, SnapshotId::next(req.started_at, parent.as_ref()), req.started_at)?;
        let build_id = snapshot_id.to_string();
        let staged = (|| -> std::result::Result<(SnapshotManifest, Schema, u64), ReadFault> {
            let raw = staging.join("__model.parquet");
            let select: Vec<String> = cols.iter().map(|c| ident(&c.name)).collect();
            engine.execute(&format!(
                "COPY (SELECT {} FROM ({sql}) AS __model) TO {} (FORMAT parquet, COMPRESSION zstd)",
                select.join(", "),
                literal(&raw.to_string_lossy())
            ))?;
            let loose = Arc::new(ArrowSchema::new(
                cols.iter().map(|c| Field::new(&c.name, parquet_io::data_type(c.ty), true)).collect::<Vec<_>>(),
            ));
            let batches: Vec<RecordBatch> =
                parquet_io::read(&raw)?.iter().map(|b| parquet_io::conform(b, &loose)).collect::<Result<_>>()?;
            fs::remove_file(&raw).at(&raw)?;
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
            arrays.push(Arc::new(StringArray::from(vec![req.site_id; n])));
            let batch = RecordBatch::try_new(parquet_io::arrow_schema(&schema), arrays).map_err(|e| invalid(format!("model `{}`: {e}", spec.id)))?;
            let part = part_name(0);
            parquet_io::write(&staging.join(&part), &batch)?;

            // The tests read the staged rows under the model's id, beside its inputs.
            engine.register(&spec.id, &format!("SELECT * FROM read_parquet({})", literal(&staging.join(&part).to_string_lossy())))?;
            for t in &spec.tests {
                let test_sql = t.sql.trim().trim_end_matches(';');
                admit(&engine.serialize(test_sql)?, |n| n == spec.id || tables.iter().any(|x| x == n))?;
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
                    withheld_cells: false,
                    disclosure_digest: spec.disclosure_digest(),
                    partitions_failed: None,
                    semantics_version: Some(SEMANTICS_VERSION),
                    fingerprint_recipe: Some(FINGERPRINT_RECIPE.to_string()),
                }),
                _ => None,
            };
            let manifest = SnapshotManifest {
                snapshot_id: snapshot_id.clone(),
                parent: parent.clone(),
                table: spec.id.clone(),
                created_at: req.started_at,
                includes_runs: Vec::new(),
                primary_key: spec.grain().to_vec(),
                order_by: None,
                row_count: n as u64,
                valid_time: None,
                parts: vec![PartEntry { name: part, key_version: 0 }],
                indexes: Vec::new(),
                fence: None,
                commit_seq: None,
                publish: section,
            };
            let bytes = serde_json::to_vec_pretty(&manifest).expect("a manifest serializes");
            fs::write(staging.join(MANIFEST_FILE), bytes).at(staging.join(MANIFEST_FILE))?;
            Ok((manifest, schema, n as u64))
        })();
        let (manifest, schema, rows) = match staged {
            Ok(v) => v,
            Err(e) => {
                drop(in_flight);
                let _ = fs::remove_dir_all(&staging);
                return Err(e);
            }
        };

        // The schema a reader resolves the new snapshot through goes in place under the
        // schema lock, and returns to its prior value when the pointer is lost.
        let _schema_lock = store.lock_schema(&spec.id)?;
        let prior_schema = store.try_schema(&spec.id)?;
        store.write_schema(&spec.id, &schema)?;
        let section = manifest.publish.clone();
        let staged = Staged {
            table: spec.id.clone(),
            manifest,
            staging: staging.clone(),
            etag,
            runs: 0,
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

        let decl = TableDecl::named(spec.id.clone());
        crate::fold::collect(store, &decl, req.completed_at)?;
        if publishes {
            write_logs(store, &spec.id)?;
        }
        Ok(Built { model: spec.id.clone(), build_id, rows, watermark, section })
    }
}
