//! `run.normalize`: the declared mode and depth, and the nested types `native` infers over
//! a batch of deferred-typing JSON for the store sink.

use crate::run::ports::{Row, Types};
use crate::run::journal::sha256_hex;
use crate::run::RunError;
use crate::store::reconcile::{supertype, ColumnType, StructField};
use serde_json::Value;
use std::collections::BTreeMap;
use crate::redaction::ValueStep;

/// One projected cell's exact source location and physical destination.
#[derive(Debug, Clone)]
pub struct NormalizedCell {
    pub table: String,
    pub column: String,
    pub source_column: String,
    pub path: Vec<ValueStep>,
    pub source_row: usize,
}

/// Logical projection with canonical lineage; materialization derives identities only
/// after the policy has rewritten its source cells.
pub struct NormalizedGroup {
    rows: Vec<Row>,
    table: String,
    load_id: String,
    depth: u32,
    cells: Vec<NormalizedCell>,
    destinations: Vec<String>,
}

impl NormalizedGroup {
    pub fn new(rows: Vec<Row>, table: &str, load_id: &str, depth: u32) -> Self {
        let (tables, cells) = relational(rows.clone(), table, load_id, depth, true);
        Self { rows, table: table.into(), load_id: load_id.into(), depth, cells, destinations: tables.into_keys().collect() }
    }

    pub fn root(&self) -> &str { &self.table }

    pub fn destinations(&self) -> &[String] { &self.destinations }

    pub fn cells(&self) -> &[NormalizedCell] { &self.cells }

    pub fn has_column(&self, column: &str) -> bool {
        self.rows.iter().all(|row| row.contains_key(column))
    }

    pub fn column_values(&self, column: &str) -> Option<Vec<Value>> {
        self.rows.iter().map(|row| row.get(column).cloned()).collect()
    }

    pub fn rewrite_cells(&mut self, rewrite: &mut dyn FnMut(&NormalizedCell, &mut Value) -> Result<(), crate::enforce::EnforceError>) -> Result<(), crate::enforce::EnforceError> {
        for cell in &self.cells {
            let mut value = self.rows.get_mut(cell.source_row).and_then(|r| r.get_mut(&cell.source_column));
            for step in &cell.path {
                value = value.and_then(|v| match step {
                    ValueStep::Key(key) => v.as_object_mut().and_then(|v| v.get_mut(key)),
                    ValueStep::Index(index) => v.as_array_mut().and_then(|v| v.get_mut(*index)),
                });
            }
            let value = value.ok_or_else(|| crate::enforce::EnforceError::RedactionInvalid("normalized lineage no longer resolves its source cell".into()))?;
            rewrite(cell, value)?;
        }
        Ok(())
    }

    /// The shredded tables, each child holding the list index that reverses its projection
    /// (`run.normalize.list-index-missing`).
    pub fn into_tables(self) -> Result<BTreeMap<String, Vec<Row>>, RunError> {
        let tables = relational_tables(self.rows, &self.table, &self.load_id, self.depth);
        check_list_index(&tables, &self.table)?;
        Ok(tables)
    }
}

/// Levels of nesting `native` keeps before a deeper subtree lands as one `Json` value
/// (`run.normalize.nesting-depth`).
pub const DEFAULT_NESTING_DEPTH: u32 = 5;

/// How a batch's nested values reach the sink (`run.normalize.mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Native,
    Relational,
}

impl Mode {
    pub const SPELLINGS: [&'static str; 2] = ["native", "relational"];

    fn parse(s: &str) -> Result<Mode, RunError> {
        match s {
            "native" => Ok(Mode::Native),
            "relational" => Ok(Mode::Relational),
            other => Err(RunError::PipelineNormalizeModeUnknown(format!(
                "normalize mode `{other}` is neither `{}` nor `{}`",
                Mode::SPELLINGS[0],
                Mode::SPELLINGS[1]
            ))),
        }
    }
}

/// A pipeline's `normalize` declaration: `"native"`, `"relational"`, or a table carrying
/// `mode` and `depth`. An absent declaration resolves to `native` at the default depth
/// (`run.normalize.mode-resolution`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Normalize {
    pub mode: Mode,
    pub depth: u32,
}

impl Default for Normalize {
    fn default() -> Normalize {
        Normalize {
            mode: Mode::Native,
            depth: DEFAULT_NESTING_DEPTH,
        }
    }
}

