//! `store.index`: the sidecar declaration, the identifier column every sidecar of a table
//! shares, the full-text tokenizer, and the manifest entry each built sidecar records.

use super::declare::TableDecl;
use super::lay_out::escape;
use super::reconcile::{ColumnType, Schema};
use super::StoreError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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

/// The builder a full-text sidecar records, with [`FULLTEXT_BUILDER_VERSION`] its identity
/// (`store.index.identity`).
pub const FULLTEXT_BUILDER: &str = "contextful-postings";

/// The on-disk postings format version the builder writes and the reader accepts.
pub const FULLTEXT_BUILDER_VERSION: u32 = 1;

/// Partitions past which a partition plan warns: 1000, the `store-partition-count` bound of
/// `store.index.partition-warnings`. Every check and message reads it here.
pub const PARTITION_COUNT_WARN: usize = 1000;

/// Median partition size below which a partition plan warns: 16 MiB, the
/// `store-partition-median` bound of `store.index.partition-warnings`.
pub const PARTITION_MEDIAN_WARN_BYTES: u64 = 16 * 1024 * 1024;

/// The warnings a partition plan over `by` raises, given each partition's size in bytes:
/// more than [`PARTITION_COUNT_WARN`] partitions, or a median partition below
/// [`PARTITION_MEDIAN_WARN_BYTES`]. Each names the partition columns
/// (`store.index.partition-warnings`).
pub fn partition_warnings(by: &[String], sizes: &[u64]) -> Vec<String> {
    if by.is_empty() || sizes.is_empty() {
        return Vec::new();
    }
    let columns = by.iter().map(|c| format!("`{c}`")).collect::<Vec<_>>().join(", ");
    let mut warnings = Vec::new();
    if sizes.len() > PARTITION_COUNT_WARN {
        warnings.push(format!("partition_by {columns} projects {} partitions, more than {PARTITION_COUNT_WARN}", sizes.len()));
    }
    let mut sorted = sizes.to_vec();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    let median = if sorted.len().is_multiple_of(2) { sorted[mid - 1] / 2 + sorted[mid] / 2 } else { sorted[mid] };
    if median < PARTITION_MEDIAN_WARN_BYTES {
        warnings.push(format!(
            "partition_by {columns} projects a median partition of {median} bytes, below {} MiB",
            PARTITION_MEDIAN_WARN_BYTES / (1024 * 1024)
        ));
    }
    warnings
}

/// A sidecar kind a declaration names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexKind {
    Vector,
    Fulltext,
}

/// How a full-text sidecar splits text into terms (`store.index.tokenizer`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tokenizer {
    /// Each lowercased alphanumeric run is one term.
    #[default]
    Unicode,
    /// As `Unicode`, except that each Han, Kana or Hangul stretch of a run indexes as
    /// overlapping character bigrams, and a one-character stretch as itself.
    Cjk,
}

impl Tokenizer {
    pub fn name(self) -> &'static str {
        match self {
            Tokenizer::Unicode => "unicode",
            Tokenizer::Cjk => "cjk",
        }
    }

    /// The terms of `text` in position order: the i-th term sits at position i.
    pub fn terms(self, text: &str) -> Vec<String> {
        let lowered = text.to_lowercase();
        let mut out = Vec::new();
        for run in lowered.split(|c: char| !c.is_alphanumeric()).filter(|r| !r.is_empty()) {
            match self {
                Tokenizer::Unicode => out.push(run.to_string()),
                Tokenizer::Cjk => cjk_terms(run, &mut out),
            }
        }
        out
    }
}

/// Whether `c` is a Han ideograph, a Hiragana or Katakana kana, or a Hangul syllable or
/// jamo: the scripts written without spaces between words.
pub fn is_cjk(c: char) -> bool {
    matches!(u32::from(c),
        0x1100..=0x11FF | 0x3040..=0x309F | 0x30A0..=0x30FF | 0x3130..=0x318F | 0x31F0..=0x31FF
        | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF66..=0xFF9F
        | 0x20000..=0x323AF)
}

/// Split one alphanumeric run into its CJK stretches, each as overlapping bigrams, and the
/// stretches between them, each as one term.
fn cjk_terms(run: &str, out: &mut Vec<String>) {
    let chars: Vec<char> = run.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let cjk = is_cjk(chars[i]);
        let start = i;
        while i < chars.len() && is_cjk(chars[i]) == cjk {
            i += 1;
        }
        let stretch = &chars[start..i];
        if cjk && stretch.len() > 1 {
            out.extend(stretch.windows(2).map(|w| w.iter().collect::<String>()));
        } else {
            out.push(stretch.iter().collect());
        }
    }
}

/// A vector sidecar's distance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Metric {
    #[default]
    Cosine,
}

