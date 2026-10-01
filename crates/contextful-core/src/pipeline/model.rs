//! `run.model` and `run.publish`: the `[[model]]` block, the contract identity a build
//! publishes, the manifest section it commits, and the history logs derived from
//! committed manifests.

use super::canonical::canonical_json;
use super::declare::{Declared, ManifestFile};
use crate::run::journal::sha256_hex;
use crate::run::RunError;
use crate::store::declare::TableDecl;
use crate::store::lay_out::SnapshotManifest;
use crate::store::reconcile::ColumnType;
use crate::store::reserve::{check_table_name, ALWAYS_INJECTED};
use crate::time::Instant;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// The engine's injected-column semantics a published build carries; it advances when
/// the engine adds an injected column (`run.publish.semantics-version`).
pub const SEMANTICS_VERSION: u32 = 2;

/// The inputs of a schema fingerprint (`run.publish.semantics-version`).
pub const FINGERPRINT_RECIPE: &str =
    "sha256(jcs({columns: [{name, type, nullable}], grain, injected: [_ingested_at, _run_id, _row_seq, _commit_seq, _site_id]}))";

/// The history logs a published model's table directory holds (`run.publish.history-logs`).
pub const BUILDS_LOG: &str = "builds.jsonl";
pub const HOLDS_LOG: &str = "holds.jsonl";
pub const CONTRACT_HISTORY_LOG: &str = "contract-history.jsonl";

/// The directory of hold manifests inside a model's table directory (`run.model.hold-manifest`).
pub const HOLDS_DIR: &str = "holds";

/// How a model materializes (`run.model.materialized`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Materialized {
    #[default]
    Table,
}

/// A `<major>.<minor>.<patch>` contract version (`run.model.contract-block`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ContractVersion(String);

impl ContractVersion {
    pub fn parse(s: &str) -> Option<ContractVersion> {
        let parts: Vec<&str> = s.split('.').collect();
        let numeric = |p: &&str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && (p.len() == 1 || !p.starts_with('0'));
        (parts.len() == 3 && parts.iter().all(numeric)).then(|| ContractVersion(s.to_string()))
    }

    pub fn major(&self) -> u64 {
        major(&self.0).unwrap_or(0)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The major component of a version string.
pub fn major(version: &str) -> Option<u64> {
    version.split('.').next()?.parse().ok()
}

impl<'de> Deserialize<'de> for ContractVersion {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        ContractVersion::parse(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("version `{s}` is not `<major>.<minor>.<patch>`")))
    }
}

/// A positive span in the [`crate::time::duration_secs`] grammar, as `max_lag` and a
/// hold's `--for` read it; zero reads as `None`.
pub fn duration_secs(s: &str) -> Option<u64> {
    crate::time::duration_secs(s).filter(|n| *n > 0)
}

fn spelled_type<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let s = String::deserialize(d)?;
    match ColumnType::parse(&s) {
        Some(t) => Ok(t.spell()),
        None => Err(serde::de::Error::custom(format!("type `{s}` is not one a landing reads"))),
    }
}

fn spelled_lag<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let s = String::deserialize(d)?;
    duration_secs(&s).map(|_| s.clone()).ok_or_else(|| serde::de::Error::custom(format!("max_lag `{s}` is not <n>s, <n>m, <n>h or <n>d")))
}

/// One contract column (`run.model.contract-block`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractColumn {
    pub name: String,
    #[serde(rename = "type", deserialize_with = "spelled_type")]
    pub ty: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nullable: Option<bool>,
}

impl ContractColumn {
    pub fn column_type(&self) -> ColumnType {
        ColumnType::parse(&self.ty).expect("a contract type is checked when it parses")
    }

    pub fn nullable(&self) -> bool {
        self.nullable.unwrap_or(true)
    }
}

/// `[model.contract]` (`run.model.contract-block`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractDecl {
    pub version: ContractVersion,
    pub columns: Vec<ContractColumn>,
}

/// `[model.freshness]` (`run.model.freshness-block`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FreshnessDecl {
    #[serde(deserialize_with = "spelled_lag")]
    pub max_lag: String,
}

/// One `[[model.test]]` (`run.model.test-block`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelTest {
    pub name: String,
    pub sql: String,
}

/// One `[[model]]` block (`run.model.model-block`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSpec {
    pub id: String,
    pub sql: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialized: Option<Materialized>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unique_key: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<ContractDecl>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<FreshnessDecl>,
    #[serde(default, rename = "test", skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<ModelTest>,
}

impl ModelSpec {
    pub fn publishes(&self) -> bool {
        self.publish.unwrap_or(true)
    }

    pub fn grain(&self) -> &[String] {
        self.unique_key.as_deref().unwrap_or(&[])
    }

