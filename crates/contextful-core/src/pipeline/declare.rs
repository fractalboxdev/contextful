//! `run.declare`: the pipeline specification, its serializations, manifest discovery,
//! the content hash and the destination table name.

use super::canonical::canonical_json;
use super::transform::TransformOp;
use crate::run::journal::sha256_hex;
use crate::run::RunError;
use crate::store::declare::{TableDecl, WriteMode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A connector name beside its free-form configuration object (`run.declare.source-block`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceBlock {
    pub name: String,
    #[serde(default = "empty_object", skip_serializing_if = "is_empty_object")]
    pub config: Value,
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}

fn is_empty_object(v: &Value) -> bool {
    v.as_object().is_some_and(|m| m.is_empty())
}

/// The destination every pipeline lands in unless it names another: the local store.
pub const STORE_DESTINATION: &str = "store";

/// A `tables` entry: a bare name or a table block (`run.declare.table-entry`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TableEntry {
    Name(String),
    Block(Box<TableDecl>),
}

impl TableEntry {
    /// The entry as a table block, a bare name carrying every key unset.
    pub fn decl(&self) -> TableDecl {
        match self {
            TableEntry::Name(n) => TableDecl::named(n.clone()),
            TableEntry::Block(d) => (**d).clone(),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            TableEntry::Name(n) => n,
            TableEntry::Block(d) => &d.name,
        }
    }
}

/// How a fire answers one failing table (`run.declare.table-error`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnTableError {
    #[default]
    Abort,
    Continue,
}

/// One pipeline specification (`run.declare.pipeline-spec`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineSpec {
    pub id: String,
    pub source: SourceBlock,
    pub tables: Vec<TableEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<SourceBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// The stream clock field (`run.declare.incremental-field`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incremental: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transforms: Vec<TransformOp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redaction: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalize: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backfill: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queries: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_table_error: Option<OnTableError>,
}

impl PipelineSpec {
    pub fn on_table_error(&self) -> OnTableError {
        self.on_table_error.unwrap_or_default()
    }

    /// The specification's canonical value: every optional field at its default elided,
    /// so an explicit default serializes as its absence.
    pub fn canonical_value(&self) -> Value {
        let mut spec = self.clone();
        if spec.on_table_error == Some(OnTableError::Abort) {
            spec.on_table_error = None;
        }
        if spec.destination.as_ref().is_some_and(|d| d.name == STORE_DESTINATION && is_empty_object(&d.config)) {
            spec.destination = None;
        }
        for t in &mut spec.tables {
            if let TableEntry::Block(d) = t {
                if d.write_mode == Some(WriteMode::Append) {
                    d.write_mode = None;
                }
                if d.order_by.as_deref() == Some(crate::store::reserve::INGESTED_AT) {
                    d.order_by = None;
                }
                if **d == TableDecl::named(d.name.clone()) {
                    *t = TableEntry::Name(d.name.clone());
                }
            }
        }
        serde_json::to_value(&spec).unwrap_or(Value::Null)
    }

    /// The sha256 of the RFC 8785 canonical JSON of [`PipelineSpec::canonical_value`]
    /// (`run.declare.content-hash`).
    pub fn content_hash(&self) -> String {
        sha256_hex(canonical_json(&self.canonical_value()).as_bytes())
    }

    /// Hold the declaration to the rules checked before any I/O.
    pub fn validate(&self) -> Result<(), RunError> {
        if let Some(d) = &self.destination {
            if d.name != STORE_DESTINATION {
                return Err(RunError::PipelineUnknownDestination(format!(
                    "pipeline `{}` names destination `{}`; the local store is the only destination",
                    self.id, d.name
                )));
            }
        }
        for t in &self.tables {
            let d = t.decl();
            if d.write_mode() == WriteMode::Replace {
                let reason = if self.incremental.is_some() {
                    Some("a `monotonic` cursor")
                } else if self.backfill.is_some() {
                    Some("a backfill chunk plan")
                } else if self.seed.is_some() {
                    Some("a seed ceiling")
                } else {
                    None
                };
                if let Some(reason) = reason {
                    return Err(RunError::PipelineReplaceUnsupported(format!(
                        "pipeline `{}` table `{}` declares `write_mode = \"replace\"` beside {reason}; a windowed or chunked load is not the table's whole state",
                        self.id,
                        d.name
                    )));
                }
            }
        }
        Ok(())
    }

