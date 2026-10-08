//! `store.declare`: a table's declaration block and the checks over it.

use super::reconcile::{ColumnType, Schema};
use super::reserve::{check_table_name, is_injected, INGESTED_AT};
use super::StoreError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

fn spelled_types<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<BTreeMap<String, String>>, D::Error> {
    let map = BTreeMap::<String, String>::deserialize(d)?;
    for (column, spelled) in &map {
        if ColumnType::parse(spelled).is_none() {
            return Err(serde::de::Error::custom(format!("column `{column}` declares type `{spelled}`, which no landing reads")));
        }
    }
    Ok(Some(map))
}

/// Default `retain_runs` window: 7 d (`store.fold.retention`).
pub const DEFAULT_RETAIN_RUNS_SECS: u64 = 7 * 24 * 60 * 60;

/// How a run's rows relate to the rows before it (`store.declare.write-mode`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WriteMode {
    /// Keep the last write per key and retire no key.
    #[default]
    Append,
    /// Each run carries the source's complete state.
    Replace,
}

/// A declared valid-time pair of the table's own columns (`store.bound-time.valid-time-declaration`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidTime {
    pub from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

/// The timestamp and age limiting rows of a table (`store.declare.retain-rows`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainRows {
    pub column: String,
    pub age: String,
}

/// One `[[pipeline.tables]]` block (`store.declare.table-block`). An unset key is absent
/// from the canonical serialization; an unknown key refuses to parse.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableDecl {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_key: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_mode: Option<WriteMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replicate: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub erasure_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referenced_by: Option<Vec<ErasureReference>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_erase: Option<ErasureSurvival>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_time: Option<ValidTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_by: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_by: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retain_runs: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retain_rows: Option<RetainRows>,
    /// A derive output table keeping each task version's rows current under that version
    /// (`run.emit.version-retained`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retain_versions: Option<bool>,
    /// Column types every landing reads (`store.declare.column-types`), spelled as
    /// [`ColumnType::parse`] reads them; an unreadable spelling refuses the block.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "spelled_types")]
    pub columns: Option<BTreeMap<String, String>>,
    /// Sidecar declarations (`store.index.declaration`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexes: Option<Vec<super::index::IndexDecl>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_hint: Option<String>,
    /// Hints for named columns in a table description (`read.register.describe-payload`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column_hints: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example_queries: Option<Vec<String>>,
    /// The column carrying each row's content hash, the row key a ranked read keeps one
    /// row per (`read.retrieve.row-key-dedup`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash_column: Option<String>,
    /// The time to live of a cached read result touching the table, which opts the table
    /// into the result cache (`read.cache.cache-is-opt-in`), spelled as `retain_runs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_cache: Option<String>,
    /// A table whose rows never enter the result cache (`read.cache.cache-is-opt-in`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private: Option<bool>,
}

/// A declared incoming reference to the table's erasure key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErasureReference {
    pub table: String,
    pub column: String,
}

/// A citing table retains its rows while its erased source becomes unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ErasureSurvival {
    Survive,
}

#[derive(Deserialize)]
struct PipelineFile {
    #[serde(default)]
    pipeline: PipelineTables,
}

#[derive(Default, Deserialize)]
struct PipelineTables {
    #[serde(default)]
    tables: Vec<TableDecl>,
}

/// A declaration that does not parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("declaration malformed: {0}")]
pub struct DeclarationMalformed(pub String);

/// The tables the declaration's scheduled folds cover (`store.declare.fold-job`): a
/// `[[job]]` block of kind `fold` carrying a `schedule`, not disabled, covers its `target`,
/// or every table when it names none. The job union's own checks belong to `surface.fire`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FoldCoverage {
    every: bool,
    tables: BTreeSet<String>,
}