    pub fn max_lag(&self) -> Option<&str> {
        self.freshness.as_ref().map(|f| f.max_lag.as_str())
    }

    /// Hold the block to the rules checked before any row is read.
    pub fn validate(&self) -> Result<(), RunError> {
        check_table_name(&self.id)?;
        if self.publishes() && self.contract.is_none() {
            return Err(RunError::ModelContractUndeclared(format!(
                "model `{}` publishes and declares no `[model.contract]`; declare one, or `publish = false`",
                self.id
            )));
        }
        if let Some(c) = &self.contract {
            let mut seen = std::collections::BTreeSet::new();
            if let Some(dup) = c.columns.iter().find(|col| !seen.insert(col.name.as_str())) {
                return Err(RunError::PipelineSpecInvalid(format!("model `{}`: contract column `{}` is declared twice", self.id, dup.name)));
            }
            if let Some(k) = self.grain().iter().find(|k| !seen.contains(k.as_str())) {
                return Err(RunError::PipelineSpecInvalid(format!("model `{}`: unique_key column `{k}` is not a contract column", self.id)));
            }
        }
        let mut names = std::collections::BTreeSet::new();
        if let Some(t) = self.tests.iter().find(|t| !names.insert(t.name.as_str())) {
            return Err(RunError::PipelineSpecInvalid(format!("model `{}`: test `{}` is declared twice", self.id, t.name)));
        }
        Ok(())
    }

    /// The schema fingerprint over the declared columns, their types, nullability and the
    /// grain, with the injected columns the recipe names (`run.publish.contract-identity`).
    pub fn schema_fingerprint(&self) -> Option<String> {
        let c = self.contract.as_ref()?;
        let columns: Vec<Value> =
            c.columns.iter().map(|col| json!({"name": col.name, "type": col.column_type().spell(), "nullable": col.nullable()})).collect();
        let v = json!({"columns": columns, "grain": self.grain(), "injected": ALWAYS_INJECTED});
        Some(sha256_hex(canonical_json(&v).as_bytes()))
    }
}

/// The disclosure keys of a table declaration a model's table reads under.
fn disclosure_policy(decl: &TableDecl) -> Value {
    let mut out = serde_json::Map::new();
    for (key, v) in [("class", &decl.class), ("policy", &decl.policy), ("visibility", &decl.visibility)] {
        if let Some(v) = v {
            out.insert(key.to_string(), sort_sets(v.clone()));
        }
    }
    Value::Object(out)
}

/// Sort every array of scalars, the set-valued fields; an array holding an object keeps
/// its order, since row exceptions apply first match.
fn sort_sets(v: Value) -> Value {
    match v {
        Value::Array(items) if items.iter().all(|i| !i.is_object() && !i.is_array()) => {
            let mut keyed: Vec<(String, Value)> = items.into_iter().map(|i| (canonical_json(&i), i)).collect();
            keyed.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Array(keyed.into_iter().map(|(_, i)| i).collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sort_sets).collect()),
        Value::Object(map) => Value::Object(map.into_iter().map(|(k, v)| (k, sort_sets(v))).collect()),
        other => other,
    }
}

/// The digest over the `class`, `policy` and `visibility` a model's table declares,
/// set-valued fields sorted and absent keys omitted (`run.publish.disclosure-digest`).
pub fn disclosure_digest(decl: &TableDecl) -> String {
    sha256_hex(canonical_json(&disclosure_policy(decl)).as_bytes())
}

/// Whether a model's table declares any disclosure key, so that some reader is served
/// fewer cells than the build wrote (`run.publish.disclosure-digest`).
pub fn withholds_cells(decl: &TableDecl) -> bool {
    decl.class.is_some() || decl.policy.is_some() || decl.visibility.is_some()
}

/// A model and where it was declared.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclaredModel {
    pub spec: ModelSpec,
    pub file: String,
    pub line: usize,
}

#[derive(Deserialize)]
struct ModelBlocks {
    #[serde(default)]
    model: Option<toml::Value>,
}

