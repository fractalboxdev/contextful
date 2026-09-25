//! A scan: the explicit sorted file list a read hands `read_parquet`, and the relation
//! the table registers as under the read's bounds.

use crate::error::Result;
use crate::parquet_io;
use crate::store::Store;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::reconcile::Column;
use contextful_core::store::relation::relation;
use serde_json::Value;
use std::collections::BTreeSet;

/// A resolved read of one table.
#[derive(Debug, Clone)]
pub struct Scan {
    /// Data files relative to the store root, sorted.
    pub files: Vec<String>,
    /// The table's FROM-source over the absolute file list.
    pub relation: String,
    /// `contextful.bounds`, absent for an unbounded read.
    pub bounds: Option<Value>,
}

/// Resolve `decl`'s table under `bounds`. The file list comes from the pointer and the
/// manifests alone, so a stray file, a staging directory and an uncommitted run join
/// nothing (`store.reconcile.explicit-file-list`).
pub fn scan(store: &Store, decl: &TableDecl, bounds: Bounds) -> Result<Scan> {
    let table = decl.name.as_str();
    let schema = store.schema(table)?;
    decl.validate(&schema)?;
    let state = store.state(decl)?;
    let resolution = state.resolve(bounds.as_of)?;
    let table_rel = format!("tables/{table}");
    let files: Vec<String> = resolution.files().into_iter().map(|f| format!("{table_rel}/{f}")).collect();
    let absolute: Vec<String> = files.iter().map(|f| store.root().join(f).to_string_lossy().into_owned()).collect();

    let mut carried = BTreeSet::new();
    for f in &absolute {
        carried.extend(parquet_io::columns(std::path::Path::new(f))?);
    }
    let absent: Vec<Column> = schema.columns.iter().filter(|c| !carried.contains(&c.name)).cloned().collect();
    let relation = relation(decl, &absolute, &schema.columns, &absent, bounds.valid_as_of)?;
    Ok(Scan { files, relation, bounds: bounds.echo() })
}