impl FoldCoverage {
    /// Read the `[[job]]` blocks of one manifest file. A job key of the wrong type refuses.
    pub fn parse(toml_text: &str) -> Result<FoldCoverage, DeclarationMalformed> {
        let value: toml::Value = toml::from_str(toml_text).map_err(|e| DeclarationMalformed(e.to_string()))?;
        let mut coverage = FoldCoverage::default();
        let Some(jobs) = value.get("job") else { return Ok(coverage) };
        let jobs = jobs.as_array().ok_or_else(|| DeclarationMalformed("`job` is declared as `[[job]]`, an array of tables".into()))?;
        for (i, job) in jobs.iter().enumerate() {
            let key = |k: &str| job.get(k);
            let malformed = |k: &str, ty: &str| DeclarationMalformed(format!("job[{i}]: `{k}` is not {ty}"));
            let kind = key("kind").map(|v| v.as_str().ok_or_else(|| malformed("kind", "a string"))).transpose()?;
            let schedule = key("schedule").map(|v| v.as_str().ok_or_else(|| malformed("schedule", "a string"))).transpose()?;
            let enabled = key("enabled").map(|v| v.as_bool().ok_or_else(|| malformed("enabled", "a boolean"))).transpose()?;
            let target = key("target").map(|v| v.as_str().ok_or_else(|| malformed("target", "a string"))).transpose()?;
            if kind != Some("fold") || schedule.is_none() || enabled == Some(false) {
                continue;
            }
            match target {
                Some(t) => {
                    coverage.tables.insert(t.to_string());
                }
                None => coverage.every = true,
            }
        }
        Ok(coverage)
    }

    /// Add the coverage another manifest file declares.
    pub fn extend(&mut self, other: FoldCoverage) {
        self.every |= other.every;
        self.tables.extend(other.tables);
    }

    /// Whether a scheduled, enabled fold covers the table named by its destination name.
    pub fn covers(&self, table: &str) -> bool {
        self.every || self.tables.contains(table)
    }
}

/// The tables, each once its retention window and visibility block parse
/// (`disclosure.declare-fidelity.family-bound`).
fn checked(tables: Vec<TableDecl>) -> Result<Vec<TableDecl>, crate::disclosure::declare::DeclareError> {
    for t in &tables {
        t.retain_runs_secs()?;
        t.retain_rows_secs()?;
        t.result_cache_secs()?;
        crate::disclosure::declare::Binding::of(t)?;
    }
    Ok(tables)
}

impl TableDecl {
    /// A table with every key unset.
    pub fn named(name: impl Into<String>) -> TableDecl {
        TableDecl { name: name.into(), ..TableDecl::default() }
    }

    /// Every table block of a pipeline manifest: the `[[pipeline.tables]]` of a
    /// `[pipeline]` table as named, and those of each `[[pipeline]]` specification under
    /// its destination name `<pipeline id>_<table name>` (`run.declare.table-name`).
    ///
    /// A table's visibility block outside its family bound or budget grammar refuses the
    /// manifest with its typed refusal (`disclosure.declare-fidelity.family-bound`).
    pub fn parse_pipeline(toml_text: &str) -> Result<Vec<TableDecl>, crate::disclosure::declare::DeclareError> {
        let value: toml::Value = toml::from_str(toml_text).map_err(|e| DeclarationMalformed(e.to_string()))?;
        let tables = match value.get("pipeline") {
            Some(toml::Value::Array(specs)) => {
                let mut out = Vec::new();
                for spec in specs {
                    let id = spec.get("id").and_then(toml::Value::as_str).ok_or_else(|| DeclarationMalformed("a `[[pipeline]]` block carries no `id`".into()))?;
                    for entry in spec.get("tables").and_then(toml::Value::as_array).into_iter().flatten() {
                        let mut decl = match entry {
                            toml::Value::String(name) => TableDecl::named(name.clone()),
                            other => other.clone().try_into().map_err(|e: toml::de::Error| DeclarationMalformed(e.to_string()))?,
                        };
                        decl.name = crate::pipeline::declare::table_name(id, &decl.name);
                        out.push(decl);
                    }
                }
                out
            }
            _ => {
                let file: PipelineFile = value.try_into().map_err(|e: toml::de::Error| DeclarationMalformed(e.to_string()))?;
                file.pipeline.tables
            }
        };
        checked(tables)
    }

