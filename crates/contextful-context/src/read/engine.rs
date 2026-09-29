//! The embedded SQL engine a read executes on: in-memory connections a pooled session
//! reuses across statements, the session's relations registered as views, the
//! pepper-holding mask functions, and the statement serialization the guard walks.

use super::fault::ReadFault;
use contextful_core::read::respond::Cell;
use contextful_core::read::template::{Bindings, Bound};
use contextful_core::time::Instant;
use contextful_policy::enforce::mask::{Pepper, HASH_BYTES_FUNCTION, HASH_FUNCTION, TOKEN_FUNCTION};
use contextful_policy::enforce::session::{Session, TENANT_RELATION};
use contextful_core::store::relation::{ident, literal};
use duckdb::core::{DataChunkHandle, Inserter, LogicalTypeId};
use duckdb::ffi::duckdb_string_t;
use duckdb::types::{DuckString, TimeUnit, Value as Engine};
use duckdb::vscalar::{ScalarFunctionSignature, VScalar};
use duckdb::vtab::arrow::WritableVector;
use duckdb::{params_from_iter, Config, Connection};
use serde_json::Value;

/// The engine's name, as the internals block reports it.
pub const ENGINE: &str = "duckdb";

/// Apply `f` to the bytes of every non-null text or blob cell of the first input column;
/// the engine lays both out as one string type.
fn map_bytes(input: &mut DataChunkHandle, output: &mut dyn WritableVector, f: impl Fn(&[u8]) -> String) {
    let n = input.len();
    let column = input.flat_vector(0);
    let cells = unsafe { column.as_slice_with_len::<duckdb_string_t>(n) };
    let mut out = output.flat_vector();
    for (i, cell) in cells.iter().enumerate().take(n) {
        if column.row_is_null(i as u64) {
            out.set_null(i);
        } else {
            let bytes = DuckString::new(&mut { *cell }).as_bytes().to_vec();
            out.insert(i, f(&bytes).as_str());
        }
    }
}

/// Apply `f` to every non-null text cell of the first input column.
fn map_text(input: &mut DataChunkHandle, output: &mut dyn WritableVector, f: impl Fn(&str) -> String) {
    map_bytes(input, output, |b| f(&String::from_utf8_lossy(b)))
}

fn text_to_text() -> Vec<ScalarFunctionSignature> {
    vec![ScalarFunctionSignature::exact(vec![LogicalTypeId::Varchar.into()], LogicalTypeId::Varchar.into())]
}

/// The keyed digest, computed in process with the pepper held as the function's state.
struct MaskHash;

impl VScalar for MaskHash {
    type State = Pepper;
    fn invoke(pepper: &Pepper, input: &mut DataChunkHandle, output: &mut dyn WritableVector) -> Result<(), Box<dyn std::error::Error>> {
        map_text(input, output, |v| pepper.digest(v));
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        text_to_text()
    }
}

/// The keyed digest over a binary column's bytes, the pepper held as the function's state.
struct MaskHashBytes;

impl VScalar for MaskHashBytes {
    type State = Pepper;
    fn invoke(pepper: &Pepper, input: &mut DataChunkHandle, output: &mut dyn WritableVector) -> Result<(), Box<dyn std::error::Error>> {
        map_bytes(input, output, |v| pepper.digest_bytes(v));
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(vec![LogicalTypeId::Blob.into()], LogicalTypeId::Varchar.into())]
    }
}

/// The token, computed in process with the pepper held as the function's state.
struct MaskToken;

impl VScalar for MaskToken {
    type State = Pepper;
    fn invoke(pepper: &Pepper, input: &mut DataChunkHandle, output: &mut dyn WritableVector) -> Result<(), Box<dyn std::error::Error>> {
        map_text(input, output, |v| pepper.token(v));
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        text_to_text()
    }
}

/// One connection of the embedded engine.
pub struct SqlEngine {
    conn: Connection,
}

fn fault(e: duckdb::Error) -> ReadFault {
    ReadFault::Engine(e.to_string())
}

impl SqlEngine {
    /// A connection loading no extension, installing none, with no relation registered.
    fn connect() -> Result<SqlEngine, ReadFault> {
        let config = Config::default()
            .enable_autoload_extension(false)
            .and_then(|c| c.with("autoinstall_known_extensions", "false"))
            .map_err(fault)?;
        Ok(SqlEngine { conn: Connection::open_in_memory_with_flags(config).map_err(fault)? })
    }