/// One `[[pipeline.tables.indexes]]` block (`store.index.declaration`): a `vector` block
/// takes `model`, `dim`, `metric`, `m` and `ef_construction`, a `fulltext` block a
/// `tokenizer`, and a key of the other kind refuses the block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "IndexBlock")]
pub struct IndexDecl {
    pub kind: IndexKind,
    pub column: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id_column: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dim: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metric: Option<Metric>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub m: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ef_construction: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokenizer: Option<Tokenizer>,
}

/// An index block as written, before its keys are held to its kind.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexBlock {
    kind: IndexKind,
    column: String,
    #[serde(default)]
    id_column: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    dim: Option<u32>,
    #[serde(default)]
    metric: Option<Metric>,
    #[serde(default)]
    m: Option<u32>,
    #[serde(default)]
    ef_construction: Option<u32>,
    #[serde(default)]
    tokenizer: Option<Tokenizer>,
}

impl TryFrom<IndexBlock> for IndexDecl {
    type Error = String;

    fn try_from(b: IndexBlock) -> Result<IndexDecl, String> {
        let column = &b.column;
        match b.kind {
            IndexKind::Vector => {
                if b.model.is_none() || b.dim.is_none() {
                    return Err(format!("the vector sidecar over `{column}` declares no `model` or no `dim`"));
                }
                if b.tokenizer.is_some() {
                    return Err(format!("the vector sidecar over `{column}` declares a `tokenizer`, which only a full-text sidecar takes"));
                }
            }
            IndexKind::Fulltext => {
                let vector_keys = [
                    ("model", b.model.is_some()),
                    ("dim", b.dim.is_some()),
                    ("metric", b.metric.is_some()),
                    ("m", b.m.is_some()),
                    ("ef_construction", b.ef_construction.is_some()),
                ];
                if let Some((key, _)) = vector_keys.iter().find(|(_, set)| *set) {
                    return Err(format!("the full-text sidecar over `{column}` declares `{key}`, which only a vector sidecar takes"));
                }
            }
        }
        Ok(IndexDecl {
            kind: b.kind,
            column: b.column,
            id_column: b.id_column,
            model: b.model,
            dim: b.dim,
            metric: b.metric,
            m: b.m,
            ef_construction: b.ef_construction,
            tokenizer: b.tokenizer,
        })
    }
}

impl IndexDecl {
    /// A vector sidecar's model; empty on a full-text one.
    pub fn model(&self) -> &str {
        self.model.as_deref().unwrap_or_default()
    }

    /// A vector sidecar's dimension; 0 on a full-text one.
    pub fn dim(&self) -> u32 {
        self.dim.unwrap_or(0)
    }

    pub fn metric(&self) -> Metric {
        self.metric.unwrap_or_default()
    }

    /// A full-text sidecar's tokenizer, `unicode` unless declared.
    pub fn tokenizer(&self) -> Tokenizer {
        self.tokenizer.unwrap_or_default()
    }

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
        match self.kind {
            IndexKind::Vector => format!("{INDEXES_DIR}/vec-{}-{}/zone={ZONE_ALL}", escape(&self.column), escape(self.model())),
            IndexKind::Fulltext => format!("{INDEXES_DIR}/fts-{}-{}", escape(&self.column), self.tokenizer().name()),
        }
    }
}

/// The entry a built full-text sidecar records in its snapshot manifest's `indexes`, and
/// byte for byte in its own directory's manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FulltextEntry {
    pub kind: IndexKind,
    /// The sidecar directory, relative to the snapshot directory.
    pub path: String,
    pub table: String,
    pub snapshot_id: String,
    pub column: String,
    pub id_column: String,
    pub tokenizer: Tokenizer,
    pub builder: String,
    pub builder_version: u32,
    /// Rows the postings hold.
    pub row_count: u64,
    /// Distinct terms the dictionary holds.
    pub term_count: u64,
    /// The key version sealing the sidecar's files; 0 for plaintext.
    pub key_version: u32,
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

/// One entry of a snapshot manifest's `indexes`: a built sidecar of a kind this build
/// reads, or an entry it does not recognise, kept byte for byte so a newer writer's sidecar
/// neither refuses the manifest nor vanishes from it on a rewrite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexEntry {
    Vector(VectorEntry),
    Fulltext(FulltextEntry),
    Unrecognized(UnrecognizedEntry),
}

/// An `indexes` entry this build does not read: its text as the manifest held it, written
/// back verbatim, and its fields parsed for the probes that name a kind, path or column.
#[derive(Debug, Clone)]
pub struct UnrecognizedEntry {
    raw: Box<serde_json::value::RawValue>,
    fields: serde_json::Value,
}

impl UnrecognizedEntry {
    /// The entry the JSON text `json` spells, kept as written.
    pub fn parse(json: &str) -> serde_json::Result<UnrecognizedEntry> {
        let raw = serde_json::value::RawValue::from_string(json.to_string())?;
        let fields = serde_json::from_str(raw.get())?;
        Ok(UnrecognizedEntry { raw, fields })
    }

