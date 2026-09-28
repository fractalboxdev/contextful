//! The full-text sidecar: the fold builds one per declaration into the staging directory,
//! so the snapshot's one rename commits data and sidecar together, and the read path
//! probes it for candidate `id_column` values across the whole snapshot
//! (`store.index.fulltext-by-fold`, `read.retrieve.fulltext-probe`).
//!
//! A sidecar directory holds `_manifest.json`, byte-equal to the snapshot manifest's entry,
//! and `postings.bin`. Both sit on disk as a vector sidecar's do: plaintext and mapped in
//! an unencrypted project, sealed per file and opened into process memory in an encrypted
//! one (`store.encrypt.sidecar-reader`).

pub mod postings;

use crate::error::{IoPath, Result};
use crate::vector::{identifiers, Backing, Fallback, Sealing};
use arrow_array::{Array, RecordBatch, StringArray};
use contextful_core::read::rank::{bm25_idf, bm25_term, fulltext_sealed_over_cap};
use contextful_core::read::tokens::PLURAL_SUFFIX_FLOOR;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::index::{FulltextEntry, IndexDecl, IndexKind, FULLTEXT_BUILDER, FULLTEXT_BUILDER_VERSION};
use contextful_core::store::lay_out::{SnapshotId, MANIFEST_FILE};
use contextful_core::store::StoreError;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// The laid-out postings and their identifiers, inside a sidecar directory.
pub const POSTINGS_FILE: &str = "postings.bin";

/// Build the full-text sidecar `index` declares over the staged `rows`, writing it under
/// `snapshot_dir` and returning the entry its snapshot manifest records. The postings hold
/// each row whose identifier is non-null and whose text yields a term
/// (`store.index.fulltext-by-fold`).
pub fn build(
    snapshot_dir: &Path,
    snapshot_id: &SnapshotId,
    rows: &RecordBatch,
    decl: &TableDecl,
    index: &IndexDecl,
    sealing: &Sealing<'_>,
) -> Result<FulltextEntry> {
    let table = decl.name.as_str();
    let id_column = decl
        .id_column()?
        .ok_or_else(|| StoreError::StoreIndexIdColumnUnresolved(format!("table `{table}` resolves no `id_column`")))?
        .to_string();
    let ids = identifiers(rows, table, &id_column)?;
    let text = rows
        .column_by_name(&index.column)
        .ok_or_else(|| StoreError::StoreIndexColumnAbsent(format!("table `{table}`: `{}` is no column of the staged rows", index.column)))?;
    let text = text
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| StoreError::StoreIndexColumnType(format!("table `{table}`: `{}` is not text", index.column)))?;
    let tokenizer = index.tokenizer();
    let docs: Vec<(String, Vec<String>)> = ids
        .into_iter()
        .enumerate()
        .filter_map(|(i, id)| {
            let terms = tokenizer.terms(if text.is_null(i) { "" } else { text.value(i) });
            Some((id?, terms)).filter(|(_, t)| !t.is_empty())
        })
        .collect();
    let bytes = postings::build(&docs);
    let layout = postings::Layout::parse(&bytes).expect("a built file parses");

    let path = index.path(crate::fold::escape);
    let dir = snapshot_dir.join(&path);
    fs::create_dir_all(&dir).at(&dir)?;
    let entry = FulltextEntry {
        kind: IndexKind::Fulltext,
        path,
        table: table.to_string(),
        snapshot_id: snapshot_id.to_string(),
        column: index.column.clone(),
        id_column,
        tokenizer,
        builder: FULLTEXT_BUILDER.to_string(),
        builder_version: FULLTEXT_BUILDER_VERSION,
        row_count: docs.len() as u64,
        term_count: layout.term_count as u64,
        key_version: sealing.key_version(),
    };
    sealing.write(&dir.join(POSTINGS_FILE), &bytes)?;
    sealing.write(&dir.join(MANIFEST_FILE), &serde_json::to_vec_pretty(&entry).expect("an entry serializes"))?;
    Ok(entry)
}

/// An opened full-text sidecar.
pub struct FulltextSidecar {
    entry: FulltextEntry,
    backing: Backing,
    layout: postings::Layout,
}

/// A candidate a probe returns: an `id_column` value and its BM25 score over the snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: String,
    pub score: f64,
}

/// What one probe returns: its candidates, best first, and how many postings it decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    pub candidates: Vec<Candidate>,
    pub postings_scored: u64,
}