    /// Close the connection to everything outside it but `files`: no file, extension or
    /// other external state is reachable from SQL — a replacement scan included — and the
    /// configuration locks so no statement reopens it.
    fn lock(&self, files: &[String]) -> Result<(), ReadFault> {
        let allowed: Vec<String> = files.iter().map(|f| literal(f)).collect();
        self.conn
            .execute_batch(&format!(
                "SET allowed_paths = [{}]; SET enable_external_access = false; SET lock_configuration = true;",
                allowed.join(", ")
            ))
            .map_err(fault)
    }

    /// A locked connection with no relation registered, for serialization alone.
    pub fn bare() -> Result<SqlEngine, ReadFault> {
        let engine = SqlEngine::connect()?;
        engine.lock(&[])?;
        Ok(engine)
    }

    /// An unlocked connection for operator text, which runs raw
    /// (`read.guard.statement-provenance`): local files, table functions and explicit
    /// `INSTALL`/`LOAD` stay reachable, and no extension autoinstalls or autoloads.
    pub fn raw() -> Result<SqlEngine, ReadFault> {
        SqlEngine::connect()
    }

    /// The number of statements the engine's parser extracts from `sql`, found on a
    /// private connection that runs none of them. Preparing multi-statement text on a
    /// connection executes every statement but the last, so the count precedes any run.
    pub fn statement_count(sql: &str) -> Result<u64, ReadFault> {
        use duckdb::ffi;
        let text = std::ffi::CString::new(sql).map_err(|e| ReadFault::Engine(format!("the statement holds a NUL byte: {e}")))?;
        let mut db: ffi::duckdb_database = std::ptr::null_mut();
        let mut con: ffi::duckdb_connection = std::ptr::null_mut();
        let mut extracted: ffi::duckdb_extracted_statements = std::ptr::null_mut();
        // SAFETY: each handle is created here, checked before use, and released once below
        // whether or not the calls that follow it succeed.
        unsafe {
            if ffi::duckdb_open(std::ptr::null(), &mut db) != ffi::DuckDBSuccess {
                ffi::duckdb_close(&mut db);
                return Err(ReadFault::Engine("opening the statement parser failed".into()));
            }
            if ffi::duckdb_connect(db, &mut con) != ffi::DuckDBSuccess {
                ffi::duckdb_disconnect(&mut con);
                ffi::duckdb_close(&mut db);
                return Err(ReadFault::Engine("connecting the statement parser failed".into()));
            }
            let count = ffi::duckdb_extract_statements(con, text.as_ptr(), &mut extracted);
            let error = if count == 0 {
                let e = ffi::duckdb_extract_statements_error(extracted);
                (!e.is_null()).then(|| std::ffi::CStr::from_ptr(e).to_string_lossy().into_owned())
            } else {
                None
            };
            ffi::duckdb_destroy_extracted(&mut extracted);
            ffi::duckdb_disconnect(&mut con);
            ffi::duckdb_close(&mut db);
            match error {
                Some(e) => Err(ReadFault::Engine(e)),
                None => Ok(count),
            }
        }
    }

