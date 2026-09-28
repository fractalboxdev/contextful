//! `store.reconcile`: the column types a table carries, the one-promotion lattice, the
//! binary and vector types that take no promotion, and the merged schema persisted as
//! `schema.json` in Arrow JSON form.

use super::StoreError;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Largest integer a `Float64` holds exactly (`store.reconcile.float-loss`).
pub const FLOAT_EXACT_INTEGER_LIMIT: i64 = 9_007_199_254_740_992;

/// The element type of a fixed-size vector column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FloatItem {
    Float32,
    /// Half precision: stored at 2 bytes an element, read by the engine as `FLOAT`.
    Float16,
}

impl FloatItem {
    pub fn name(self) -> &'static str {
        match self {
            FloatItem::Float32 => "Float32",
            FloatItem::Float16 => "Float16",
        }
    }

    fn arrow_precision(self) -> &'static str {
        match self {
            FloatItem::Float32 => "SINGLE",
            FloatItem::Float16 => "HALF",
        }
    }
}

/// The logical type of one column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColumnType {
    /// Every observed value was null; any type absorbs it.
    Null,
    Boolean,
    Int32,
    Int64,
    Float64,
    Utf8,
    /// Text carrying a JSON document.
    Json,
    /// `TIMESTAMP(UTC, NANOS)`.
    Timestamp,
    /// Variable-length bytes.
    Binary,
    /// Bytes of one fixed width, such as a digest.
    FixedSizeBinary(u32),
    /// A vector of one fixed dimension over one float width.
    FixedSizeList(FloatItem, u32),
}

impl ColumnType {
    /// The type's name as a refusal reports it.
    pub fn name(self) -> String {
        match self {
            ColumnType::Null => "Null".into(),
            ColumnType::Boolean => "Boolean".into(),
            ColumnType::Int32 => "Int32".into(),
            ColumnType::Int64 => "Int64".into(),
            ColumnType::Float64 => "Float64".into(),
            ColumnType::Utf8 => "Utf8".into(),
            ColumnType::Json => "Json".into(),
            ColumnType::Timestamp => "Timestamp".into(),
            ColumnType::Binary => "Binary".into(),
            ColumnType::FixedSizeBinary(n) => format!("FixedSizeBinary({n})"),
            ColumnType::FixedSizeList(item, n) => format!("FixedSizeList<{}, {n}>", item.name()),
        }
    }

    /// Whether the column carries bytes.
    pub fn is_binary(self) -> bool {
        matches!(self, ColumnType::Binary | ColumnType::FixedSizeBinary(_))
    }

    /// Whether the column carries a fixed-size vector.
    pub fn is_vector(self) -> bool {
        matches!(self, ColumnType::FixedSizeList(..))
    }

    /// Parse a type as the command line and declarations spell it: `binary`, `binary(<n>)`
    /// for a fixed width, and `float32[<n>]` or `float16[<n>]` for a vector.
    pub fn parse(s: &str) -> Option<ColumnType> {
        let lower = s.trim().to_ascii_lowercase();
        let width = |digits: &str| digits.parse::<u32>().ok().filter(|n| (1..=MAX_WIDTH).contains(n));
        if let Some(n) = lower.strip_prefix("binary(").and_then(|r| r.strip_suffix(')')) {
            return width(n).map(ColumnType::FixedSizeBinary);
        }
        for (prefix, item) in [("float32[", FloatItem::Float32), ("float16[", FloatItem::Float16)] {
            if let Some(n) = lower.strip_prefix(prefix).and_then(|r| r.strip_suffix(']')) {
                return width(n).map(|n| ColumnType::FixedSizeList(item, n));
            }
        }
        Some(match lower.as_str() {
            "boolean" | "bool" => ColumnType::Boolean,
            "int32" => ColumnType::Int32,
            "int64" | "integer" => ColumnType::Int64,
            "float64" | "double" => ColumnType::Float64,
            "utf8" | "string" | "text" => ColumnType::Utf8,
            "json" => ColumnType::Json,
            "timestamp" => ColumnType::Timestamp,
            "binary" | "bytes" => ColumnType::Binary,
            _ => return None,
        })
    }

