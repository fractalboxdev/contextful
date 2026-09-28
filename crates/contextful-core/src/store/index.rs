//! `store.index`: the sidecar declaration, the identifier column every sidecar of a table
//! shares, and the manifest entry a built vector sidecar records.

use super::declare::TableDecl;
use super::reconcile::{ColumnType, Schema};
use super::StoreError;
use serde::{Deserialize, Serialize};

/// The column naming the model that produced each row's vector (`read.embed.model-identifier`).
pub const EMBEDDING_MODEL_COLUMN: &str = "embedding_model";

/// The zone label of a sidecar indexing every row of its snapshot; the re-join through the
/// enforced relation applies each reader's zone (`store.index.paths`).
pub const ZONE_ALL: &str = "all";

/// The directory under a snapshot holding its sidecars.
pub const INDEXES_DIR: &str = "indexes";

/// Default neighbours per node on a graph layer above the base.
pub const DEFAULT_M: u32 = 16;

/// Default candidate-list width while inserting into the graph.
pub const DEFAULT_EF_CONSTRUCTION: u32 = 200;

/// The builder a vector sidecar records, with [`VECTOR_BUILDER_VERSION`] its identity
/// (`store.index.identity`).
pub const VECTOR_BUILDER: &str = "contextful-hnsw";

/// The on-disk graph format version the builder writes and the reader accepts.
pub const VECTOR_BUILDER_VERSION: u32 = 1;

/// A sidecar kind a declaration names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexKind {
    Vector,
}

/// A vector sidecar's distance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Metric {
    #[default]
    Cosine,
}

/// One `[[pipeline.tables.indexes]]` block (`store.index.declaration`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexDecl {
    pub kind: IndexKind,
    pub column: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_column: Option<String>,
    pub model: String,
    pub dim: u32,
    #[serde(default)]
    pub metric: Metric,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub m: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ef_construction: Option<u32>,
}

impl IndexDecl {
    /// Neighbours per node above the base layer; the base layer holds twice as many.
    pub fn m(&self) -> u32 {
        self.m.unwrap_or(DEFAULT_M).max(2)
    }

    pub fn ef_construction(&self) -> u32 {
        self.ef_construction.unwrap_or(DEFAULT_EF_CONSTRUCTION).max(self.m())
    }

    /// The sidecar's directory relative to its snapshot directory, `escape` rendering the
    /// column and model as path segments (`store.index.paths`).
    pub fn path(&self, escape: impl Fn(&str) -> String) -> String {
        format!("{INDEXES_DIR}/vec-{}-{}/zone={ZONE_ALL}", escape(&self.column), escape(&self.model))
    }
}

/// The entry a built vector sidecar records in its snapshot manifest's `indexes`, and
/// byte for byte in its own directory's manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VectorEntry {
    pub kind: IndexKind,
    /// The sidecar directory, relative to the snapshot directory.
    pub path: String,
    pub table: String,
    pub snapshot_id: String,
    pub column: String,
    pub id_column: String,
    pub model: String,
    pub dim: u32,
    pub metric: Metric,
    pub m: u32,
    pub ef_construction: u32,
    pub builder: String,
    pub builder_version: u32,
    /// Rows the graph holds.
    pub row_count: u64,
    /// The key version sealing the sidecar's files; 0 for plaintext.
    pub key_version: u32,
}

impl VectorEntry {
    /// The bytes of stored vectors the graph holds, the figure the read path's size cap
    /// compares (`read.retrieve.sidecar-size-cap`).
    pub fn stored_vector_bytes(&self) -> u64 {
        self.row_count.saturating_mul(u64::from(self.dim)).saturating_mul(4)
    }
}

impl TableDecl {
    /// The declared sidecars.
    pub fn indexes(&self) -> &[IndexDecl] {
        self.indexes.as_deref().unwrap_or(&[])
    }

    /// The one identifier column every sidecar of the table shares: each declaration's
    /// `id_column`, defaulting to a single-column primary key (`store.index.id-column`).
    /// `None` when the table declares no sidecar.
    pub fn id_column(&self) -> Result<Option<&str>, StoreError> {
        let default = match self.primary_key() {
            [k] => Some(k.as_str()),
            _ => None,
        };
        let mut resolved: Option<&str> = None;
        for idx in self.indexes() {
            let Some(id) = idx.id_column.as_deref().or(default) else {
                return Err(StoreError::StoreIndexIdColumnUnresolved(format!(
                    "table `{}`: the sidecar over `{}` names no `id_column`, and the table has no single-column primary key",
                    self.name, idx.column
                )));
            };
            match resolved {
                Some(r) if r != id => {
                    return Err(StoreError::StoreIndexIdColumnUnresolved(format!(
                        "table `{}`: its sidecars name `id_column` `{r}` and `{id}`; every sidecar of a table shares one",
                        self.name
                    )))
                }
                _ => resolved = Some(id),
            }
        }
        Ok(resolved)
    }

    /// Hold the sidecar declarations to the reconciled schema before the pass that builds
    /// them (`store.index.column-absent`, `store.index.column-type`).
    pub fn validate_indexes(&self, schema: &Schema) -> Result<(), StoreError> {
        let Some(id) = self.id_column()? else { return Ok(()) };
        match schema.get(id).map(|c| c.ty) {
            None => {
                return Err(StoreError::StoreIndexColumnAbsent(format!(
                    "table `{}`: `id_column` `{id}` is no column of the table",
                    self.name
                )))
            }
            Some(ColumnType::Utf8 | ColumnType::Int64 | ColumnType::Int32) => {}
            Some(ty) => {
                return Err(StoreError::StoreIndexColumnType(format!(
                    "table `{}`: `id_column` `{id}` is typed {}; an identifier is text or integer",
                    self.name,
                    ty.name()
                )))
            }
        }
        for idx in self.indexes() {
            match schema.get(&idx.column).map(|c| c.ty) {
                None => {
                    return Err(StoreError::StoreIndexColumnAbsent(format!(
                        "table `{}`: a vector sidecar indexes `{}`, which is no column of the table",
                        self.name, idx.column
                    )))
                }
                Some(ColumnType::FixedSizeList(_, n)) if n == idx.dim => {}
                Some(ty) => {
                    return Err(StoreError::StoreIndexColumnType(format!(
                        "table `{}`: the vector sidecar over `{}` declares dim {}, and the column is typed {}",
                        self.name,
                        idx.column,
                        idx.dim,
                        ty.name()
                    )))
                }
            }
        }
        Ok(())
    }
}
