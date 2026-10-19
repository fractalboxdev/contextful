//! A store booting on metadata: the part source that fetches each Parquet part whole from
//! the bucket when a read first needs it (`store.pull.lazy-parts`), held in a bounded local
//! cache (`store.pull.part-cache`).

use crate::sync::{sync_dir, write};
use crate::transfer::each;
use contextful_core::run::journal::sha256_hex;
use contextful_core::store::object::{ObjectError, ObjectStore, PartHold, PartSource};
use contextful_core::store::sync::{confine, BucketManifest, Entry, MANIFEST_KEY};
use contextful_core::store::StoreError;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Longest a read waits for a part the bucket answers unavailable: 30 s (`read.cache.miss-wait`).
pub const PART_WAIT: Duration = Duration::from_secs(30);

/// The file under the sync directory listing each fetched part and its size.
const INDEX_FILE: &str = "part-cache.json";

/// Download receipts name fetched parts independently of the cache index.
const RECEIPTS_DIR: &str = "fetched-parts";

/// Whether `key` is an object a lazy pull leaves in the bucket: a table's Parquet data part.
pub fn is_lazy_part(key: &str) -> bool {
    key.ends_with(".parquet") && key.split('/').any(|s| s == "data")
}

/// The parts a store fetched lazily, relative to its root, which no push or pull records as
/// held (`store.merge.held-record`): the cache may remove any of them.
pub(crate) fn fetched_parts(root: &Path, project: &str) -> BTreeSet<String> {
    let sync = sync_dir(root, project);
    let mut parts: BTreeSet<String> = read_index(&sync.join(INDEX_FILE)).into_keys().collect();
    let receipts = sync.join(RECEIPTS_DIR);
    for entry in std::fs::read_dir(receipts).into_iter().flatten().flatten() {
        if let Ok(rel) = std::fs::read_to_string(entry.path()) {
            if is_lazy_part(&rel) && root.join(&rel).is_file() {
                parts.insert(rel);
            }
        }
    }
    parts
}

fn read_index(path: &Path) -> BTreeMap<String, u64> {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// One fetched part: its size and when a read last held it.
struct Cached {
    size: u64,
    tick: u64,
}

#[derive(Default)]
struct Cache {
    parts: BTreeMap<String, Cached>,
    /// Holds each part carries now, a part held by any read staying on disk.
    held: HashMap<String, usize>,
    tick: u64,
    bytes: u64,
}

struct Inner {
    bucket: Arc<dyn ObjectStore>,
    prefix: String,
    project: String,
    root: PathBuf,
    bound: u64,
    /// Fetches in flight at once (`store.pull.transfer-width`).
    width: usize,
    index: PathBuf,
    receipts: PathBuf,
    /// The bucket manifest's entries, read on the first fetch and re-read when it lacks a key.
    entries: Mutex<Option<BTreeMap<String, Entry>>>,
    cache: Mutex<Cache>,
}

/// The part source of one lazy store.
#[derive(Clone)]
pub struct LazyParts(Arc<Inner>);

impl std::fmt::Debug for LazyParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LazyParts").field("root", &self.0.root).field("bound", &self.0.bound).finish_non_exhaustive()
    }
}

/// Parts one read holds; dropping it releases them and lets the cache shrink to its bound.
struct Holding {
    inner: Arc<Inner>,
    files: Vec<String>,
    /// A refused batch persists its completed downloads after releasing its holds.
    save_on_drop: bool,
}

impl Drop for Holding {
    fn drop(&mut self) {
        let mut cache = self.inner.lock();
        for f in &self.files {
            if let Some(n) = cache.held.get_mut(f) {
                *n -= 1;
                if *n == 0 {
                    cache.held.remove(f);
                }
            }
        }
        if self.inner.shrink(&mut cache) || self.save_on_drop {
            self.inner.save(&cache);
        }
    }
}

impl LazyParts {
    /// The part source of the store at `root` under `project`, fetching through `bucket` under
    /// `prefix`, `width` at once, and holding at most `bound` bytes of fetched parts once no read
    /// holds them.
    pub fn new(bucket: Arc<dyn ObjectStore>, prefix: &str, project: &str, root: &Path, bound: u64, width: usize) -> LazyParts {
        let index = sync_dir(root, project).join(INDEX_FILE);
        let mut cache = Cache::default();
        for rel in fetched_parts(root, project) {
            if let Ok(meta) = std::fs::metadata(root.join(&rel)) {
                let size = meta.len();
                cache.bytes += size;
                cache.parts.insert(rel, Cached { size, tick: 0 });
            }
        }
        LazyParts(Arc::new(Inner {
            bucket,
            prefix: prefix.to_string(),
            project: project.to_string(),
            root: root.to_path_buf(),
            bound,
            width,
            index,
            receipts: sync_dir(root, project).join(RECEIPTS_DIR),
            entries: Mutex::new(None),
            cache: Mutex::new(cache),
        }))
    }
}

impl Inner {
    fn lock(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The bucket manifest's entry for `key`, re-reading the manifest once when the held copy lacks it.
    fn entry(&self, key: &str) -> Result<Option<Entry>, StoreError> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = entries.as_ref().and_then(|m| m.get(key)) {
            return Ok(Some(e.clone()));
        }
        let manifest = match self.bucket.get(&confine(&self.prefix, MANIFEST_KEY)?).map_err(|e| unavailable(MANIFEST_KEY, &e))? {
            Some((bytes, _)) => serde_json::from_slice::<BucketManifest>(&bytes).map_err(|e| StoreError::SyncObjectDigestMismatch(format!("`{MANIFEST_KEY}`: {e}")))?,
            None => BucketManifest::default(),
        };
        let found = manifest.entries.get(key).cloned();
        *entries = Some(manifest.entries);
        Ok(found)
    }