impl FulltextSidecar {
    /// Open the sidecar a snapshot manifest's `entry` records, under `snapshot_dir`. Every
    /// precondition failure is a [`Fallback`], never an error: the sidecar accelerates a
    /// read and decides none.
    pub fn open(snapshot_dir: &Path, table: &str, entry: &serde_json::Value, sealing: &Sealing<'_>) -> std::result::Result<FulltextSidecar, Fallback> {
        let entry: FulltextEntry = serde_json::from_value(entry.clone()).map_err(|_| Fallback::ManifestMismatch)?;
        if entry.table != table
            || entry.kind != IndexKind::Fulltext
            || entry.builder != FULLTEXT_BUILDER
            || entry.builder_version != FULLTEXT_BUILDER_VERSION
        {
            return Err(Fallback::ManifestMismatch);
        }
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
        let own: FulltextEntry = serde_json::from_slice(&read(MANIFEST_FILE)?).map_err(|_| Fallback::Unreadable)?;
        if own != entry {
            return Err(Fallback::ManifestMismatch);
        }
        let file = dir.join(POSTINGS_FILE);
        let backing = match (sealing, entry.key_version) {
            (Sealing::Plaintext, 0) => {
                let f = fs::File::open(&file).map_err(|_| Fallback::Unreadable)?;
                // SAFETY: the file sits in a published snapshot directory, which no writer
                // rewrites; a pass stages a new snapshot beside it instead.
                Backing::Mapped(unsafe { memmap2::Mmap::map(&f) }.map_err(|_| Fallback::Unreadable)?)
            }
            (Sealing::Sealed(_), v) if v > 0 => {
                // Checked before the file is read, so an oversized sidecar is never decrypted
                // into memory to learn it is oversized.
                let size = fs::metadata(&file).map_err(|_| Fallback::Unreadable)?.len();
                if fulltext_sealed_over_cap(size) {
                    return Err(Fallback::OverCap);
                }
                Backing::Owned(read(POSTINGS_FILE)?)
            }
            _ => return Err(Fallback::Unreadable),
        };
        let layout = postings::Layout::parse(backing.bytes()).ok_or(Fallback::Unreadable)?;
        if layout.doc_count as u64 != entry.row_count || layout.term_count as u64 != entry.term_count {
            return Err(Fallback::ManifestMismatch);
        }
        Ok(FulltextSidecar { entry, backing, layout })
    }

    pub fn entry(&self) -> &FulltextEntry {
        &self.entry
    }

    /// Whether the postings are memory-mapped from a plaintext file rather than held in
    /// process memory.
    pub fn is_mapped(&self) -> bool {
        matches!(self.backing, Backing::Mapped(_))
    }

    /// Each document holding `token` and how often: a token the tokenizer keeps whole
    /// matches its term, and an ASCII word token its plural too; a token it splits matches
    /// its terms at consecutive positions (`read.retrieve.fulltext-probe`).
    fn occurrences(&self, bytes: &[u8], token: &str, decoded: &mut u64) -> std::result::Result<HashMap<u32, u32>, Fallback> {
        let terms = self.entry.tokenizer.terms(token);
        let mut out: HashMap<u32, u32> = HashMap::new();
        let mut lists = Vec::with_capacity(terms.len());
        let variants: Vec<String> = match terms.as_slice() {
            [] => return Ok(out),
            [one] => {
                let word = one.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
                let mut v = vec![one.clone()];
                if word && one.len() >= PLURAL_SUFFIX_FLOOR {
                    v.extend([format!("{one}s"), format!("{one}es")]);
                }
                v
            }
            _ => Vec::new(),
        };
        let fetch = |term: &str, decoded: &mut u64| -> std::result::Result<Option<Vec<postings::Posting>>, Fallback> {
            let Some(e) = self.layout.lookup(bytes, term).map_err(|_| Fallback::Unreadable)? else { return Ok(None) };
            *decoded += u64::from(e.df);
            self.layout.postings(bytes, e).map(Some).ok_or(Fallback::Unreadable)
        };
        if !variants.is_empty() {
            for v in &variants {
                for p in fetch(v, decoded)?.unwrap_or_default() {
                    *out.entry(p.doc).or_insert(0) += p.positions.len() as u32;
                }
            }
            return Ok(out);
        }
        for t in &terms {
            match fetch(t, decoded)? {
                Some(list) => lists.push(list),
                None => return Ok(out),
            }
        }
        // A phrase: walk the first term's documents; each later term's list is ascending by
        // document, so one cursor per list suffices.
        let mut cursors = vec![0usize; lists.len()];
        'docs: for first in &lists[0] {
            let mut rest = Vec::with_capacity(lists.len() - 1);
            for (i, list) in lists.iter().enumerate().skip(1) {
                while cursors[i] < list.len() && list[cursors[i]].doc < first.doc {
                    cursors[i] += 1;
                }
                match list.get(cursors[i]) {
                    Some(p) if p.doc == first.doc => rest.push(&p.positions),
                    _ => continue 'docs,
                }
            }
            let tf = first
                .positions
                .iter()
                .filter(|&&start| rest.iter().enumerate().all(|(i, ps)| ps.binary_search(&(start + 1 + i as u32)).is_ok()))
                .count() as u32;
            if tf > 0 {
                out.insert(first.doc, tf);
            }
        }
        Ok(out)
    }

    /// The `k` best candidates for the content `tokens` over the whole snapshot, by BM25
    /// over one should-clause per token; ties break by row order. A document matching no
    /// token is absent.
    pub fn probe(&self, tokens: &[String], k: usize) -> std::result::Result<Probe, Fallback> {
        let bytes = self.backing.bytes();
        let n = self.layout.doc_count as f64;
        let average = if self.layout.doc_count == 0 { 0.0 } else { self.layout.total_terms as f64 / n };
        let mut scores: HashMap<u32, f64> = HashMap::new();
        let mut decoded = 0u64;
        for token in tokens {
            let hits = self.occurrences(bytes, token, &mut decoded)?;
            let idf = bm25_idf(n, hits.len() as f64);
            for (doc, tf) in hits {
                let length = self.layout.length(bytes, doc).ok_or(Fallback::Unreadable)?;
                let norm = if average > 0.0 { f64::from(length) / average } else { 0.0 };
                *scores.entry(doc).or_insert(0.0) += bm25_term(idf, f64::from(tf), norm);
            }
        }
        let mut ranked: Vec<(u32, f64)> = scores.into_iter().collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        ranked.truncate(k);
        let candidates = ranked
            .into_iter()
            .map(|(doc, score)| Ok(Candidate { id: self.layout.id(bytes, doc).ok_or(Fallback::Unreadable)?.to_string(), score }))
            .collect::<std::result::Result<Vec<_>, Fallback>>()?;
        Ok(Probe { candidates, postings_scored: decoded })
    }
}

