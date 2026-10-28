//! `run.export-iceberg`: the Iceberg target of an `[[export]]` block, the Iceberg type each
//! stored column takes, the namespace each credential owns, and the record that tells an
//! unchanged export from one that commits. Catalog requests, encoding and the record file
//! live in the binary; this module performs no I/O.

use super::{Block, Export, ExportError, Signal};
use crate::connector::reference::Template;
use crate::store::lay_out::is_path_segment;
use crate::store::reconcile::{variant_fields, ColumnType, FloatItem};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use url::Url;

/// Snapshot commits one export attempts before refusing (`run.export-iceberg.commit-retry`).
pub const ICEBERG_COMMIT_ATTEMPTS: usize = 3;

/// Largest data file one export writes (`run.export-iceberg.file-size`).
pub const ICEBERG_DATA_FILE_BYTES: u64 = 128 * 1024 * 1024;

/// The table property prefix recording a fixed-size list's width (`run.export-iceberg.fixed-list`).
pub const FIXED_SIZE_LIST_PROPERTY: &str = "contextful.fixed-size-list.";

/// Where one Iceberg export publishes, and the one credential it publishes for
/// (`run.export-iceberg.block`, `run.export-iceberg.audience`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IcebergTarget {
    /// The subject of the one admitted credential the export reads as.
    pub credential: String,
    /// The REST catalog's base URI, ahead of `/v1`.
    pub catalog: Url,
    /// The object-store endpoint the table's files are written through.
    pub storage: Url,
    pub warehouse: String,
    pub namespace: String,
    pub table_name: String,
    /// 2 or 3.
    pub format_version: u8,
    /// The catalog bearer token, hydrated per request.
    pub token: Template,
}

impl IcebergTarget {
    /// The declared target an export record binds (`run.export-iceberg.frontier`).
    pub fn key(&self) -> String {
        serde_json::json!([self.catalog.as_str(), self.storage.as_str(), self.warehouse, self.namespace, self.table_name, self.format_version]).to_string()
    }
}

/// An Iceberg type, its field ids assigned where the schema is built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IcebergType {
    Boolean,
    Int,
    Long,
    Float,
    Double,
    String,
    Binary,
    Fixed(u32),
    /// `timestamptz_ns`, format version 3 alone.
    TimestamptzNs,
    List(Box<IcebergType>),
    /// A map from `string` keys.
    Map(Box<IcebergType>),
    Struct(Vec<(String, IcebergType)>),
}

/// One exported column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IcebergColumn {
    pub name: String,
    pub ty: IcebergType,
}

/// The exported columns, in read order, and the table properties they need.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IcebergPlan {
    pub columns: Vec<IcebergColumn>,
    pub properties: BTreeMap<String, String>,
}

/// What an export last committed, and the values it committed under
/// (`run.export-iceberg.frontier`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IcebergRecord {
    pub snapshot_id: i64,
    pub frontier: String,
    pub identity: String,
    pub target: String,
}

impl IcebergRecord {
    /// Whether the table still stands at the recorded snapshot and the export's present
    /// frontier, read identity and target equal the recorded ones.
    pub fn holds(&self, current_snapshot: Option<i64>, frontier: &str, identity: &str, target: &str) -> bool {
        current_snapshot == Some(self.snapshot_id) && self.frontier == frontier && self.identity == identity && self.target == target
    }
}

/// The Iceberg type of each column the read returns outside the engine's `_` namespace
/// (`run.export-iceberg.cast`, `run.export-iceberg.fixed-list`, `run.export-iceberg.timestamp-ns`),
/// or `ExportTypeUnsupported` naming the first column no type holds
/// (`run.export-iceberg.type-refused`).
pub fn plan(export: &str, columns: &[(String, ColumnType)], format_version: u8) -> Result<IcebergPlan, ExportError> {
    let mut out = IcebergPlan::default();
    for (name, ty) in columns.iter().filter(|(name, _)| !name.starts_with('_')) {
        let ty = map(export, name, ty, format_version, &mut out.properties)?;
        out.columns.push(IcebergColumn { name: name.clone(), ty });
    }
    Ok(out)
}

fn map(export: &str, path: &str, ty: &ColumnType, format_version: u8, properties: &mut BTreeMap<String, String>) -> Result<IcebergType, ExportError> {
    Ok(match ty {
        ColumnType::Boolean => IcebergType::Boolean,
        ColumnType::Int32 => IcebergType::Int,
        ColumnType::Int64 => IcebergType::Long,
        ColumnType::Float64 => IcebergType::Double,
        // A column every value of which is null holds no value a `string` cannot.
        ColumnType::Utf8 | ColumnType::Json | ColumnType::Null => IcebergType::String,
        ColumnType::Binary => IcebergType::Binary,
        ColumnType::FixedSizeBinary(n) => IcebergType::Fixed(*n),
        ColumnType::Timestamp if format_version >= 3 => IcebergType::TimestamptzNs,
        ColumnType::Timestamp => {
            return Err(ExportError::ExportTypeUnsupported(format!(
                "export `{export}`: column `{path}` holds nanosecond timestamps, which `format_version = 2` cannot; declare `format_version = 3`"
            )))
        }
        ColumnType::FixedSizeList(item, n) => {
            properties.insert(format!("{FIXED_SIZE_LIST_PROPERTY}{path}"), n.to_string());
            IcebergType::List(Box::new(match item {
                FloatItem::Float32 | FloatItem::Float16 => IcebergType::Float,
            }))
        }
        ColumnType::List(item) => IcebergType::List(Box::new(map(export, path, item, format_version, properties)?)),
        ColumnType::Map(value) => IcebergType::Map(Box::new(map(export, path, value, format_version, properties)?)),
        ColumnType::Struct(fields) => IcebergType::Struct(
            fields.iter().map(|f| Ok((f.name.clone(), map(export, &format!("{path}.{}", f.name), &f.ty, format_version, properties)?))).collect::<Result<_, ExportError>>()?,
        ),
        ColumnType::Variant => IcebergType::Struct(
            variant_fields().iter().map(|f| Ok((f.name.clone(), map(export, &format!("{path}.{}", f.name), &f.ty, format_version, properties)?))).collect::<Result<_, ExportError>>()?,
        ),
    })
}

