//! The vector sidecar: the fold builds one per declaration into the staging directory, so
//! the snapshot's one rename commits data and sidecar together, and the read path opens
//! it to generate candidate `id_column` values (`store.index.vector-by-fold`,
//! `store.index.candidate-ids`).
//!
//! A sidecar directory holds `_manifest.json`, byte-equal to the snapshot manifest's entry,
//! and `graph.bin`, the laid-out graph with its identifiers. In an unencrypted project both
//! are plaintext and the reader memory-maps the graph read-only; in an encrypted one both
//! are sealed per file, and the reader decrypts them into process memory and writes no
//! cleartext to disk (`store.encrypt.sidecar-reader`).

pub mod graph;

use crate::error::{ContextError, IoPath, Result};
use arrow_array::cast::AsArray;
use arrow_array::types::{Float16Type, Float32Type};
use arrow_array::{Array, RecordBatch, StringArray};
use arrow_schema::DataType;
use contextful_core::read::rank::sidecar_over_cap;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::encrypt::FileCipher;
use contextful_core::store::index::{
    IndexDecl, IndexEntry, IndexKind, VectorEntry, EMBEDDING_MODEL_COLUMN, VECTOR_BUILDER, VECTOR_BUILDER_VERSION,
};
use contextful_core::store::lay_out::{SnapshotId, MANIFEST_FILE};
use contextful_core::store::StoreError;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path};

/// The laid-out graph and its identifiers, inside a sidecar directory.
pub const GRAPH_FILE: &str = "graph.bin";

/// Base-layer search width floor. A probe of k candidates searches at least this wide.
pub const EF_SEARCH_FLOOR: usize = 128;

/// How a project's sidecar files sit on disk.
#[derive(Clone, Copy)]
pub enum Sealing<'a> {
    /// An unencrypted project: files are plaintext and a reader maps them.
    Plaintext,
    /// An encrypted project: every file is sealed by the cipher, and a reader opens it
    /// into process memory.
    Sealed(&'a dyn FileCipher),
}

impl Sealing<'_> {
    pub(crate) fn write(&self, path: &Path, plaintext: &[u8]) -> Result<()> {
        let bytes = match self {
            Sealing::Plaintext => plaintext.to_vec(),
            Sealing::Sealed(c) => c.seal(plaintext).map_err(|e| ContextError::Invalid(format!("{}: sealing: {e}", path.display())))?,
        };
        fs::write(path, bytes).at(path)
    }

    pub(crate) fn key_version(&self) -> u32 {
        match self {
            Sealing::Plaintext => 0,
            Sealing::Sealed(c) => c.key_version(),
        }
    }
}

/// The identifier column's values as text, `None` for a null.
pub(crate) fn identifiers(rows: &RecordBatch, table: &str, column: &str) -> Result<Vec<Option<String>>> {
    let col = rows
        .column_by_name(column)
        .ok_or_else(|| StoreError::StoreIndexColumnAbsent(format!("table `{table}`: `id_column` `{column}` is no column of the staged rows")))?;
    let text = arrow_cast::cast(col, &DataType::Utf8)
        .map_err(|e| StoreError::StoreIndexColumnType(format!("table `{table}`: `id_column` `{column}`: {e}")))?;
    let text = text.as_any().downcast_ref::<StringArray>().expect("cast to Utf8");
    Ok((0..text.len()).map(|i| (!text.is_null(i)).then(|| text.value(i).to_string())).collect())
}

/// Refuse a staged row set in which one identifier value names two rows
/// (`store.index.id-unique`).
pub fn check_identifiers(rows: &RecordBatch, decl: &TableDecl) -> Result<()> {
    let Some(column) = decl.id_column()? else { return Ok(()) };
    let mut seen = HashSet::new();
    for id in identifiers(rows, &decl.name, column)?.into_iter().flatten() {
        if !seen.insert(id.clone()) {
            return Err(StoreError::StoreIndexIdNotUnique(format!(
                "table `{}`: `id_column` `{column}` holds `{id}` on two rows of the snapshot; nothing was published",
                decl.name
            ))
            .into());
        }
    }
    Ok(())
}

/// The seed a sidecar's layers draw from: the snapshot id and column, so one staged row set
/// builds one byte-identical graph (`store.index.graph`).
fn seed(snapshot_id: &SnapshotId, column: &str) -> u64 {
    let d = Sha256::digest(format!("{snapshot_id}\u{1f}{column}").as_bytes());
    u64::from_le_bytes(d[..8].try_into().expect("8 bytes"))
}