    /// The SQL type the engine reads the column as, and a zero-row branch casts a literal
    /// null to: bytes of either width as `BLOB`, a vector as a `FLOAT` array of its
    /// dimension whatever its stored width.
    pub fn sql(self) -> String {
        match self {
            ColumnType::Null => "INTEGER".into(),
            ColumnType::Boolean => "BOOLEAN".into(),
            ColumnType::Int32 => "INTEGER".into(),
            ColumnType::Int64 => "BIGINT".into(),
            ColumnType::Float64 => "DOUBLE".into(),
            ColumnType::Utf8 => "VARCHAR".into(),
            ColumnType::Json => "JSON".into(),
            ColumnType::Timestamp => "TIMESTAMPTZ".into(),
            ColumnType::Binary | ColumnType::FixedSizeBinary(_) => "BLOB".into(),
            ColumnType::FixedSizeList(_, n) => format!("FLOAT[{n}]"),
        }
    }

    fn arrow_json(self) -> Value {
        match self {
            ColumnType::Null => json!({"name": "null"}),
            ColumnType::Boolean => json!({"name": "bool"}),
            ColumnType::Int32 => json!({"name": "int", "bitWidth": 32, "isSigned": true}),
            ColumnType::Int64 => json!({"name": "int", "bitWidth": 64, "isSigned": true}),
            ColumnType::Float64 => json!({"name": "floatingpoint", "precision": "DOUBLE"}),
            ColumnType::Utf8 | ColumnType::Json => json!({"name": "utf8"}),
            ColumnType::Timestamp => json!({"name": "timestamp", "unit": "NANOSECOND", "timezone": "UTC"}),
            ColumnType::Binary => json!({"name": "binary"}),
            ColumnType::FixedSizeBinary(n) => json!({"name": "fixedsizebinary", "byteWidth": n}),
            ColumnType::FixedSizeList(_, n) => json!({"name": "fixedsizelist", "listSize": n}),
        }
    }

    /// The child fields the Arrow JSON form carries: a vector's one element field.
    fn arrow_children(self) -> Value {
        match self {
            ColumnType::FixedSizeList(item, _) => json!([{
                "name": VECTOR_ITEM,
                "nullable": false,
                "type": {"name": "floatingpoint", "precision": item.arrow_precision()},
                "children": []
            }]),
            _ => json!([]),
        }
    }

    fn from_arrow_json(ty: &Value, children: Option<&Value>, json_extension: bool) -> Option<ColumnType> {
        let name = ty.get("name")?.as_str()?;
        let width = |key: &str| ty.get(key)?.as_u64().and_then(|n| u32::try_from(n).ok()).filter(|n| (1..=MAX_WIDTH).contains(n));
        Some(match name {
            "null" => ColumnType::Null,
            "bool" => ColumnType::Boolean,
            "int" => match ty.get("bitWidth")?.as_u64()? {
                32 => ColumnType::Int32,
                64 => ColumnType::Int64,
                _ => return None,
            },
            "floatingpoint" if ty.get("precision")?.as_str()? == "DOUBLE" => ColumnType::Float64,
            "utf8" if json_extension => ColumnType::Json,
            "utf8" => ColumnType::Utf8,
            "timestamp" if ty.get("unit")?.as_str()? == "NANOSECOND" => ColumnType::Timestamp,
            "binary" => ColumnType::Binary,
            "fixedsizebinary" => ColumnType::FixedSizeBinary(width("byteWidth")?),
            "fixedsizelist" => {
                let [child] = children?.as_array()?.as_slice() else { return None };
                let item = match child.get("type")?.get("precision")?.as_str()? {
                    "SINGLE" => FloatItem::Float32,
                    "HALF" => FloatItem::Float16,
                    _ => return None,
                };
                ColumnType::FixedSizeList(item, width("listSize")?)
            }
            _ => return None,
        })
    }
}

/// Widest fixed width or dimension a declaration spells: Arrow carries it as a signed
/// 32-bit count.
pub const MAX_WIDTH: u32 = i32::MAX as u32;

/// The name of a vector column's element field.
pub const VECTOR_ITEM: &str = "item";