    /// The field `key`, when the entry is an object carrying it.
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.fields.get(key)
    }

    /// The entry's text, byte for byte as read.
    pub fn as_str(&self) -> &str {
        self.raw.get()
    }
}

impl PartialEq for UnrecognizedEntry {
    fn eq(&self, other: &UnrecognizedEntry) -> bool {
        self.raw.get() == other.raw.get()
    }
}

impl Eq for UnrecognizedEntry {}

impl std::fmt::Display for UnrecognizedEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.raw.get())
    }
}

impl IndexEntry {
    /// The kind of a recognised entry.
    pub fn kind(&self) -> Option<IndexKind> {
        match self {
            IndexEntry::Vector(_) => Some(IndexKind::Vector),
            IndexEntry::Fulltext(_) => Some(IndexKind::Fulltext),
            IndexEntry::Unrecognized(_) => None,
        }
    }

    /// The `kind` string the entry carries, recognised or not.
    pub fn kind_name(&self) -> Option<&str> {
        match self {
            IndexEntry::Vector(_) => Some("vector"),
            IndexEntry::Fulltext(_) => Some("fulltext"),
            IndexEntry::Unrecognized(v) => v.get("kind").and_then(serde_json::Value::as_str),
        }
    }

    /// The sidecar directory, relative to the snapshot directory.
    pub fn path(&self) -> Option<&str> {
        match self {
            IndexEntry::Vector(e) => Some(&e.path),
            IndexEntry::Fulltext(e) => Some(&e.path),
            IndexEntry::Unrecognized(v) => v.get("path").and_then(serde_json::Value::as_str),
        }
    }

    /// The indexed column.
    pub fn column(&self) -> Option<&str> {
        match self {
            IndexEntry::Vector(e) => Some(&e.column),
            IndexEntry::Fulltext(e) => Some(&e.column),
            IndexEntry::Unrecognized(v) => v.get("column").and_then(serde_json::Value::as_str),
        }
    }
}

impl From<VectorEntry> for IndexEntry {
    fn from(e: VectorEntry) -> IndexEntry {
        IndexEntry::Vector(e)
    }
}

impl From<FulltextEntry> for IndexEntry {
    fn from(e: FulltextEntry) -> IndexEntry {
        IndexEntry::Fulltext(e)
    }
}

