//! `store.reconcile`: the column types a table carries, the one-promotion lattice, the
//! binary and vector types that take no promotion, the struct, list and map types that
//! reconcile field by field, and the merged schema persisted as
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

/// One named field of a struct column. Every field is nullable: a file written before a
/// field joined reads it as null (`store.reconcile.nested-lattice`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StructField {
    pub name: String,
    pub ty: ColumnType,
}

impl StructField {
    pub fn new(name: impl Into<String>, ty: ColumnType) -> StructField {
        StructField {
            name: name.into(),
            ty,
        }
    }
}

/// The logical type of one column.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
    /// Named fields in order, each nullable.
    Struct(Vec<StructField>),
    /// A variable-length list of one nullable item type.
    List(Box<ColumnType>),
    /// A map from `Utf8` keys to one nullable value type.
    Map(Box<ColumnType>),
    /// One scalar kind per row in a tagged struct.
    Variant,
}

/// The name of a list's item field and of a map's entries, keys and values in the Arrow
/// JSON form.
pub const LIST_ITEM: &str = "item";
pub const MAP_ENTRIES: &str = "entries";
pub const MAP_KEY: &str = "key";
pub const MAP_VALUE: &str = "value";
pub const VARIANT_EXTENSION: &str = "contextful.variant";

pub fn variant_fields() -> Vec<StructField> {
    [
        ("kind", ColumnType::Utf8),
        ("str", ColumnType::Utf8),
        ("int", ColumnType::Int64),
        ("double", ColumnType::Float64),
        ("bool", ColumnType::Boolean),
        ("bytes", ColumnType::Binary),
    ]
    .into_iter()
    .map(|(name, ty)| StructField::new(name, ty))
    .collect()
}

impl ColumnType {
    /// A list of `item`.
    pub fn list(item: ColumnType) -> ColumnType {
        ColumnType::List(Box::new(item))
    }

    /// A map from `Utf8` keys to `value`.
    pub fn map(value: ColumnType) -> ColumnType {
        ColumnType::Map(Box::new(value))
    }

    /// A struct of `fields`, in order.
    pub fn structure<N: Into<String>>(
        fields: impl IntoIterator<Item = (N, ColumnType)>,
    ) -> ColumnType {
        ColumnType::Struct(
            fields
                .into_iter()
                .map(|(n, t)| StructField::new(n, t))
                .collect(),
        )
    }

