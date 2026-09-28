//! The store's two catalogs as ports (`store.lay-out.catalog-ports`). `machine.sqlite`
//! holds the rows behind [`crate::coordinate::Catalog`]; `derived.sqlite` holds the rows
//! behind [`DerivedCatalog`], each reconstructible from the canonical tree
//! (`store.lay-out.derived-catalog`). An adapter package implements both; code above the
//! ports names no SQLite type.

use crate::run::failure::Failure;
use crate::time::Instant;
use serde::{Deserialize, Serialize};

/// The file name of the rebuildable catalog under a store root.
pub const DERIVED_CATALOG_FILE: &str = "derived.sqlite";
/// The file name of the machine-local catalog under a store root.
pub const MACHINE_CATALOG_FILE: &str = "machine.sqlite";

/// One table a `schema.json` declares, with the snapshot its pointer names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedTable {
    pub table: String,
    /// The merged schema, in Arrow JSON form.
    pub schema: String,
    /// The snapshot `_pointer.json` names; `None` before the first fold.
    pub snapshot_id: Option<String>,
    /// The fence that published the pointer.
    pub fence: Option<u64>,
}

/// One snapshot the pointer reaches, directly or through `parent` links.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedSnapshot {
    pub table: String,
    pub snapshot_id: String,
    pub parent: Option<String>,
    pub created_at: Instant,
    pub row_count: u64,
    pub parts: u64,
}

/// One committed run manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedRun {
    pub table: String,
    pub run_id: String,
    pub node_id: String,
    pub committed_at: Instant,
    pub parts: u64,
    pub pipeline_id: Option<String>,
}

/// Every row `derived.sqlite` holds, each list in the catalog's order: tables by name,
/// snapshots by table then id, runs by table, run id and node id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedRows {
    pub tables: Vec<DerivedTable>,
    pub snapshots: Vec<DerivedSnapshot>,
    pub runs: Vec<DerivedRun>,
}

impl DerivedRows {
    /// The rows in the catalog's order, so two rebuilds of one tree compare equal.
    pub fn sorted(mut self) -> DerivedRows {
        self.tables.sort_by(|a, b| a.table.cmp(&b.table));
        self.snapshots.sort_by(|a, b| (&a.table, &a.snapshot_id).cmp(&(&b.table, &b.snapshot_id)));
        self.runs.sort_by(|a, b| (&a.table, &a.run_id, &a.node_id).cmp(&(&b.table, &b.run_id, &b.node_id)));
        self
    }
}

/// The `derived.sqlite` port: a cache the canonical tree reconstructs, which commits
/// nothing.
pub trait DerivedCatalog {
    /// Replace every row with `rows` in one transaction: a reader sees the old set or the
    /// new one, never a mix.
    fn replace(&self, rows: &DerivedRows) -> Result<(), Failure>;
    /// Every row, in the catalog's order.
    fn rows(&self) -> Result<DerivedRows, Failure>;
}