/// An Iceberg namespace or table name: a letter or `_`, then letters, digits and `_`, at
/// most 128 bytes, so a fixed query template names it unquoted.
fn is_identifier(s: &str) -> bool {
    s.len() <= 128 && s.bytes().next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_') && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Validate an Iceberg block (`run.export-iceberg.block`).
pub(super) fn check(name: String, block: Block) -> Result<Export, ExportError> {
    let invalid = |why: String| ExportError::Invalid(format!("export `{name}`: {why}"));
    if block.signal.is_some() || block.endpoint.is_some() || !block.key.is_empty() || block.schedule.is_some() || !block.headers.is_empty() {
        return Err(invalid("an Iceberg export takes no `signal`, `endpoint`, `key`, `schedule` or `headers`".into()));
    }
    if block.table.trim().is_empty() {
        return Err(invalid("names no `table`".into()));
    }
    let field = |value: Option<String>, field: &str| value.filter(|v| !v.trim().is_empty()).ok_or_else(|| invalid(format!("an Iceberg export names `{field}`")));
    let url = |value: String, field: &str| {
        let url = Url::parse(&value).map_err(|e| invalid(format!("`{field}`: {e}")))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(invalid(format!("`{field}` is an http or https URL with a host")));
        }
        Ok(url)
    };
    let credential = field(block.credential, "credential")?;
    let catalog = url(field(block.catalog, "catalog")?, "catalog")?;
    let storage = url(field(block.storage, "storage")?, "storage")?;
    let warehouse = field(block.warehouse, "warehouse")?;
    if !is_path_segment(&warehouse) || warehouse.len() > 128 {
        return Err(invalid(format!("`warehouse` `{warehouse}` is not 1 to 128 chars of [A-Za-z0-9._-]")));
    }
    let namespace = field(block.namespace, "namespace")?;
    let table_name = field(block.table_name, "table_name")?;
    for (what, value) in [("namespace", &namespace), ("table_name", &table_name)] {
        if !is_identifier(value) {
            return Err(invalid(format!("`{what}` `{value}` is a letter or `_` followed by letters, digits and `_`")));
        }
    }
    let format_version = match block.format_version {
        Some(2) => 2,
        Some(3) => 3,
        other => return Err(invalid(format!("`format_version` is 2 or 3, not {}", other.map_or("absent".to_string(), |v| v.to_string())))),
    };
    let token = Template::parse(&field(block.token, "token")?).map_err(|e| invalid(format!("`token`: {e}")))?;
    let endpoint = catalog.clone();
    let target = IcebergTarget { credential, catalog, storage, warehouse, namespace, table_name, format_version, token };
    Ok(Export { name, table: block.table, endpoint, signal: Signal::Iceberg, key: Vec::new(), schedule: None, headers: BTreeMap::new(), iceberg: Some(target) })
}

/// One catalog however its URI is spelled: scheme, host, port and path, a trailing `/` dropped.
fn catalog_key(url: &Url) -> String {
    format!("{}://{}:{}{}", url.scheme(), url.host_str().unwrap_or_default().to_ascii_lowercase(), url.port_or_known_default().unwrap_or_default(), url.path().trim_end_matches('/'))
}

/// Refuse two Iceberg exports sharing one namespace under different credentials, one
/// credential under two namespaces, or one table (`run.export-iceberg.namespace-shared`).
pub(super) fn namespace_shared(exports: &[Export]) -> Result<(), ExportError> {
    let targets: Vec<(&str, &IcebergTarget)> = exports.iter().filter_map(|e| e.iceberg.as_ref().map(|t| (e.name.as_str(), t))).collect();
    for (i, (a, ta)) in targets.iter().enumerate() {
        for (b, tb) in &targets[i + 1..] {
            let namespace = |t: &IcebergTarget| (catalog_key(&t.catalog), t.warehouse.clone(), t.namespace.clone());
            let why = if namespace(ta) == namespace(tb) && ta.table_name == tb.table_name {
                Some(format!("both publish table `{}.{}`", ta.namespace, ta.table_name))
            } else if namespace(ta) == namespace(tb) && ta.credential != tb.credential {
                Some(format!("namespace `{}` serves credentials `{}` and `{}`", ta.namespace, ta.credential, tb.credential))
            } else if ta.credential == tb.credential && namespace(ta) != namespace(tb) {
                Some(format!("credential `{}` publishes into namespaces `{}` and `{}`", ta.credential, ta.namespace, tb.namespace))
            } else {
                None
            };
            if let Some(why) = why {
                return Err(ExportError::ExportNamespaceShared(format!("exports `{a}` and `{b}`: {why}; one credential maps to exactly one namespace")));
            }
        }
    }
    Ok(())
}