    /// The type's name as a refusal reports it.
    pub fn name(&self) -> String {
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
            ColumnType::Struct(fields) => {
                format!(
                    "Struct<{}>",
                    fields
                        .iter()
                        .map(|f| format!("{}: {}", f.name, f.ty.name()))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
            ColumnType::List(item) => format!("List<{}>", item.name()),
            ColumnType::Map(value) => format!("Map<Utf8, {}>", value.name()),
            ColumnType::Variant => "Variant".into(),
        }
    }

    /// The type as a declaration spells it, the inverse of [`ColumnType::parse`].
    pub fn spell(&self) -> String {
        match self {
            ColumnType::Null => "null".into(),
            ColumnType::Boolean => "boolean".into(),
            ColumnType::Int32 => "int32".into(),
            ColumnType::Int64 => "int64".into(),
            ColumnType::Float64 => "float64".into(),
            ColumnType::Utf8 => "utf8".into(),
            ColumnType::Json => "json".into(),
            ColumnType::Timestamp => "timestamp".into(),
            ColumnType::Binary => "binary".into(),
            ColumnType::FixedSizeBinary(n) => format!("binary({n})"),
            ColumnType::FixedSizeList(FloatItem::Float32, n) => format!("float32[{n}]"),
            ColumnType::FixedSizeList(FloatItem::Float16, n) => format!("float16[{n}]"),
            ColumnType::Struct(fields) => {
                format!(
                    "struct<{}>",
                    fields
                        .iter()
                        .map(|f| format!("{}: {}", f.name, f.ty.spell()))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
            ColumnType::List(item) => format!("list<{}>", item.spell()),
            ColumnType::Map(value) => format!("map<utf8, {}>", value.spell()),
            ColumnType::Variant => "variant".into(),
        }
    }

    /// Whether the column carries bytes.
    pub fn is_binary(&self) -> bool {
        matches!(self, ColumnType::Binary | ColumnType::FixedSizeBinary(_))
    }

    /// Whether the column carries a fixed-size vector.
    pub fn is_vector(&self) -> bool {
        matches!(self, ColumnType::FixedSizeList(..))
    }

    /// Whether the column is a struct, a list or a map.
    pub fn is_nested(&self) -> bool {
        matches!(
            self,
            ColumnType::Struct(_) | ColumnType::List(_) | ColumnType::Map(_) | ColumnType::Variant
        )
    }

    /// Whether the type holds a fixed-size vector at any depth.
    pub fn holds_vector(&self) -> bool {
        match self {
            ColumnType::FixedSizeList(..) => true,
            ColumnType::Struct(fields) => fields.iter().any(|f| f.ty.holds_vector()),
            ColumnType::List(t) | ColumnType::Map(t) => t.holds_vector(),
            _ => false,
        }
    }

    /// Parse a type as the command line and declarations spell it: `binary`, `binary(<n>)`
    /// for a fixed width, `float32[<n>]` or `float16[<n>]` for a vector, and
    /// `struct<<name>: <type>, …>`, `list<<type>>` and `map<utf8, <type>>` over any of them.
    /// Keywords are case-insensitive; a field name keeps its case.
    pub fn parse(s: &str) -> Option<ColumnType> {
        let s = s.trim();
        if let Some(open) = s.find('<') {
            let keyword = s[..open].trim().to_ascii_lowercase();
            let inner = s[open + 1..].strip_suffix('>')?;
            let parts = split_top(inner)?;
            return match (keyword.as_str(), parts.as_slice()) {
                ("list", [item]) => Some(ColumnType::list(ColumnType::parse(item)?)),
                ("map", [key, value]) => {
                    (ColumnType::parse(key)? == ColumnType::Utf8).then_some(())?;
                    Some(ColumnType::map(ColumnType::parse(value)?))
                }
                ("struct", fields) => {
                    let mut out: Vec<StructField> = Vec::new();
                    for f in fields {
                        let (name, ty) = f.split_once(':')?;
                        let name = name.trim();
                        if !is_field_name(name) || out.iter().any(|o| o.name == name) {
                            return None;
                        }
                        out.push(StructField::new(name, ColumnType::parse(ty)?));
                    }
                    (!out.is_empty()).then_some(ColumnType::Struct(out))
                }
                _ => None,
            };
        }
        let lower = s.to_ascii_lowercase();
        let width = |digits: &str| {
            digits
                .parse::<u32>()
                .ok()
                .filter(|n| (1..=MAX_WIDTH).contains(n))
        };
        if let Some(n) = lower
            .strip_prefix("binary(")
            .and_then(|r| r.strip_suffix(')'))
        {
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
            "variant" => ColumnType::Variant,
            _ => return None,
        })
    }

    /// The SQL type the engine reads the column as, and a zero-row branch casts a literal
    /// null to: bytes of either width as `BLOB`, a vector as a `FLOAT` array of its
    /// dimension whatever its stored width, a struct as `STRUCT`, a list as `T[]` and a
    /// map as `MAP(VARCHAR, T)`.
    pub fn sql(&self) -> String {
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
            ColumnType::Struct(fields) => format!(
                "STRUCT({})",
                fields
                    .iter()
                    .map(|f| format!("\"{}\" {}", f.name.replace('"', "\"\""), f.ty.sql()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            ColumnType::List(item) => format!("{}[]", item.sql()),
            ColumnType::Map(value) => format!("MAP(VARCHAR, {})", value.sql()),
            ColumnType::Variant => ColumnType::Struct(variant_fields()).sql(),
        }
    }

    fn arrow_json(&self) -> Value {
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
            ColumnType::Struct(_) => json!({"name": "struct"}),
            ColumnType::List(_) => json!({"name": "list"}),
            ColumnType::Map(_) => json!({"name": "map", "keysSorted": false}),
            ColumnType::Variant => json!({"name": "struct"}),
        }
    }

    /// The child fields the Arrow JSON form carries: a vector's one element field, a
    /// struct's fields, a list's item and a map's entries.
    fn arrow_children(&self) -> Value {
        match self {
            ColumnType::FixedSizeList(item, _) => json!([{
                "name": VECTOR_ITEM,
                "nullable": false,
                "type": {"name": "floatingpoint", "precision": item.arrow_precision()},
                "children": []
            }]),
            ColumnType::Struct(fields) => Value::Array(
                fields
                    .iter()
                    .map(|f| field_json(&f.name, &f.ty, true))
                    .collect(),
            ),
            ColumnType::List(item) => json!([field_json(LIST_ITEM, item, true)]),
            ColumnType::Map(value) => json!([{
                "name": MAP_ENTRIES,
                "nullable": false,
                "type": {"name": "struct"},
                "children": [field_json(MAP_KEY, &ColumnType::Utf8, false), field_json(MAP_VALUE, value, true)]
            }]),
            ColumnType::Variant => Value::Array(variant_fields().iter().map(|f| field_json(&f.name, &f.ty, true)).collect()),
            _ => json!([]),
        }
    }

    fn from_arrow_json(ty: &Value, children: Option<&Value>, extension: Option<&str>) -> Option<ColumnType> {
        let name = ty.get("name")?.as_str()?;
        let width = |key: &str| {
            ty.get(key)?
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| (1..=MAX_WIDTH).contains(n))
        };
        let children = || {
            children
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default()
        };
        Some(match name {
            "null" => ColumnType::Null,
            "bool" => ColumnType::Boolean,
            "int" => match ty.get("bitWidth")?.as_u64()? {
                32 => ColumnType::Int32,
                64 => ColumnType::Int64,
                _ => return None,
            },
            "floatingpoint" if ty.get("precision")?.as_str()? == "DOUBLE" => ColumnType::Float64,
            "utf8" if extension == Some(JSON_EXTENSION) => ColumnType::Json,
            "utf8" => ColumnType::Utf8,
            "timestamp" if ty.get("unit")?.as_str()? == "NANOSECOND" => ColumnType::Timestamp,
            "binary" => ColumnType::Binary,
            "fixedsizebinary" => ColumnType::FixedSizeBinary(width("byteWidth")?),
            "fixedsizelist" => {
                let [child] = children() else { return None };
                let item = match child.get("type")?.get("precision")?.as_str()? {
                    "SINGLE" => FloatItem::Float32,
                    "HALF" => FloatItem::Float16,
                    _ => return None,
                };
                ColumnType::FixedSizeList(item, width("listSize")?)
            }
            "struct" => {
                let fields = children()
                    .iter()
                    .map(|c| field_from_json(c).map(|(n, t, _)| StructField::new(n, t)))
                    .collect::<Option<Vec<_>>>()?;
                if extension == Some(VARIANT_EXTENSION) {
                    (fields == variant_fields()).then_some(ColumnType::Variant)?
                } else {
                    (!fields.is_empty()).then_some(ColumnType::Struct(fields))?
                }
            }
            "list" => {
                let [child] = children() else { return None };
                ColumnType::list(field_from_json(child)?.1)
            }
            "map" => {
                let [entries] = children() else { return None };
                let [key, value] = entries.get("children")?.as_array()?.as_slice() else {
                    return None;
                };
                (field_from_json(key)?.1 == ColumnType::Utf8).then_some(())?;
                ColumnType::map(field_from_json(value)?.1)
            }
            _ => return None,
        })
    }
}

/// Split `s` at the commas outside any `<…>`, trimming each part; `None` for unbalanced
/// brackets or an empty part.
fn split_top(s: &str) -> Option<Vec<&str>> {
    let (mut depth, mut start, mut parts) = (0i32, 0usize, Vec::new());
    for (i, ch) in s.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
        if depth < 0 {
            return None;
        }
    }
    parts.push(s[start..].trim());
    (depth == 0 && parts.iter().all(|p| !p.is_empty())).then_some(parts)
}

/// A struct field name a declaration spells: letters, digits and `_`, not starting with a digit.
fn is_field_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// One field in Arrow JSON form, carrying the JSON extension on a JSON column at any depth.
fn field_json(name: &str, ty: &ColumnType, nullable: bool) -> Value {
    let mut f = json!({"name": name, "nullable": nullable, "type": ty.arrow_json(), "children": ty.arrow_children()});
    if let Some(extension) = match ty {
        ColumnType::Json => Some(JSON_EXTENSION),
        ColumnType::Variant => Some(VARIANT_EXTENSION),
        _ => None,
    } {
        f["metadata"] = json!([{"key": EXTENSION_NAME, "value": extension}]);
    }
    f
}

/// The name, type and nullability one Arrow JSON field carries.
fn field_from_json(f: &Value) -> Option<(String, ColumnType, bool)> {
    let extension = f
        .get("metadata")
        .and_then(Value::as_array)
        .and_then(|m| m.iter().find(|kv| kv.get("key").and_then(Value::as_str) == Some(EXTENSION_NAME)))
        .and_then(|kv| kv.get("value"))
        .and_then(Value::as_str);
    Some((
        f.get("name")?.as_str()?.to_string(),
        ColumnType::from_arrow_json(f.get("type")?, f.get("children"), extension)?,
        f.get("nullable")?.as_bool()?,
    ))
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
/// (`store.reconcile.lattice`, `store.reconcile.nested-lattice`).
pub fn supertype(a: &ColumnType, b: &ColumnType) -> Option<ColumnType> {
    reconcile_type(a, b, "").ok()
}

/// Where two types for one column meet no supertype: the path inside the column, and the
/// stored and arriving types found there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeClash {
    /// `` for the column itself; `.field`, `[]` for a list item and `{}` for a map value below it.
    pub path: String,
    pub stored: ColumnType,
    pub arriving: ColumnType,
}

/// The supertype of `a` and `b` under the lattice, recursing into nested types: a struct
/// gains the fields of either side, the stored side's first; a list item and a map value
/// reconcile by the lattice; a kind change refuses at the path where it occurs.
pub fn reconcile_type(a: &ColumnType, b: &ColumnType, path: &str) -> Result<ColumnType, TypeClash> {
    use ColumnType::*;
    let clash = || TypeClash {
        path: path.to_string(),
        stored: a.clone(),
        arriving: b.clone(),
    };
    Ok(match (a, b) {
        _ if a == b => a.clone(),
        (Null, t) | (t, Null) => t.clone(),
        (Int64, Float64) | (Float64, Int64) => Float64,
        (Json, Utf8) | (Utf8, Json) => Json,
        (List(x), List(y)) => ColumnType::list(reconcile_type(x, y, &format!("{path}[]"))?),
        (Map(x), Map(y)) => ColumnType::map(reconcile_type(x, y, &format!("{path}{{}}"))?),
        (Struct(xs), Struct(ys)) => {
            let mut fields = xs.clone();
            for y in ys {
                match fields.iter_mut().find(|f| f.name == y.name) {
                    Some(f) => f.ty = reconcile_type(&f.ty, &y.ty, &format!("{path}.{}", y.name))?,
                    None => fields.push(y.clone()),
                }
            }
            Struct(fields)
        }
        _ => return Err(clash()),
    })
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
                    let sup = reconcile_type(&stored.ty, &col.ty, "").map_err(|c| {
                        let at = if c.path.is_empty() {
                            String::new()
                        } else {
                            format!(" at `{}{}`", col.name, c.path)
                        };
                        StoreError::StoreSchemaIncompatible(format!(
                            "column `{}` is stored as {} and arrives as {}{at}{}",
                            col.name,
                            stored.ty.name(),
                            col.ty.name(),
                            if c.path.is_empty() {
                                String::new()
                            } else {
                                format!(
                                    ", stored there as {} and arriving as {}",
                                    c.stored.name(),
                                    c.arriving.name()
                                )
                            }
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
                    merged.columns.push(Column {
                        name: col.name.clone(),
                        ty: col.ty.clone(),
                        nullable,
                    });
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
            .map(|c| field_json(&c.name, &c.ty, c.nullable))
            .collect();
        json!({ "fields": fields })
    }

    /// Decode the Arrow JSON form; `None` names a malformed document.
    pub fn from_arrow_json(v: &Value) -> Option<Schema> {
        let mut columns = Vec::new();
        for f in v.get("fields")?.as_array()? {
            let (name, ty, nullable) = field_from_json(f)?;
            columns.push(Column { name, ty, nullable });
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