/// Every `[[model]]` block across the manifest files, in order. An id declared twice,
/// or equal to a pipeline's destination table, refuses (`run.model.model-id`).
pub fn collect_models(files: &[ManifestFile], pipelines: &[Declared]) -> Result<Vec<DeclaredModel>, RunError> {
    let mut all: Vec<DeclaredModel> = Vec::new();
    for f in files.iter().filter(|f| !f.path.ends_with(".json")) {
        let value: toml::Value = toml::from_str(&f.text).map_err(|e| invalid(&f.path, "", e.message()))?;
        let blocks: ModelBlocks = value.try_into().map_err(|e: toml::de::Error| invalid(&f.path, "", e.message()))?;
        let items = match blocks.model {
            None => continue,
            Some(toml::Value::Array(items)) => items,
            Some(_) => return Err(invalid(&f.path, "model", "a model is declared as `[[model]]`, an array of tables")),
        };
        let lines: Vec<usize> = f.text.lines().enumerate().filter(|(_, l)| l.trim() == "[[model]]").map(|(i, _)| i + 1).collect();
        for (i, item) in items.into_iter().enumerate() {
            let spec: ModelSpec = serde_path_to_error::deserialize(item).map_err(|e| {
                let path = e.path().to_string();
                invalid(&f.path, &format!("model[{i}]{}", if path == "." { String::new() } else { format!(".{path}") }), e.into_inner())
            })?;
            let d = DeclaredModel { spec, file: f.path.clone(), line: lines.get(i).copied().unwrap_or(1) };
            let here = format!("model `{}` ({}:{})", d.spec.id, d.file, d.line);
            if let Some(first) = all.iter().find(|m| m.spec.id == d.spec.id) {
                return Err(RunError::PipelineTableNameCollision(format!(
                    "{here} and model `{}` ({}:{}) both build `{}`",
                    first.spec.id, first.file, first.line, d.spec.id
                )));
            }
            for p in pipelines {
                if let Some(t) = p.spec.tables.iter().find(|t| p.spec.table_name(t.name()) == d.spec.id) {
                    return Err(RunError::PipelineTableNameCollision(format!(
                        "{here} and pipeline `{}` table `{}` ({}:{}) both write `{}`",
                        p.spec.id,
                        t.name(),
                        p.file,
                        p.line,
                        d.spec.id
                    )));
                }
            }
            all.push(d);
        }
    }
    Ok(all)
}

fn invalid(file: &str, path: &str, e: impl std::fmt::Display) -> RunError {
    let path = if path.is_empty() { "<root>".to_string() } else { path.to_string() };
    RunError::PipelineSpecInvalid(format!("{file}: at `{path}`: {}", e.to_string().trim()))
}

/// A build's status (`run.publish.build-entry`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildStatus {
    Published,
    Refused,
    Partial,
}

/// One input table's frontier: its current snapshot and the committed runs that snapshot
/// omits (`run.model.watermark`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputFrontier {
    pub snapshot_id: Option<String>,
    pub runs: Vec<String>,
}

/// A build's watermark: the frontier of each input and the newest commit instant among
/// them (`run.model.watermark`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watermark {
    pub at: Option<Instant>,
    pub inputs: BTreeMap<String, InputFrontier>,
}

/// The section a published build commits in its snapshot manifest
/// (`run.publish.manifest-section`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublishSection {
    pub contract_version: String,
    pub schema_fingerprint: String,
    pub build_id: String,
    pub build_started_at: Instant,
    pub last_built_at: Instant,
    pub watermark: Watermark,
    pub max_lag: Option<String>,
    pub last_build_status: BuildStatus,
    pub withheld_cells: bool,
    pub disclosure_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partitions_failed: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantics_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint_recipe: Option<String>,
}

impl PublishSection {
    /// The table's freshness, read off the section (`run.publish.freshness`).
    pub fn freshness(&self) -> Freshness {
        Freshness {
            build_id: self.build_id.clone(),
            watermark: self.watermark.clone(),
            max_lag: self.max_lag.clone(),
            last_build_status: self.last_build_status,
            withheld_cells: self.withheld_cells,
        }
    }
}

/// A published table's freshness. Staleness is computed from it and never stored
/// (`run.publish.freshness`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Freshness {
    pub build_id: String,
    pub watermark: Watermark,
    pub max_lag: Option<String>,
    pub last_build_status: BuildStatus,
    pub withheld_cells: bool,
}

impl Freshness {
    /// Whether the watermark trails `now` by more than `max_lag`. A table declaring no
    /// `max_lag` is never stale; one whose inputs hold no commit is stale under any lag.
    pub fn stale(&self, now: Instant) -> bool {
        let Some(lag) = self.max_lag.as_deref().and_then(duration_secs) else { return false };
        match self.watermark.at {
            Some(at) => at.plus_secs(lag) < now,
            None => true,
        }
    }
}

/// One `builds.jsonl` entry (`run.publish.build-entry`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuildEntry {
    pub build_id: String,
    pub started_at: Instant,
    pub completed_at: Instant,
    pub status: BuildStatus,
    pub contract_version: String,
    pub schema_fingerprint: String,
    pub partitions_unfilled: Vec<String>,
    pub disclosure_digest: String,
}

/// One `contract-history.jsonl` entry: a build that published a contract identity its
/// predecessor did not (`run.publish.history-logs`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractHistoryEntry {
    pub contract_version: String,
    pub schema_fingerprint: String,
    pub build_id: String,
    pub built_at: Instant,
}

