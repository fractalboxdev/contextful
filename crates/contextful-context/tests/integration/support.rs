//! A scratch store, a landing helper, and DuckDB to execute the relation a scan resolves.

use contextful_context::land::{land, Batch, RunContext};
use contextful_context::scan::{scan, Scan};
use contextful_context::{Result, Store};
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::{NodeId, RunManifest};
use contextful_core::store::reconcile::ColumnType;
use contextful_core::store::reserve::Injection;
use contextful_core::time::Instant;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

pub fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}

pub fn decl(toml_block: &str) -> TableDecl {
    TableDecl::parse_pipeline(&format!("[[pipeline.tables]]\n{toml_block}\n")).unwrap().remove(0)
}

pub struct Fixture {
    _dir: tempfile::TempDir,
    pub store: Store,
}

impl Fixture {
    pub fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), "research").unwrap();
        Fixture { _dir: dir, store }
    }

    pub fn table_dir(&self, table: &str) -> PathBuf {
        self.store.table_dir(table).unwrap()
    }

    /// Land `rows` (a JSON array of objects) as run `run` from node `ingest-a`.
    pub fn land(&self, d: &TableDecl, run: &str, rows: Value, now: &str) -> Result<RunManifest> {
        self.land_on(d, run, "ingest-a", rows, now, &[])
    }

    pub fn land_typed(&self, d: &TableDecl, run: &str, rows: Value, now: &str, types: &[(&str, ColumnType)]) -> Result<RunManifest> {
        self.land_on(d, run, "ingest-a", rows, now, types)
    }

    pub fn land_on(
        &self,
        d: &TableDecl,
        run: &str,
        node: &str,
        rows: Value,
        now: &str,
        types: &[(&str, ColumnType)],
    ) -> Result<RunManifest> {
        let rows = rows.as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect();
        let batch = Batch { rows, types: types.iter().map(|(c, t)| (c.to_string(), *t)).collect::<HashMap<_, _>>() };
        let ctx = RunContext {
            node: NodeId::parse(node).unwrap(),
            injection: Injection { run_id: run.into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, taint: None },
            committed_at: at(now),
        };
        land(&self.store, d, &batch, &ctx)
    }

    pub fn scan(&self, d: &TableDecl, bounds: Bounds) -> Result<Scan> {
        scan(&self.store, d, bounds)
    }

    /// Execute `select` over the table's relation, bound as `t`; every column is read as text.
    pub fn query(&self, d: &TableDecl, bounds: Bounds, select: &str) -> Vec<Vec<Option<String>>> {
        let s = self.scan(d, bounds).unwrap();
        query(&format!("WITH t AS ({}) {select}", s.relation))
    }
}

/// Run a statement in an in-memory DuckDB, reading every column as text.
pub fn query(sql: &str) -> Vec<Vec<Option<String>>> {
    let conn = duckdb::Connection::open_in_memory().unwrap();
    let mut stmt = conn.prepare(sql).unwrap_or_else(|e| panic!("{e}\n{sql}"));
    let mut rows = stmt.query([]).unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().unwrap() {
        let n = row.as_ref().column_count();
        out.push((0..n).map(|i| text(row.get::<_, duckdb::types::Value>(i).unwrap())).collect());
    }
    out
}

fn text(v: duckdb::types::Value) -> Option<String> {
    use duckdb::types::Value::*;
    Some(match v {
        Null => return None,
        Boolean(b) => b.to_string(),
        TinyInt(n) => n.to_string(),
        SmallInt(n) => n.to_string(),
        Int(n) => n.to_string(),
        BigInt(n) => n.to_string(),
        HugeInt(n) => n.to_string(),
        UTinyInt(n) => n.to_string(),
        USmallInt(n) => n.to_string(),
        UInt(n) => n.to_string(),
        UBigInt(n) => n.to_string(),
        Float(n) => format!("{n:?}"),
        Double(n) => format!("{n:?}"),
        Text(s) => s,
        other => format!("{other:?}"),
    })
}

/// Shorthand for an expected text cell.
pub fn s(v: &str) -> Option<String> {
    Some(v.to_string())
}