impl Normalize {
    /// Canonical mode and depth bind prepared recording projections.
    pub fn recording_projection(self) -> (&'static str, u32) {
        (match self.mode { Mode::Native => Mode::SPELLINGS[0], Mode::Relational => Mode::SPELLINGS[1] }, self.depth)
    }

    pub fn parse(declared: Option<&Value>) -> Result<Normalize, RunError> {
        let malformed = |why: String| RunError::Invalid(format!("normalize: {why}"));
        Ok(match declared {
            None => Normalize::default(),
            Some(Value::String(mode)) => Normalize {
                mode: Mode::parse(mode)?,
                ..Normalize::default()
            },
            Some(Value::Object(o)) => {
                if let Some(key) = o.keys().find(|k| !["mode", "depth"].contains(&k.as_str())) {
                    return Err(malformed(format!("`{key}` is neither `mode` nor `depth`")));
                }
                let mode = match o.get("mode") {
                    None => Mode::Native,
                    Some(Value::String(m)) => Mode::parse(m)?,
                    Some(other) => return Err(malformed(format!("`mode` is {other}, not text"))),
                };
                let depth = match o.get("depth") {
                    None => DEFAULT_NESTING_DEPTH,
                    Some(d) => d
                        .as_u64()
                        .and_then(|d| u32::try_from(d).ok())
                        .ok_or_else(|| malformed(format!("`depth` is {d}, not a level count")))?,
                };
                Normalize { mode, depth }
            }
            Some(other) => return Err(malformed(format!("{other} is neither a mode nor a table"))),
        })
    }

    /// The nested types this declaration lands on the store sink for `rows`: under
    /// `native`, [`native_types`] at its depth; under `relational`, none.
    pub fn store_types(&self, rows: &[Row]) -> Types {
        match self.mode {
            Mode::Native => native_types(rows, self.depth),
            Mode::Relational => Types::new(),
        }
    }
}

/// A relational batch keyed by destination table. Object fields use their path as a
/// column name; each list becomes an indexed child table with a parent reference.
pub fn relational_tables(rows: Vec<Row>, table: &str, load_id: &str, depth: u32) -> BTreeMap<String, Vec<Row>> {
    relational(rows, table, load_id, depth, false).0
}

/// Refuses a child table of `root` holding a row without `list_index`, naming the parent
/// table and the list the child shreds (`run.normalize.list-index-missing`). A child's
/// parent is the longest other table whose name and `_` prefix the child's name.
pub fn check_list_index(tables: &BTreeMap<String, Vec<Row>>, root: &str) -> Result<(), RunError> {
    for (child, rows) in tables.iter().filter(|(name, _)| name.as_str() != root) {
        if rows.iter().all(|row| row.contains_key("list_index")) {
            continue;
        }
        let parent = tables
            .keys()
            .filter(|p| p.as_str() != child && child.starts_with(&format!("{p}_")))
            .max_by_key(|p| p.len())
            .map_or(root, String::as_str);
        let list = child.strip_prefix(&format!("{parent}_")).unwrap_or(child);
        return Err(RunError::PipelineListIndexMissing(format!(
            "child table `{child}` of parent `{parent}` carries a row without `list_index`, so list `{list}` cannot be reassembled"
        )));
    }
    Ok(())
}

fn relational(rows: Vec<Row>, table: &str, load_id: &str, depth: u32, collect: bool) -> (BTreeMap<String, Vec<Row>>, Vec<NormalizedCell>) {
    let mut tables = BTreeMap::new();
    let mut cells = Vec::new();
    for (source_row, row) in rows.into_iter().enumerate() {
        let row_id = sha256_hex(&serde_json::to_vec(&row).expect("a JSON row serializes"));
        let mut flat = Row::new();
        flat.insert("row_id".into(), Value::String(row_id.clone()));
        flat.insert("load_id".into(), Value::String(load_id.into()));
        for (name, value) in row {
            project(&mut tables, &mut flat, table, &name, value, &row_id, &row_id, 1, depth, &mut cells, source_row, &name, &[], collect);
        }
        tables.entry(table.into()).or_default().push(flat);
    }
    (tables, cells)
}