    /// Every table block of a declaration set (`store.declare.declaration-set`): the
    /// declaration's under [`TableDecl::parse_pipeline`], then, for each `pipelines/`
    /// file, the `[[pipeline.tables]]` of a `[pipeline]` table as named and those of each
    /// specification `run.declare.manifest-file` reads under its destination name.
    pub fn parse_declaration_set(
        declaration: &str,
        pipelines: &[crate::pipeline::declare::ManifestFile],
    ) -> Result<Vec<TableDecl>, crate::disclosure::declare::DeclareError> {
        let mut tables = TableDecl::parse_pipeline(declaration)?;
        let mut declared = Vec::new();
        for f in pipelines {
            if f.path.ends_with(".toml") {
                let value: toml::Value = toml::from_str(&f.text).map_err(|e| DeclarationMalformed(format!("{}: {e}", f.path)))?;
                if matches!(value.get("pipeline"), Some(toml::Value::Table(_))) {
                    tables.extend(TableDecl::parse_pipeline(&f.text)?);
                }
            }
            for d in crate::pipeline::declare::read_manifest(f).map_err(|e| DeclarationMalformed(e.to_string()))? {
                declared.extend(d.spec.tables.iter().map(|t| TableDecl { name: d.spec.table_name(t.name()), ..t.decl() }));
            }
        }
        tables.extend(checked(declared)?);
        Ok(tables)
    }

    /// The canonical serialization: JSON with every unset key absent.
    pub fn canonical(&self) -> String {
        serde_json::to_string(self).expect("a declaration serializes")
    }

    pub fn primary_key(&self) -> &[String] {
        self.primary_key.as_deref().unwrap_or(&[])
    }

    /// The declared content-hash column, if any.
    pub fn content_hash_column(&self) -> Option<&str> {
        self.content_hash_column.as_deref()
    }

    pub fn is_keyed(&self) -> bool {
        !self.primary_key().is_empty()
    }

    /// The column picking the surviving row per key (`store.declare.order-by-default`).
    pub fn order_by(&self) -> &str {
        self.order_by.as_deref().unwrap_or(INGESTED_AT)
    }

    pub fn write_mode(&self) -> WriteMode {
        self.write_mode.unwrap_or_default()
    }

    pub fn cluster_by(&self) -> &[String] {
        self.cluster_by.as_deref().unwrap_or(&[])
    }

    pub fn partition_by(&self) -> &[String] {
        self.partition_by.as_deref().unwrap_or(&[])
    }

    /// The declared column types (`store.declare.column-types`).
    pub fn column_types(&self) -> BTreeMap<String, ColumnType> {
        self.columns.iter().flatten().filter_map(|(c, t)| Some((c.clone(), ColumnType::parse(t)?))).collect()
    }

    /// The `retain_runs` window in seconds: `<n>d`, `<n>h`, `<n>m` or `<n>s`.
    pub fn retain_runs_secs(&self) -> Result<u64, DeclarationMalformed> {
        let Some(s) = &self.retain_runs else { return Ok(DEFAULT_RETAIN_RUNS_SECS) };
        crate::time::duration_secs(s)
            .ok_or_else(|| DeclarationMalformed(format!("table `{}`: retain_runs `{s}` is not <n>d, <n>h, <n>m or <n>s", self.name)))
    }

    /// The declared row-age duration in seconds, if the table retains by row age.
    pub fn retain_rows_secs(&self) -> Result<Option<u64>, DeclarationMalformed> {
        let Some(retention) = &self.retain_rows else { return Ok(None) };
        let age = &retention.age;
        if !age.ends_with('d') {
            return Err(DeclarationMalformed(format!("table `{}`: retain_rows age `{age}` is not <n>d", self.name)));
        }
        crate::time::duration_secs(age)
            .filter(|age| *age > 0)
            .map(Some)
            .ok_or_else(|| DeclarationMalformed(format!("table `{}`: retain_rows age `{age}` is not a positive <n>d", self.name)))
    }