/// The snapshot directory and full-text manifest entries of `table`'s current snapshot, or
/// why there is none.
pub fn current_entries(store: &crate::Store, table: &str) -> std::result::Result<(PathBuf, String, Vec<serde_json::Value>), Fallback> {
    let (chain, _) = store.chain(table).map_err(|_| Fallback::Unreadable)?;
    let snapshot = chain.first().ok_or(Fallback::NoSnapshot)?;
    let dir = store.snapshot_dir(table, &snapshot.snapshot_id).map_err(|_| Fallback::Unreadable)?;
    let entries: Vec<serde_json::Value> =
        snapshot.indexes.iter().filter(|e| e.get("kind").and_then(|k| k.as_str()) == Some("fulltext")).cloned().collect();
    if entries.is_empty() {
        return Err(Fallback::NoSidecar);
    }
    Ok((dir, snapshot.snapshot_id.to_string(), entries))
}

/// The key an opened sidecar caches under: a digest of its table, snapshot, path and key
/// version. A published snapshot's files never change, so the key carries its own validity
/// (`read.rank.lexical-index-cache`).
pub fn fingerprint(table: &str, snapshot_id: &str, path: &str, key_version: u32) -> String {
    let d = Sha256::digest(format!("{table}\u{1f}{snapshot_id}\u{1f}{path}\u{1f}{key_version}").as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// The cached values by key, and their keys oldest first.
type Entries<V> = (HashMap<String, Arc<V>>, VecDeque<String>);

/// A first-in, first-out cache of opened sidecars keyed on their fingerprint, counting its
/// hits and opens (`read.rank.lexical-index-cache`).
pub struct SidecarCache<V> {
    capacity: usize,
    inner: Mutex<Entries<V>>,
    hits: AtomicU64,
    opens: AtomicU64,
}

impl<V> SidecarCache<V> {
    pub fn new(capacity: usize) -> SidecarCache<V> {
        SidecarCache { capacity, inner: Mutex::new((HashMap::new(), VecDeque::new())), hits: AtomicU64::new(0), opens: AtomicU64::new(0) }
    }

    /// The value under `key`, opening and caching it on a miss. A failed open caches
    /// nothing; past capacity the oldest entry leaves.
    pub fn get_or_open<E>(&self, key: &str, open: impl FnOnce() -> std::result::Result<V, E>) -> std::result::Result<Arc<V>, E> {
        if let Some(hit) = self.inner.lock().expect("the cache lock").0.get(key).cloned() {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(hit);
        }
        self.opens.fetch_add(1, Ordering::Relaxed);
        let value = Arc::new(open()?);
        let mut guard = self.inner.lock().expect("the cache lock");
        let (map, order) = &mut *guard;
        if map.insert(key.to_string(), value.clone()).is_none() {
            order.push_back(key.to_string());
        }
        while order.len() > self.capacity {
            if let Some(old) = order.pop_front() {
                map.remove(&old);
            }
        }
        Ok(value)
    }

    /// Lookups served from the cache, and opens it ran, since it was created.
    pub fn counts(&self) -> (u64, u64) {
        (self.hits.load(Ordering::Relaxed), self.opens.load(Ordering::Relaxed))
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("the cache lock").1.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