    /// Fetch `rel` whole, verified against its manifest entry, waiting out an unavailable
    /// bucket within [`PART_WAIT`], and account for it in the cache.
    fn fetch(&self, rel: &str) -> Result<(), StoreError> {
        let key = format!("{}/{rel}", self.project);
        let snapshot = rel.split('/').skip_while(|s| *s != "snapshots").nth(1).unwrap_or("-");
        let collected = || StoreError::SyncPartCollected(format!("`{rel}` of snapshot `{snapshot}` is gone from the bucket; the next pull advances the pointer past it"));
        let Some(entry) = self.entry(&key)? else { return Err(collected()) };
        let started = Instant::now();
        let bytes = loop {
            match self.bucket.get(&confine(&self.prefix, &key)?) {
                Ok(Some((bytes, _))) => break bytes,
                Ok(None) => return Err(collected()),
                Err(e @ (ObjectError::Unavailable(_) | ObjectError::Transport(_))) => {
                    if started.elapsed() >= PART_WAIT {
                        return Err(unavailable(rel, &e));
                    }
                    std::thread::sleep(Duration::from_secs(1).min(PART_WAIT.saturating_sub(started.elapsed())));
                }
                Err(e) => return Err(unavailable(rel, &e)),
            }
        };
        if sha256_hex(&bytes) != entry.sha256 {
            return Err(StoreError::SyncObjectDigestMismatch(format!("`{key}` arrived with a digest other than its entry's; the object is discarded")));
        }
        // A receipt precedes its part so a missing index loses no download's provenance.
        write(&self.receipts.join(sha256_hex(rel.as_bytes())), rel.as_bytes()).map_err(|e| StoreError::ReadPartUnavailable(format!("`{rel}`: {e}")))?;
        write(&self.root.join(rel), &bytes).map_err(|e| StoreError::ReadPartUnavailable(format!("`{rel}`: {e}")))?;
        let mut cache = self.lock();
        let size = bytes.len() as u64;
        let tick = cache.tick;
        // Two reads fetching one part at once count it once.
        if !cache.parts.contains_key(rel) {
            cache.bytes += size;
        }
        cache.parts.insert(rel.to_string(), Cached { size, tick });
        Ok(())
    }

    /// Remove the least recently held fetched parts no read holds until the cache fits its bound.
    fn shrink(&self, cache: &mut Cache) -> bool {
        let before = cache.parts.len();
        while cache.bytes > self.bound {
            let Some(rel) = cache.parts.iter().filter(|(rel, _)| !cache.held.contains_key(*rel)).min_by_key(|(_, c)| c.tick).map(|(rel, _)| rel.clone()) else { break };
            let _ = std::fs::remove_file(self.root.join(&rel));
            let _ = std::fs::remove_file(self.receipts.join(sha256_hex(rel.as_bytes())));
            if let Some(c) = cache.parts.remove(&rel) {
                cache.bytes -= c.size;
            }
        }
        cache.parts.len() != before
    }

    /// Write the index; download receipts retain each part when the index write fails.
    fn save(&self, cache: &Cache) {
        let index: BTreeMap<&String, u64> = cache.parts.iter().map(|(rel, c)| (rel, c.size)).collect();
        if let Ok(bytes) = serde_json::to_vec(&index) {
            let _ = write(&self.index, &bytes);
        }
    }
}

fn unavailable(what: &str, e: &ObjectError) -> StoreError {
    StoreError::ReadPartUnavailable(format!("`{what}` did not download within {} s: {e}", PART_WAIT.as_secs()))
}

impl PartSource for LazyParts {
    fn hold_inputs(&self, files: &[String]) -> Result<PartHold, StoreError> {
        let mut total = 0u64;
        for rel in files {
            total += match std::fs::metadata(self.0.root.join(rel)) {
                Ok(m) => m.len(),
                Err(_) => self.0.entry(&format!("{}/{rel}", self.0.project))?.map_or(0, |e| e.size),
            };
        }
        if total > self.0.bound {
            return Err(StoreError::FoldInputsExceedCache(format!(
                "the pass reads {total} B of parts and `[sync] part_cache_bytes` holds {} B; raise the bound or fold on a store holding its parts",
                self.0.bound
            )));
        }
        self.hold(files)
    }

    fn hold(&self, files: &[String]) -> Result<PartHold, StoreError> {
        let missing: Vec<String> = {
            let mut cache = self.0.lock();
            cache.tick += 1;
            let tick = cache.tick;
            for f in files {
                *cache.held.entry(f.clone()).or_default() += 1;
                if let Some(c) = cache.parts.get_mut(f) {
                    c.tick = tick;
                }
            }
            files.iter().filter(|f| !self.0.root.join(f.as_str()).exists()).cloned().collect()
        };
        // The holding releases every file on any return, a refused fetch included.
        let mut holding = Holding { inner: self.0.clone(), files: files.to_vec(), save_on_drop: !missing.is_empty() };
        each(self.0.width, &missing, |rel| self.0.fetch(rel).map_err(crate::sync::SyncError::Store)).map_err(|e| match e {
            crate::sync::SyncError::Store(s) => s,
            other => StoreError::ReadPartUnavailable(other.to_string()),
        })?;
        if holding.save_on_drop {
            self.0.save(&self.0.lock());
            holding.save_on_drop = false;
        }
        Ok(PartHold(Arc::new(holding)))
    }
}