    /// Refuse a struct, list or map column, as the schema holds it or `columns` declares
    /// it, in a key, ordering, clustering, partition or valid-time role
    /// (`store.declare.nested-key`).
    fn check_nested_roles(&self, schema: &Schema) -> Result<(), StoreError> {
        let declared = self.column_types();
        let mut roles: Vec<(&str, &str)> = self.primary_key().iter().map(|c| ("keys on", c.as_str())).collect();
        roles.push(("orders by", self.order_by()));
        roles.extend(self.cluster_by().iter().map(|c| ("clusters by", c.as_str())));
        roles.extend(self.partition_by().iter().map(|c| ("partitions by", c.as_str())));
        if let Some(vt) = &self.valid_time {
            roles.extend(std::iter::once(&vt.from).chain(vt.to.as_ref()).map(|c| ("bounds valid time by", c.as_str())));
        }
        for (role, column) in roles {
            let ty = schema.get(column).map(|c| &c.ty).or_else(|| declared.get(column));
            if let Some(ty) = ty.filter(|t| t.is_nested()) {
                return Err(StoreError::StoreNestedKeyColumn(format!(
                    "table `{}` {role} `{column}`, typed {}; a struct, list or map column takes no key or ordering role",
                    self.name,
                    ty.name()
                )));
            }
        }
        Ok(())
    }

    /// The declared result-cache time to live in seconds, or `None` where the table does
    /// not opt in (`read.cache.cache-is-opt-in`).
    pub fn result_cache_secs(&self) -> Result<Option<u64>, DeclarationMalformed> {
        let Some(s) = &self.result_cache else { return Ok(None) };
        crate::time::duration_secs(s)
            .map(Some)
            .ok_or_else(|| DeclarationMalformed(format!("table `{}`: result_cache `{s}` is not <n>d, <n>h, <n>m or <n>s", self.name)))
    }

    /// Whether the table is tagged `private = true`.
    pub fn is_private(&self) -> bool {
        self.private == Some(true)
    }

    /// Hold the declaration to the table's reconciled schema, before any Parquet lands.
    pub fn validate(&self, schema: &Schema) -> Result<(), StoreError> {
        check_table_name(&self.name)?;
        self.retain_rows_secs().map_err(|e| StoreError::StoreRetentionColumnInvalid(e.to_string()))?;
        if let Some(retention) = &self.retain_rows {
            let column = &retention.column;
            let ty = schema.get(column).map(|c| &c.ty);
            if ty != Some(&ColumnType::Timestamp) || (column != INGESTED_AT && self.column_types().get(column) != Some(&ColumnType::Timestamp)) {
                return Err(StoreError::StoreRetentionColumnInvalid(format!(
                    "table `{}` retains rows by `{column}`, which must be `_ingested_at` or a declared Timestamp column",
                    self.name
                )));
            }
        }
        self.check_nested_roles(schema)?;
        let order_by = self.order_by();
        if !is_injected(order_by) && schema.get(order_by).is_none() {
            return Err(StoreError::StoreOrderByUnknownColumn(format!(
                "table `{}` orders by `{order_by}`, which is neither a column of the table nor an injected column",
                self.name
            )));
        }
        if let Some(vt) = &self.valid_time {
            for col in std::iter::once(&vt.from).chain(vt.to.as_ref()) {
                match schema.get(col) {
                    Some(c) if c.ty == ColumnType::Timestamp => {}
                    Some(c) => {
                        return Err(StoreError::StoreValidTimeNotTimestamp(format!(
                            "table `{}` declares valid-time column `{col}`, typed {}",
                            self.name,
                            c.ty.name()
                        )))
                    }
                    None => {
                        return Err(StoreError::StoreValidTimeNotTimestamp(format!(
                            "table `{}` declares valid-time column `{col}`, which the table does not carry",
                            self.name
                        )))
                    }
                }
            }
        }
        for p in self.partition_by() {
            if let Some(c) = schema.get(p).filter(|c| c.ty.is_binary() || c.ty.is_vector()) {
                return Err(StoreError::StorePartitionColumnType(format!(
                    "table `{}` partitions by `{p}`, typed {}; bytes name no directory",
                    self.name,
                    c.ty.name()
                )));
            }
        }
        for k in self.primary_key() {
            if !is_injected(k) && schema.get(k).is_none() {
                return Err(StoreError::StoreKeyUnknownColumn(format!(
                    "table `{}` keys on `{k}`, which is neither a column of the table nor an injected column",
                    self.name
                )));
            }
            if schema.get(k).is_some_and(|c| c.ty == ColumnType::Float64) {
                return Err(StoreError::StoreKeyWidened(format!(
                    "table `{}`: primary-key column `{k}` is reconciled to Float64",
                    self.name
                )));
            }
        }
        Ok(())
    }
}
