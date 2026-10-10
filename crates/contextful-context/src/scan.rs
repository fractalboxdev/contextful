//! A scan: the explicit sorted file list a read hands `read_parquet`, and the relation
//! the table registers as under the read's bounds.

use crate::error::Result;
use crate::store::Store;
use contextful_core::pipeline::model::PublishSection;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::SnapshotManifest;
use contextful_core::store::reconcile::{Column, Schema};
use contextful_core::store::relation::relation_with_encryption;
use contextful_core::store::resolve::Resolution;
use serde_json::Value;
use std::collections::BTreeSet;

/// A resolved read of one table.
#[derive(Debug, Clone)]
pub struct Scan {
    /// Data files relative to the store root, sorted.
    pub files: Vec<String>,
    /// The table's FROM-source over the absolute file list.
    pub relation: String,
    /// The columns the relation projects: a pinned build's own, else the table's schema.
    pub columns: Vec<Column>,
    /// `contextful.bounds`, absent for an unbounded read.
    pub bounds: Option<Value>,
    /// The publish section of the build the relation reads, where it reads one published
    /// snapshot and no run beside it.
    pub publish: Option<PublishSection>,
}

/// Resolve `decl`'s table under `bounds`. The file list comes from the pointer and the
/// manifests alone, so a stray file, a staging directory and an uncommitted run join
/// nothing (`store.reconcile.explicit-file-list`).
pub fn scan(store: &Store, decl: &TableDecl, bounds: Bounds) -> Result<Scan> {
    scan_at(store, decl, bounds, None)
}

/// Resolve `decl`'s table to `snapshot`'s parts alone where one is given — a pinned
/// build's committed manifest (`read.resolve-pin.pin-parameter`), read under the columns
/// its own parts carry — and under `bounds` otherwise; `valid_as_of` wraps the relation
/// either way.
pub fn scan_at(store: &Store, decl: &TableDecl, bounds: Bounds, snapshot: Option<&SnapshotManifest>) -> Result<Scan> {
    let table = decl.name.as_str();
    let state;
    let resolution = match snapshot {
        Some(s) => Resolution { snapshot: Some(s), runs: Vec::new() },
        None => {
            state = store.state(decl)?;
            state.resolve(bounds.as_of)?
        }
    };
    // A replica reads only the parts it advertises (`store.replicate.missing-index`).
    if let Some(s) = resolution.snapshot {
        crate::replica::require(store, table, s, &[])?;
    }
    let table_rel = format!("tables/{table}");
    let files: Vec<String> = resolution.files().into_iter().map(|f| format!("{table_rel}/{f}")).collect();
    let absolute: Vec<String> = files.iter().map(|f| store.logical_path(f).map(|path| path.to_string_lossy().into_owned())).collect::<Result<_>>()?;
    let publish = resolution.snapshot.filter(|_| resolution.runs.is_empty()).and_then(|s| s.publish.clone());

    let schema = match snapshot {
        Some(_) if !absolute.is_empty() => {
            // A build writes every part under its contract's columns, so the parts carry
            // the schema the build published, whatever a later build moved it to.
            let mut columns: Vec<Column> = Vec::new();
            for f in &absolute {
                for c in store.parquet_schema(std::path::Path::new(f))? {
                    if !columns.iter().any(|k| k.name == c.name) {
                        columns.push(c);
                    }
                }
            }
            Schema { columns }
        }
        _ => store.schema(table)?,
    };
    decl.validate(&schema)?;

    let mut carried = BTreeSet::new();
    for f in &absolute {
        carried.extend(store.parquet_columns(std::path::Path::new(f))?);
    }
    let absent: Vec<Column> = schema.columns.iter().filter(|c| !carried.contains(&c.name)).cloned().collect();
    let key_name = store.parquet_key().map(|_| crate::encrypt::PARQUET_KEY_NAME);
    let relation = relation_with_encryption(decl, &absolute, &schema.columns, &absent, bounds.valid_as_of, key_name)?;
    Ok(Scan { files, relation, columns: schema.columns, bounds: bounds.echo(), publish })
}