    /// The destination table name of `table` (`run.declare.table-name`).
    pub fn table_name(&self, table: &str) -> String {
        table_name(&self.id, table)
    }
}

/// `<pipeline id>_<table name>`, each non-alphanumeric character folded to `_` and each
/// ASCII uppercase letter lowered.
pub fn table_name(pipeline_id: &str, table: &str) -> String {
    format!("{pipeline_id}_{table}").chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect()
}

/// A manifest file as read: its path and text.
#[derive(Debug, Clone)]
pub struct ManifestFile {
    pub path: String,
    pub text: String,
}

/// A specification and where it was declared.
#[derive(Debug, Clone, PartialEq)]
pub struct Declared {
    pub spec: PipelineSpec,
    pub file: String,
    pub line: usize,
}

#[derive(Deserialize)]
struct Blocks {
    #[serde(default)]
    pipeline: Option<toml::Value>,
}

fn invalid(file: &str, path: &str, e: impl std::fmt::Display) -> RunError {
    let path = if path.is_empty() || path == "." { "<root>".to_string() } else { path.to_string() };
    RunError::PipelineSpecInvalid(format!("{file}: at `{path}`: {}", e.to_string().trim()))
}

fn from_toml(file: &str, value: toml::Value, base: &str) -> Result<PipelineSpec, RunError> {
    serde_path_to_error::deserialize(value).map_err(|e| {
        let path = e.path().to_string();
        invalid(file, &format!("{base}{}", if path == "." { String::new() } else { format!(".{path}") }), e.into_inner())
    })
}

/// Lines on which each `[[pipeline]]` header of `text` begins, one-based.
fn block_lines(text: &str) -> Vec<usize> {
    text.lines().enumerate().filter(|(_, l)| l.trim() == "[[pipeline]]").map(|(i, _)| i + 1).collect()
}

/// Read one manifest file into its specifications. `contextful.toml` contributes its
/// inline `[[pipeline]]` blocks; a file under `pipelines/` is one specification, or
/// `[[pipeline]]` blocks of its own.
pub fn read_manifest(f: &ManifestFile) -> Result<Vec<Declared>, RunError> {
    if f.path.ends_with(".json") {
        let spec: PipelineSpec = serde_path_to_error::deserialize(&mut serde_json::Deserializer::from_str(&f.text))
            .map_err(|e| invalid(&f.path, &e.path().to_string(), e.into_inner()))?;
        return Ok(vec![Declared { spec, file: f.path.clone(), line: 1 }]);
    }
    let value: toml::Value = toml::from_str(&f.text).map_err(|e| invalid(&f.path, "", e.message()))?;
    let blocks: Blocks = value.clone().try_into().map_err(|e: toml::de::Error| invalid(&f.path, "", e.message()))?;
    match blocks.pipeline {
        Some(toml::Value::Array(items)) => {
            let lines = block_lines(&f.text);
            items
                .into_iter()
                .enumerate()
                .map(|(i, item)| {
                    let spec = from_toml(&f.path, item, &format!("pipeline[{i}]"))?;
                    Ok(Declared { spec, file: f.path.clone(), line: lines.get(i).copied().unwrap_or(1) })
                })
                .collect()
        }
        // A `[pipeline]` table holding only `tables` is a store declaration, not a specification.
        Some(_) => Ok(Vec::new()),
        None if value.get("id").is_some() => Ok(vec![Declared { spec: from_toml(&f.path, value, "")?, file: f.path.clone(), line: 1 }]),
        None => Ok(Vec::new()),
    }
}

/// Collect every specification by `id` across the manifest files, in order. One id
/// declared twice refuses, naming each file and line (`run.declare.duplicate-id`).
pub fn collect(files: &[ManifestFile]) -> Result<Vec<Declared>, RunError> {
    let mut all: Vec<Declared> = Vec::new();
    for f in files {
        for d in read_manifest(f)? {
            if let Some(first) = all.iter().find(|x| x.spec.id == d.spec.id) {
                return Err(RunError::PipelineDuplicateId(format!(
                    "pipeline `{}` is declared at {}:{} and at {}:{}",
                    d.spec.id, first.file, first.line, d.file, d.line
                )));
            }
            all.push(d);
        }
    }
    Ok(all)
}