/// Bytes as the JSON row path carries them: standard base64, padded
/// (`read.respond.bytes-and-vectors`).
pub fn encode_binary(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The bytes a padded base64 value carries; `None` for any other text.
pub fn decode_binary(text: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(text).ok()
}

/// The common supertype of two observed types, or `None` where the lattice holds none
/// (`store.reconcile.lattice`).
pub fn supertype(a: ColumnType, b: ColumnType) -> Option<ColumnType> {
    use ColumnType::*;
    match (a, b) {
        _ if a == b => Some(a),
        (Null, t) | (t, Null) => Some(t),
        (Int64, Float64) | (Float64, Int64) => Some(Float64),
        (Json, Utf8) | (Utf8, Json) => Some(Json),
        _ => None,
    }
}

/// One column of a table's schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub ty: ColumnType,
    pub nullable: bool,
}

impl Column {
    pub fn new(name: impl Into<String>, ty: ColumnType, nullable: bool) -> Column {
        Column { name: name.into(), ty, nullable }
    }
}

/// A table's current shape: an ordered column list. Reconciliation keeps this alone and
/// no history of how it was reached (`store.reconcile.no-history`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Schema {
    pub columns: Vec<Column>,
}

/// The Arrow field metadata key naming an extension type.
pub const EXTENSION_NAME: &str = "ARROW:extension:name";

/// The canonical Arrow extension a JSON column carries.
pub const JSON_EXTENSION: &str = "arrow.json";

impl Schema {
    pub fn get(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }

    /// Merge an arriving batch's schema into this one. A column seen in both resolves to
    /// the lattice supertype; an unseen column joins at the end, nullable, since files
    /// written before it read it as null (`store.reconcile.additive`). A key column
    /// taking the float promotion refuses before any Parquet is written.
    pub fn merge(&self, arriving: &Schema, primary_key: &[String]) -> Result<Schema, StoreError> {
        let mut merged = self.clone();
        for col in &arriving.columns {
            match merged.columns.iter_mut().find(|c| c.name == col.name) {
                Some(stored) => {
                    let sup = supertype(stored.ty, col.ty).ok_or_else(|| {
                        StoreError::StoreSchemaIncompatible(format!(
                            "column `{}` is stored as {} and arrives as {}",
                            col.name,
                            stored.ty.name(),
                            col.ty.name()
                        ))
                    })?;
                    if sup == ColumnType::Float64
                        && (stored.ty == ColumnType::Int64 || col.ty == ColumnType::Int64)
                        && primary_key.contains(&col.name)
                    {
                        return Err(StoreError::StoreKeyWidened(format!(
                            "primary-key column `{}` is stored as {} and arrives as {}; a key takes no Float64 promotion",
                            col.name,
                            stored.ty.name(),
                            col.ty.name()
                        )));
                    }
                    stored.ty = sup;
                    stored.nullable |= col.nullable;
                }
                None => {
                    let nullable = col.nullable || !self.columns.is_empty();
                    merged.columns.push(Column { name: col.name.clone(), ty: col.ty, nullable });
                }
            }
        }
        Ok(merged)
    }

    /// The Arrow JSON form written to `schema.json`.
    pub fn to_arrow_json(&self) -> Value {
        let fields: Vec<Value> = self
            .columns
            .iter()
            .map(|c| {
                let mut f = json!({"name": c.name, "nullable": c.nullable, "type": c.ty.arrow_json(), "children": c.ty.arrow_children()});
                if c.ty == ColumnType::Json {
                    f["metadata"] = json!([{"key": EXTENSION_NAME, "value": JSON_EXTENSION}]);
                }
                f
            })
            .collect();
        json!({ "fields": fields })
    }

    /// Decode the Arrow JSON form; `None` names a malformed document.
    pub fn from_arrow_json(v: &Value) -> Option<Schema> {
        let mut columns = Vec::new();
        for f in v.get("fields")?.as_array()? {
            let json_ext = f
                .get("metadata")
                .and_then(Value::as_array)
                .is_some_and(|m| m.iter().any(|kv| kv.get("value").and_then(Value::as_str) == Some(JSON_EXTENSION)));
            columns.push(Column {
                name: f.get("name")?.as_str()?.to_string(),
                nullable: f.get("nullable")?.as_bool()?,
                ty: ColumnType::from_arrow_json(f.get("type")?, f.get("children"), json_ext)?,
            });
        }
        Some(Schema { columns })
    }
}

impl Serialize for Schema {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_arrow_json().serialize(s)
    }
}

impl<'de> Deserialize<'de> for Schema {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        Schema::from_arrow_json(&v).ok_or_else(|| serde::de::Error::custom("not an Arrow JSON schema"))
    }
}