#[allow(clippy::too_many_arguments)]
fn project(tables: &mut BTreeMap<String, Vec<Row>>, row: &mut Row, table: &str, path: &str, value: Value, parent_id: &str, root_id: &str, level: u32, depth: u32, cells: &mut Vec<NormalizedCell>, source_row: usize, source_column: &str, source_path: &[ValueStep], collect: bool) {
    if level > depth {
        let column = insert_projected(row, path, Value::String(value.to_string()));
        if collect { cells.push(NormalizedCell { table: table.into(), column, source_column: source_column.into(), path: source_path.into(), source_row }); }
        return;
    }
    match value {
        Value::Object(fields) => {
            for (name, value) in fields {
                let mut next = source_path.to_vec();
                next.push(ValueStep::Key(name.clone()));
                project(tables, row, table, &format!("{path}_{name}"), value, parent_id, root_id, level + 1, depth, cells, source_row, source_column, &next, collect);
            }
        }
        Value::Array(items) => {
            let child_table = format!("{table}_{path}");
            for (index, value) in items.into_iter().enumerate() {
                let mut next = source_path.to_vec();
                next.push(ValueStep::Index(index));
                let child_id = sha256_hex(&serde_json::to_vec(&(parent_id, index, &value)).expect("a JSON child serializes"));
                let mut child = Row::new();
                child.insert("row_id".into(), Value::String(child_id.clone()));
                child.insert("parent_id".into(), Value::String(parent_id.into()));
                child.insert("list_index".into(), Value::from(index));
                if parent_id != root_id {
                    child.insert("root_id".into(), Value::String(root_id.into()));
                }
                match value {
                    Value::Object(fields) => {
                        for (name, value) in fields {
                            let mut child_path = next.clone();
                            child_path.push(ValueStep::Key(name.clone()));
                            project(tables, &mut child, &child_table, &name, value, &child_id, root_id, level + 1, depth, cells, source_row, source_column, &child_path, collect);
                        }
                    }
                    other => project(tables, &mut child, &child_table, "value", other, &child_id, root_id, level + 1, depth, cells, source_row, source_column, &next, collect),
                }
                tables.entry(child_table.clone()).or_default().push(child);
            }
        }
        scalar => {
            let column = insert_projected(row, path, scalar);
            if collect { cells.push(NormalizedCell { table: table.into(), column, source_column: source_column.into(), path: source_path.into(), source_row }); }
        }
    }
}

fn insert_projected(row: &mut Row, path: &str, value: Value) -> String {
    let mut name = path.to_string();
    while row.contains_key(&name) {
        name = format!("source_{name}");
    }
    row.insert(name.clone(), value);
    name
}

/// The struct or list type of each column whose values include an object or an array,
/// inferred over every row (`run.normalize.native-store`). A container nested deeper than
/// `depth` levels types as `Json`; a column whose values meet no supertype, or a scalar
/// beside a container, is left out and lands as `Json` text.
pub fn native_types(rows: &[Row], depth: u32) -> Types {
    let mut seen: std::collections::BTreeMap<&str, Option<ColumnType>> =
        std::collections::BTreeMap::new();
    for row in rows {
        for (name, v) in row {
            let ty = infer(v, 1, depth);
            let slot = seen.entry(name.as_str()).or_insert(Some(ColumnType::Null));
            *slot = slot
                .as_ref()
                .and_then(|held| ty.and_then(|t| supertype(held, &t)));
        }
    }
    seen.into_iter()
        .filter_map(|(name, ty)| {
            ty.filter(ColumnType::is_nested)
                .map(|t| (name.to_string(), t))
        })
        .collect()
}

/// The type one JSON value carries at nesting `level`; `None` where its parts meet no
/// supertype. An empty object adds no field.
fn infer(v: &Value, level: u32, depth: u32) -> Option<ColumnType> {
    Some(match v {
        Value::Null => ColumnType::Null,
        Value::Bool(_) => ColumnType::Boolean,
        Value::Number(n) if n.is_i64() => ColumnType::Int64,
        Value::Number(_) => ColumnType::Float64,
        Value::String(_) => ColumnType::Utf8,
        Value::Array(_) | Value::Object(_) if level > depth => ColumnType::Json,
        Value::Array(items) => {
            let mut item = ColumnType::Null;
            for x in items {
                item = supertype(&item, &infer(x, level + 1, depth)?)?;
            }
            ColumnType::list(item)
        }
        Value::Object(o) if o.is_empty() => ColumnType::Null,
        Value::Object(o) => ColumnType::Struct(
            o.iter()
                .map(|(k, x)| Some(StructField::new(k, infer(x, level + 1, depth)?)))
                .collect::<Option<_>>()?,
        ),
    })
}