/// Build the vector sidecar `index` declares over the staged `rows`, writing it under
/// `snapshot_dir` and returning the entry its snapshot manifest records. The graph holds
/// each row with a non-null identifier and a non-zero vector, from the declared model
/// where the rows carry `embedding_model` (`store.index.vector-by-fold`).
pub fn build(
    snapshot_dir: &Path,
    snapshot_id: &SnapshotId,
    rows: &RecordBatch,
    decl: &TableDecl,
    index: &IndexDecl,
    sealing: &Sealing<'_>,
) -> Result<VectorEntry> {
    let table = decl.name.as_str();
    let id_column = decl
        .id_column()?
        .ok_or_else(|| StoreError::StoreIndexIdColumnUnresolved(format!("table `{table}` resolves no `id_column`")))?
        .to_string();
    let dim = index.dim() as usize;
    let ids = identifiers(rows, table, &id_column)?;
    let vectors = rows
        .column_by_name(&index.column)
        .ok_or_else(|| StoreError::StoreIndexColumnAbsent(format!("table `{table}`: `{}` is no column of the staged rows", index.column)))?;
    let wrong_type = || StoreError::StoreIndexColumnType(format!("table `{table}`: `{}` is not a vector of dim {dim}", index.column));
    let list = vectors.as_fixed_size_list_opt().filter(|l| l.value_length() as usize == dim).ok_or_else(wrong_type)?;
    let values: Vec<f32> = match list.values().data_type() {
        DataType::Float32 => list.values().as_primitive::<Float32Type>().values().to_vec(),
        DataType::Float16 => list.values().as_primitive::<Float16Type>().values().iter().map(|h| h.to_f32()).collect(),
        _ => return Err(wrong_type().into()),
    };
    let models = rows.column_by_name(EMBEDDING_MODEL_COLUMN).and_then(|c| arrow_cast::cast(c, &DataType::Utf8).ok());
    let models = models.as_ref().and_then(|c| c.as_any().downcast_ref::<StringArray>());

    let mut kept_ids = Vec::new();
    let mut kept = Vec::new();
    for (i, id) in ids.into_iter().enumerate() {
        let Some(id) = id else { continue };
        if list.is_null(i) || models.is_some_and(|m| m.is_null(i) || m.value(i) != index.model()) {
            continue;
        }
        let v = &values[i * dim..(i + 1) * dim];
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if !norm.is_finite() || norm == 0.0 {
            continue;
        }
        kept.extend(v.iter().map(|x| x / norm));
        kept_ids.push(id);
    }
    let bytes = graph::build(&kept, dim, &kept_ids, index.m() as usize, index.ef_construction() as usize, seed(snapshot_id, &index.column));

    let path = index.path(crate::fold::escape);
    let dir = snapshot_dir.join(&path);
    fs::create_dir_all(&dir).at(&dir)?;
    let entry = VectorEntry {
        kind: IndexKind::Vector,
        path,
        table: table.to_string(),
        snapshot_id: snapshot_id.to_string(),
        column: index.column.clone(),
        id_column,
        model: index.model().to_string(),
        dim: index.dim(),
        metric: index.metric(),
        m: index.m(),
        ef_construction: index.ef_construction(),
        builder: VECTOR_BUILDER.to_string(),
        builder_version: VECTOR_BUILDER_VERSION,
        // Every build lays the graph out whole from the staged rows
        // (`store.index.graph-extensions`).
        extensions: 0,
        row_count: kept_ids.len() as u64,
        key_version: sealing.key_version(),
    };
    sealing.write(&dir.join(GRAPH_FILE), &bytes)?;
    sealing.write(&dir.join(MANIFEST_FILE), &manifest_bytes(&entry))?;
    Ok(entry)
}

fn manifest_bytes(entry: &VectorEntry) -> Vec<u8> {
    serde_json::to_vec_pretty(entry).expect("an entry serializes")
}

/// Why a read takes the exact scan instead of a sidecar (`read.retrieve.sidecar-falls-back`,
/// `read.retrieve.sidecar-size-cap`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fallback {
    /// The table has no published snapshot.
    NoSnapshot,
    /// The snapshot records no sidecar over the column at the query's dimension.
    NoSidecar,
    /// The reader's policy masks or zone-withholds the identifier or the indexed column, or
    /// classes an indexed text column.
    Withheld,
    /// The query vector's dimension differs from the sidecar's.
    DimensionMismatch,
    /// The sidecar's own manifest, table or identity disagrees with the snapshot's entry.
    ManifestMismatch,
    /// The sidecar holds more stored-vector bytes, or a sealed full-text file more bytes,
    /// than the read path loads.
    OverCap,
    /// A sidecar file is missing, fails to open or decrypt, or holds no laid-out graph.
    Unreadable,
}

/// A sidecar file's bytes: mapped read-only from a plaintext file, or decrypted into
/// process memory from a sealed one.
pub(crate) enum Backing {
    Mapped(memmap2::Mmap),
    Owned(Vec<u8>),
}

impl Backing {
    pub(crate) fn bytes(&self) -> &[u8] {
        match self {
            Backing::Mapped(m) => m,
            Backing::Owned(v) => v,
        }
    }
}

/// An opened vector sidecar.
pub struct VectorSidecar {
    entry: VectorEntry,
    backing: Backing,
    layout: graph::Layout,
}

/// A candidate a probe returns: an `id_column` value and its cosine similarity.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: String,
    pub similarity: f32,
}