    /// A connection for one session: the mask functions holding the pepper, the subject
    /// and tenant relations filled through parameters, and one create-or-replace view per
    /// registered relation. No view directory exists on disk
    /// (`read.register.connection-views`). Every file a view names is immutable under the
    /// pool key the connection serves, so the connection keeps Parquet footers it read.
    pub fn open(session: &Session) -> Result<SqlEngine, ReadFault> {
        let engine = SqlEngine::connect()?;
        let conn = &engine.conn;
        conn.execute_batch("SET parquet_metadata_cache = true").map_err(fault)?;
        conn.register_scalar_function_with_state::<MaskHash>(HASH_FUNCTION, session.pepper()).map_err(fault)?;
        conn.register_scalar_function_with_state::<MaskHashBytes>(HASH_BYTES_FUNCTION, session.pepper()).map_err(fault)?;
        conn.register_scalar_function_with_state::<MaskToken>(TOKEN_FUNCTION, session.pepper()).map_err(fault)?;

        let subject = session.subject_row();
        let columns: Vec<String> = subject.iter().map(|(f, _)| format!("{} VARCHAR", ident(f))).collect();
        conn.execute_batch(&format!("CREATE TEMP TABLE {} ({})", ident(session.subject_relation()), columns.join(", ")))
            .map_err(fault)?;
        let marks = vec!["?"; subject.len()].join(", ");
        conn.execute(
            &format!("INSERT INTO {} VALUES ({marks})", ident(session.subject_relation())),
            params_from_iter(subject.iter().map(|(_, v)| v.clone())),
        )
        .map_err(fault)?;

        conn.execute_batch(&format!("CREATE TEMP TABLE {} (\"table\" VARCHAR, \"value\" VARCHAR)", ident(TENANT_RELATION)))
            .map_err(fault)?;
        for (table, value) in session.tenant_rows() {
            conn.execute(&format!("INSERT INTO {} VALUES (?, ?)", ident(TENANT_RELATION)), [table, value]).map_err(fault)?;
        }

        // A request ledger registers only once a statement names it, so one unreadable
        // ledger file fails that statement alone.
        for r in session.relations() {
            conn.execute_batch(&format!("CREATE OR REPLACE TEMP VIEW {} AS {}", ident(r.name()), r.sql())).map_err(fault)?;
        }
        let files: Vec<String> = session.relations().chain(session.ledgers()).flat_map(|r| r.files().iter().cloned()).collect();
        engine.lock(&files)?;
        Ok(engine)
    }

    /// Register the request ledgers among `names`, the relations an admitted statement
    /// names; the connection already admits their files.
    pub fn register_ledgers(&self, session: &Session, names: &std::collections::BTreeSet<String>) -> Result<(), ReadFault> {
        for l in session.ledgers_named(names)? {
            self.register(l.name(), l.sql())?;
        }
        Ok(())
    }

    /// Register one more relation on an open connection under `name`, reading only files
    /// the connection already admits.
    pub fn register(&self, name: &str, sql: &str) -> Result<(), ReadFault> {
        self.conn.execute_batch(&format!("CREATE OR REPLACE TEMP VIEW {} AS {sql}", ident(name))).map_err(fault)
    }

    /// The engine's own serialization of `sql`, which the guard walks
    /// (`read.guard.engine-own-parse`).
    pub fn serialize(&self, sql: &str) -> Result<Value, ReadFault> {
        let text: String = self.conn.query_row("SELECT json_serialize_sql(CAST(? AS VARCHAR))", [sql], |r| r.get(0)).map_err(fault)?;
        serde_json::from_str(&text).map_err(|e| ReadFault::Engine(format!("the engine's serialization does not parse: {e}")))
    }

    /// Run `sql` with its placeholders bound, reading at most `fetch` rows as typed cells.
    pub fn run(&self, sql: &str, parameters: &Bindings, fetch: Option<u64>) -> Result<(Vec<String>, Vec<Vec<Cell>>), ReadFault> {
        let (columns, rows) = self.run_values(sql, parameters, fetch)?;
        Ok((columns, rows.into_iter().map(|r| r.into_iter().map(cell).collect()).collect()))
    }

    /// Run `sql`, reading at most `fetch` rows as the engine's own values. Each
    /// placeholder takes the value bound under the identifier the engine names it by.
    pub(crate) fn run_values(&self, sql: &str, parameters: &Bindings, fetch: Option<u64>) -> Result<(Vec<String>, Vec<Vec<Engine>>), ReadFault> {
        let mut stmt = self.conn.prepare(sql).map_err(fault)?;
        let values: Vec<Engine> = (1..=stmt.parameter_count())
            .map(|i| {
                let name = stmt.parameter_name(i).map_err(fault)?;
                parameters.get(&name).map(bound).ok_or_else(|| ReadFault::Engine(format!("placeholder `{name}` carries no bound value")))
            })
            .collect::<Result<_, _>>()?;
        let mut rows = stmt.query(params_from_iter(values)).map_err(fault)?;
        let mut out = Vec::new();
        let mut columns = Vec::new();
        while fetch.is_none_or(|f| (out.len() as u64) < f) {
            let Some(row) = rows.next().map_err(fault)? else { break };
            let stmt = row.as_ref();
            if columns.is_empty() {
                columns = stmt.column_names();
            }
            let n = stmt.column_count();
            out.push((0..n).map(|i| row.get::<_, Engine>(i)).collect::<Result<_, _>>().map_err(fault)?);
        }
        if columns.is_empty() {
            columns = rows.as_ref().map(|s| s.column_names()).unwrap_or_default();
        }
        Ok((columns, out))
    }
}