impl Serialize for IndexEntry {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            IndexEntry::Vector(e) => e.serialize(s),
            IndexEntry::Fulltext(e) => e.serialize(s),
            IndexEntry::Unrecognized(v) => v.raw.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for IndexEntry {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = Box::<serde_json::value::RawValue>::deserialize(d)?;
        let fields: serde_json::Value = serde_json::from_str(raw.get()).map_err(serde::de::Error::custom)?;
        let typed = match fields.get("kind").and_then(serde_json::Value::as_str) {
            Some("vector") => serde_json::from_value(fields.clone()).ok().map(IndexEntry::Vector),
            Some("fulltext") => serde_json::from_value(fields.clone()).ok().map(IndexEntry::Fulltext),
            _ => None,
        };
        Ok(typed.unwrap_or(IndexEntry::Unrecognized(UnrecognizedEntry { raw, fields })))
    }
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
    /// `id_column`, defaulting to a single-column primary key on a table declaring no
    /// `valid_time` (`store.index.id-column`), and to `derived_id` on a table keyed as a
    /// derive output table (`run.emit.derived-id`). `None` when the table declares no sidecar.
    ///
    /// A valid-time table keeps one row per key and valid-time line
    /// (`store.fold.valid-time-line`), so its key repeats within a snapshot and names no
    /// identifier (`store.index.id-column-unresolved`).
    pub fn id_column(&self) -> Result<Option<&str>, StoreError> {
        let single_key = match self.primary_key() {
            [k] => Some(k.as_str()),
            _ => None,
        };
        let derived = (self.primary_key() == crate::run::derive::config::DERIVE_PRIMARY_KEY).then_some(crate::run::derive::emit::DERIVED_ID);
        let default = single_key.filter(|_| self.valid_time.is_none()).or(derived);
        let mut resolved: Option<&str> = None;
        for idx in self.indexes() {
            let Some(id) = idx.id_column.as_deref().or(default) else {
                let why = if single_key.is_some() {
                    "the table declares `valid_time`, so its key repeats across valid-time lines"
                } else {
                    "the table has no single-column primary key"
                };
                return Err(StoreError::StoreIndexIdColumnUnresolved(format!(
                    "table `{}`: the sidecar over `{}` names no `id_column`, and {why}",
                    self.name, idx.column
                )));
            };
            if self.valid_time.is_some() && Some(id) == single_key {
                return Err(StoreError::StoreIndexIdColumnUnresolved(format!(
                    "table `{}`: `id_column` `{id}` is the key of a valid-time table, which repeats across valid-time lines",
                    self.name
                )));
            }
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

    /// Hold the sidecar declarations to the manifest alone: one shared identifier, and the
    /// types `columns` declares (`store.index.id-column-unresolved`, `store.index.column-type`).
    pub fn validate_index_declaration(&self) -> Result<(), StoreError> {
        for index in self.indexes() {
            if self.redaction.iter().flatten().any(|rule| rule.column == index.column) {
                return Err(StoreError::StoreIndexOverRedactedColumn(format!("table `{}` indexes removed column `{}`", self.name, index.column)));
            }
        }
        let declared = Schema {
            columns: self
                .column_types()
                .into_iter()
                .map(|(name, ty)| super::reconcile::Column { name, ty, nullable: true })
                .collect(),
        };
        self.check_indexes(&declared, false)
    }

    /// Hold the sidecar declarations to a landing's reconciled schema: every column it
    /// carries is typed as the sidecar reads it, before any row lands (`store.index.column-type`).
    pub fn validate_index_types(&self, schema: &Schema) -> Result<(), StoreError> {
        self.check_indexes(schema, false)
    }

    /// Hold the sidecar declarations to the reconciled schema before the pass that builds
    /// them (`store.index.column-absent`, `store.index.column-type`).
    pub fn validate_indexes(&self, schema: &Schema) -> Result<(), StoreError> {
        self.check_indexes(schema, true)
    }

    /// Refuse two declarations resolving to one sidecar directory (`store.index.path-collision`).
    fn check_index_paths(&self) -> Result<(), StoreError> {
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        for (i, idx) in self.indexes().iter().enumerate() {
            let path = idx.path(escape);
            if let Some(&first) = seen.get(&path) {
                let describe = |n: usize, d: &IndexDecl| match d.kind {
                    IndexKind::Vector => format!("`indexes[{n}]` (vector over `{}`, model `{}`)", d.column, d.model()),
                    IndexKind::Fulltext => format!("`indexes[{n}]` (full-text over `{}`, tokenizer `{}`)", d.column, d.tokenizer().name()),
                };
                return Err(StoreError::StoreIndexPathCollision(format!(
                    "table `{}`: {} and {} both resolve to `{path}`",
                    self.name,
                    describe(first, &self.indexes()[first]),
                    describe(i, idx)
                )));
            }
            seen.insert(path, i);
        }
        Ok(())
    }

    /// `require` refuses a column `schema` lacks; otherwise an absent column passes.
    fn check_indexes(&self, schema: &Schema, require: bool) -> Result<(), StoreError> {
        self.check_index_paths()?;
        let Some(id) = self.id_column()? else { return Ok(()) };
        match schema.get(id).map(|c| &c.ty) {
            None if require => {
                return Err(StoreError::StoreIndexColumnAbsent(format!(
                    "table `{}`: `id_column` `{id}` is no column of the table",
                    self.name
                )))
            }
            None | Some(ColumnType::Utf8 | ColumnType::Int64 | ColumnType::Int32) => {}
            Some(ty) => {
                return Err(StoreError::StoreIndexColumnType(format!(
                    "table `{}`: `id_column` `{id}` is typed {}; an identifier is text or integer",
                    self.name,
                    ty.name()
                )))
            }
        }
        for idx in self.indexes() {
            let kind = match idx.kind {
                IndexKind::Vector => "vector",
                IndexKind::Fulltext => "full-text",
            };
            match (idx.kind, schema.get(&idx.column).map(|c| &c.ty)) {
                (_, None) if require => {
                    return Err(StoreError::StoreIndexColumnAbsent(format!(
                        "table `{}`: a {kind} sidecar indexes `{}`, which is no column of the table",
                        self.name, idx.column
                    )))
                }
                (_, None) => {}
                (IndexKind::Vector, Some(ColumnType::FixedSizeList(_, n))) if *n == idx.dim() => {}
                (IndexKind::Fulltext, Some(ColumnType::Utf8)) => {}
                (IndexKind::Vector, Some(ty)) => {
                    return Err(StoreError::StoreIndexColumnType(format!(
                        "table `{}`: the vector sidecar over `{}` declares dim {}, and the column is typed {}",
                        self.name,
                        idx.column,
                        idx.dim(),
                        ty.name()
                    )))
                }
                (IndexKind::Fulltext, Some(ty)) => {
                    return Err(StoreError::StoreIndexColumnType(format!(
                        "table `{}`: the full-text sidecar over `{}` reads text, and the column is typed {}",
                        self.name,
                        idx.column,
                        ty.name()
                    )))
                }
            }
        }
        Ok(())
    }
}