impl VectorSidecar {
    /// Open the sidecar a snapshot manifest's `entry` records, under `snapshot_dir`. Every
    /// precondition failure is a [`Fallback`], never an error: the sidecar accelerates a
    /// read and decides none.
    pub fn open(snapshot_dir: &Path, table: &str, entry: &IndexEntry, sealing: &Sealing<'_>) -> std::result::Result<VectorSidecar, Fallback> {
        let IndexEntry::Vector(entry) = entry else { return Err(Fallback::ManifestMismatch) };
        let entry = entry.clone();
        if entry.table != table || entry.builder != VECTOR_BUILDER || entry.builder_version != VECTOR_BUILDER_VERSION {
            return Err(Fallback::ManifestMismatch);
        }
        // Checked before any sidecar byte is read, so an oversized graph is never loaded to
        // learn it is oversized.
        if sidecar_over_cap(entry.stored_vector_bytes()) {
            return Err(Fallback::OverCap);
        }
        // A manifest path that leaves the snapshot directory names no sidecar of it.
        let rel = Path::new(&entry.path);
        if rel.as_os_str().is_empty() || rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            return Err(Fallback::ManifestMismatch);
        }
        let dir = snapshot_dir.join(rel);
        let read = |name: &str| -> std::result::Result<Vec<u8>, Fallback> {
            let raw = fs::read(dir.join(name)).map_err(|_| Fallback::Unreadable)?;
            match sealing {
                Sealing::Plaintext => Ok(raw),
                Sealing::Sealed(c) => c.open(&raw).map_err(|_| Fallback::Unreadable),
            }
        };
        let own: VectorEntry = serde_json::from_slice(&read(MANIFEST_FILE)?).map_err(|_| Fallback::Unreadable)?;
        if own != entry {
            return Err(Fallback::ManifestMismatch);
        }
        let backing = match (sealing, entry.key_version) {
            (Sealing::Plaintext, 0) => {
                let file = fs::File::open(dir.join(GRAPH_FILE)).map_err(|_| Fallback::Unreadable)?;
                // SAFETY: the file sits in a published snapshot directory, which no writer
                // rewrites; a pass stages a new snapshot beside it instead.
                Backing::Mapped(unsafe { memmap2::Mmap::map(&file) }.map_err(|_| Fallback::Unreadable)?)
            }
            (Sealing::Sealed(_), v) if v > 0 => Backing::Owned(read(GRAPH_FILE)?),
            _ => return Err(Fallback::Unreadable),
        };
        let layout = graph::Layout::parse(backing.bytes()).ok_or(Fallback::Unreadable)?;
        if layout.dim != entry.dim as usize || layout.count as u64 != entry.row_count || layout.m() != entry.m as usize {
            return Err(Fallback::ManifestMismatch);
        }
        Ok(VectorSidecar { entry, backing, layout })
    }

    pub fn entry(&self) -> &VectorEntry {
        &self.entry
    }

    /// Whether the graph is memory-mapped from a plaintext file rather than held in
    /// process memory.
    pub fn is_mapped(&self) -> bool {
        matches!(self.backing, Backing::Mapped(_))
    }

    /// The `k` candidates nearest `query`, nearest first. A query of another dimension or
    /// of zero length probes nothing.
    pub fn probe(&self, query: &[f32], k: usize) -> std::result::Result<Vec<Candidate>, Fallback> {
        if query.len() != self.layout.dim {
            return Err(Fallback::DimensionMismatch);
        }
        let norm = query.iter().map(|x| x * x).sum::<f32>().sqrt();
        if !norm.is_finite() || norm == 0.0 {
            return Ok(Vec::new());
        }
        let q: Vec<f32> = query.iter().map(|x| x / norm).collect();
        let bytes = self.backing.bytes();
        Ok(self
            .layout
            .search(bytes, &q, k, k.max(EF_SEARCH_FLOOR))
            .into_iter()
            .filter_map(|(n, similarity)| Some(Candidate { id: self.layout.id(bytes, n)?.to_string(), similarity }))
            .collect())
    }
}

/// The snapshot directory and manifest entry of `table`'s current sidecar over `column`
/// at `dim`, or why there is none.
pub fn current_entry(store: &crate::Store, table: &str, column: &str, dim: usize) -> std::result::Result<(std::path::PathBuf, IndexEntry), Fallback> {
    let (chain, _) = store.chain(table).map_err(|_| Fallback::Unreadable)?;
    let snapshot = chain.first().ok_or(Fallback::NoSnapshot)?;
    let dir = store.snapshot_dir(table, &snapshot.snapshot_id).map_err(|_| Fallback::Unreadable)?;
    let entries: Vec<&IndexEntry> = snapshot
        .indexes
        .iter()
        .filter(|e| e.kind_name() == Some("vector") && e.column() == Some(column))
        .collect();
    if entries.is_empty() {
        return Err(Fallback::NoSidecar);
    }
    entries
        .into_iter()
        .find(|e| match e {
            IndexEntry::Vector(v) => v.dim as usize == dim,
            IndexEntry::Unrecognized(v) => v.get("dim").and_then(|d| d.as_u64()) == Some(dim as u64),
            IndexEntry::Fulltext(_) => false,
        })
        .map(|e| (dir, e.clone()))
        .ok_or(Fallback::DimensionMismatch)
}
