//! `store.declare`: a table's declaration block and the checks over it.

use super::reconcile::{ColumnType, Schema};
use super::reserve::{check_table_name, is_injected, INGESTED_AT};
use super::StoreError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    pub class: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_time: Option<ValidTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_by: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_by: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retain_runs: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example_queries: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct PipelineFile {
    pipeline: PipelineTables,
}

#[derive(Deserialize)]
struct PipelineTables {
    #[serde(default)]
    tables: Vec<TableDecl>,
}

/// A declaration that does not parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("declaration malformed: {0}")]
pub struct DeclarationMalformed(pub String);

impl TableDecl {
    /// A table with every key unset.
    pub fn named(name: impl Into<String>) -> TableDecl {
        TableDecl { name: name.into(), ..TableDecl::default() }
    }

    /// Every `[[pipeline.tables]]` block of a pipeline manifest.
    pub fn parse_pipeline(toml_text: &str) -> Result<Vec<TableDecl>, DeclarationMalformed> {
        let file: PipelineFile = toml::from_str(toml_text).map_err(|e| DeclarationMalformed(e.to_string()))?;
        for t in &file.pipeline.tables {
            t.retain_runs_secs()?;
        }
        Ok(file.pipeline.tables)
    }

    /// The canonical serialization: JSON with every unset key absent.
    pub fn canonical(&self) -> String {
        serde_json::to_string(self).expect("a declaration serializes")
    }

    pub fn primary_key(&self) -> &[String] {
        self.primary_key.as_deref().unwrap_or(&[])
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

    /// The `retain_runs` window in seconds: `<n>d`, `<n>h`, `<n>m` or `<n>s`.
    pub fn retain_runs_secs(&self) -> Result<u64, DeclarationMalformed> {
        let Some(s) = &self.retain_runs else { return Ok(DEFAULT_RETAIN_RUNS_SECS) };
        let bad = || DeclarationMalformed(format!("table `{}`: retain_runs `{s}` is not <n>d, <n>h, <n>m or <n>s", self.name));
        let per = match s.chars().next_back().ok_or_else(bad)? {
            'd' => 86_400,
            'h' => 3_600,
            'm' => 60,
            's' => 1,
            _ => return Err(bad()),
        };
        let n: u64 = s[..s.len() - 1].parse().map_err(|_| bad())?;
        n.checked_mul(per).ok_or_else(bad)
    }

    /// Hold the declaration to the table's reconciled schema, before any Parquet lands.
    pub fn validate(&self, schema: &Schema) -> Result<(), StoreError> {
        check_table_name(&self.name)?;
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