/// A hold on one build (`run.publish.hold`), the body of its hold manifest and of each
/// `holds.jsonl` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoldRecord {
    pub build_id: String,
    pub principal: String,
    pub placed_at: Instant,
    pub expires_at: Instant,
}

impl HoldRecord {
    pub fn active(&self, now: Instant) -> bool {
        self.expires_at > now
    }
}

/// What a hold did (`run.model.hold-verb`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Receipt {
    Held,
    Renewed,
}

/// The build entry a committed manifest's section records.
pub fn build_entry(section: &PublishSection) -> BuildEntry {
    BuildEntry {
        build_id: section.build_id.clone(),
        started_at: section.build_started_at,
        completed_at: section.last_built_at,
        status: section.last_build_status,
        contract_version: section.contract_version.clone(),
        schema_fingerprint: section.schema_fingerprint.clone(),
        partitions_unfilled: section.partitions_failed.clone().unwrap_or_default(),
        disclosure_digest: section.disclosure_digest.clone(),
    }
}

/// The build entries the committed manifests record, oldest first.
pub fn build_entries(manifests: &[SnapshotManifest]) -> Vec<BuildEntry> {
    let mut out: Vec<BuildEntry> = manifests.iter().filter_map(|m| m.publish.as_ref()).map(build_entry).collect();
    out.sort_by(|a, b| (a.completed_at, &a.build_id).cmp(&(b.completed_at, &b.build_id)));
    out
}

/// The contract-history entries the committed manifests record, oldest first: a first
/// build, and a build whose identity differs from its parent's. A build whose parent
/// retention collected is left to the existing log.
pub fn contract_history(manifests: &[SnapshotManifest]) -> Vec<ContractHistoryEntry> {
    let mut out = Vec::new();
    for m in manifests {
        let Some(s) = &m.publish else { continue };
        let changed = match &m.parent {
            None => true,
            Some(p) => match manifests.iter().find(|x| &x.snapshot_id == p) {
                Some(prev) => prev.publish.as_ref().is_none_or(|q| q.contract_version != s.contract_version || q.schema_fingerprint != s.schema_fingerprint),
                None => false,
            },
        };
        if changed {
            out.push(ContractHistoryEntry {
                contract_version: s.contract_version.clone(),
                schema_fingerprint: s.schema_fingerprint.clone(),
                build_id: s.build_id.clone(),
                built_at: s.last_built_at,
            });
        }
    }
    out.sort_by(|a, b| (a.built_at, &a.build_id).cmp(&(b.built_at, &b.build_id)));
    out
}

/// Regenerate an append-only log from what committed manifests record: an existing entry
/// sharing a key with a derived one is replaced by it, an entry no manifest still records
/// is kept, and the result is ordered by `order` then key (`run.model.log-regeneration`).
pub fn regenerate<T: Clone>(existing: &[T], derived: &[T], key: impl Fn(&T) -> String, order: impl Fn(&T) -> Instant) -> Vec<T> {
    let derived_keys: std::collections::BTreeSet<String> = derived.iter().map(&key).collect();
    let mut out: Vec<T> = existing.iter().filter(|e| !derived_keys.contains(&key(e))).cloned().collect();
    let mut seen = std::collections::BTreeSet::new();
    out.retain(|e| seen.insert(key(e)));
    out.extend(derived.iter().cloned());
    out.sort_by(|a, b| order(a).cmp(&order(b)).then_with(|| key(a).cmp(&key(b))));
    out
}

/// The top-level keys a manifest file holds (`run.model.top-level-block`).
pub const MANIFEST_BLOCKS: [&str; 21] = [
    "acl_sweep",
    "authoring_posture",
    "capabilities",
    "connector",
    "control",
    "derive",
    "job",
    "limiters",
    "media_root",
    "model",
    "pack",
    "pipeline",
    "principal",
    "project",
    "query_templates",
    "relation",
    "role_grants",
    "site_id",
    "site_id_env",
    "subject_map",
    "table",
];

/// Refuse a manifest's top-level key outside [`MANIFEST_BLOCKS`] (`run.model.top-level-block`).
pub fn check_blocks(file: &str, value: &toml::Value) -> Result<(), RunError> {
    let Some(table) = value.as_table() else { return Ok(()) };
    match table.keys().find(|k| !MANIFEST_BLOCKS.contains(&k.as_str())) {
        Some(k) => Err(RunError::PipelineUnknownBlock(format!(
            "{file}: top-level key `{k}` is no manifest block; the accepted blocks are {}",
            MANIFEST_BLOCKS.join(", ")
        ))),
        None => Ok(()),
    }
}