fn bound(b: &Bound) -> Engine {
    match b {
        Bound::Integer(n) => Engine::BigInt(*n),
        Bound::Float(f) => Engine::Double(*f),
        Bound::Text(s) => Engine::Text(s.clone()),
        Bound::Timestamp(t) => Engine::Timestamp(TimeUnit::Microsecond, (t.unix_nanos() / 1000) as i64),
        Bound::Boolean(b) => Engine::Boolean(*b),
    }
}

fn nanos(unit: TimeUnit, v: i64) -> i128 {
    let v = i128::from(v);
    match unit {
        TimeUnit::Second => v * 1_000_000_000,
        TimeUnit::Millisecond => v * 1_000_000,
        TimeUnit::Microsecond => v * 1_000,
        TimeUnit::Nanosecond => v,
    }
}

/// An engine value as a typed cell.
pub fn cell(v: Engine) -> Cell {
    match v {
        Engine::Null => Cell::Null,
        Engine::Boolean(b) => Cell::Boolean(b),
        Engine::TinyInt(n) => Cell::Integer { value: n.into(), bits: 8 },
        Engine::SmallInt(n) => Cell::Integer { value: n.into(), bits: 16 },
        Engine::Int(n) => Cell::Integer { value: n.into(), bits: 32 },
        Engine::BigInt(n) => Cell::Integer { value: n.into(), bits: 64 },
        Engine::HugeInt(n) => Cell::Integer { value: n, bits: 128 },
        Engine::UTinyInt(n) => Cell::Integer { value: n.into(), bits: 8 },
        Engine::USmallInt(n) => Cell::Integer { value: n.into(), bits: 16 },
        Engine::UInt(n) => Cell::Integer { value: n.into(), bits: 32 },
        Engine::UBigInt(n) => Cell::Integer { value: n.into(), bits: 64 },
        Engine::UHugeInt(n) => Cell::UnsignedHuge(n),
        Engine::Float(f) => Cell::Float(f.into()),
        Engine::Double(f) => Cell::Float(f),
        Engine::Decimal(d) => Cell::Decimal { value: d.value(), width: d.width(), scale: d.scale() },
        Engine::Timestamp(unit, v) => match Instant::from_unix_nanos(nanos(unit, v)) {
            Ok(t) => Cell::Timestamp(t),
            Err(_) => Cell::Text(v.to_string()),
        },
        Engine::Text(s) => Cell::Text(s),
        Engine::Blob(b) | Engine::Geometry(b) => Cell::Blob(b),
        Engine::Date32(d) => Cell::Date(d),
        Engine::Time64(unit, v) => Cell::Time((nanos(unit, v) / 1000) as i64),
        Engine::Interval { months, days, nanos } => Cell::Interval { months, days, nanos },
        Engine::Enum(s) => Cell::Enum(s),
        // A fixed-size array of non-null floats, a vector column among them, is a number array.
        Engine::Array(items) => {
            let floats: Option<Vec<f64>> = items
                .iter()
                .map(|v| match v {
                    Engine::Float(f) => Some(f64::from(*f)),
                    Engine::Double(f) => Some(*f),
                    _ => None,
                })
                .collect();
            match floats {
                Some(v) => Cell::Vector(v),
                None => Cell::Container(container_text(&Engine::Array(items))),
            }
        }
        other => Cell::Container(container_text(&other)),
    }
}

/// A container's text form, in the engine's own spelling.
fn container_text(v: &Engine) -> String {
    match v {
        Engine::List(items) | Engine::Array(items) => format!("[{}]", items.iter().map(container_text).collect::<Vec<_>>().join(", ")),
        Engine::Struct(fields) => format!(
            "{{{}}}",
            fields.iter().map(|(k, v)| format!("'{k}': {}", container_text(v))).collect::<Vec<_>>().join(", ")
        ),
        Engine::Map(entries) => format!(
            "{{{}}}",
            entries.iter().map(|(k, v)| format!("{}={}", container_text(k), container_text(v))).collect::<Vec<_>>().join(", ")
        ),
        Engine::Union(inner) => container_text(inner),
        Engine::Null => "NULL".into(),
        Engine::Text(s) | Engine::Enum(s) => s.clone(),
        scalar => match cell(scalar.clone()).to_json() {
            Value::String(s) => s,
            other => other.to_string(),
        },
    }
}
