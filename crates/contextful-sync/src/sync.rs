//! Push, pull, the probe, bucket leases and fenced pointer publication for one store.

use contextful_context::{ContextError, Store};
use contextful_core::run::journal::sha256_hex;
use contextful_core::store::lease::{compaction_key, BucketLease, BucketPointer};
use contextful_core::store::lay_out::{Pointer, SnapshotId, SnapshotManifest, MANIFEST_FILE, POINTER_FILE};
use contextful_core::store::object::{Condition, ObjectError, ObjectStore, Put};
use contextful_core::store::sync::{
    confine, is_commit_log, is_pointer, merge, owner_of, BucketManifest, Coordination, Entry, SyncConfig, MANIFEST_KEY, PROBE_PREFIX,
    PULL_CONVERGENCE,
};
use contextful_core::store::StoreError;
use contextful_core::time::Instant;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Why a sync operation refused.
#[derive(Debug)]
pub enum SyncError {
    Store(StoreError),
    Object(ObjectError),
    Context(ContextError),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Store(e) => e.fmt(f),
            SyncError::Object(e) => write!(f, "bucket: {e}"),
            SyncError::Context(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for SyncError {}

impl From<StoreError> for SyncError {
    fn from(e: StoreError) -> SyncError {
        SyncError::Store(e)
    }
}

impl From<ObjectError> for SyncError {
    fn from(e: ObjectError) -> SyncError {
        SyncError::Object(e)
    }
}

impl From<ContextError> for SyncError {
    fn from(e: ContextError) -> SyncError {
        match e {
            ContextError::Store(s) => SyncError::Store(s),
            other => SyncError::Context(other),
        }
    }
}

pub type Result<T> = std::result::Result<T, SyncError>;

fn io(path: &Path, e: std::io::Error) -> SyncError {
    SyncError::Context(ContextError::Io { path: path.to_path_buf(), source: e })
}

/// Files under the store root that never leave the machine: its configuration, the
/// machine-local catalogs, locks, staging directories, dot-files, and table pointers,
/// which commit by a fenced compare-and-set instead.
fn syncable(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    !(rel == "config.toml"
        || rel == "derived.sqlite"
        || rel == "machine.sqlite"
        || name.starts_with('.')
        || name.ends_with(".lock")
        || rel.split('/').any(|s| s.ends_with(".staging"))
        || name == POINTER_FILE)
}

/// Conditional puts one key tries before a push reports it moving.
const PUT_ROUNDS: u32 = 5;

/// How a push treats a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyClass {
    /// This node owns it: uploaded and listed by this node alone.
    Owned,
    /// Another node owns it: never pushed from here.
    Foreign,
    /// Unowned and merged by its rule: a table's `schema.json`.
    Mergeable,
    /// Unowned and written once: a snapshot's files.
    Immutable,
}

fn class(key: &str, me: &str) -> KeyClass {
    match owner_of(key) {
        Some(o) if o == me => KeyClass::Owned,
        Some(_) => KeyClass::Foreign,
        None if key.ends_with("/schema.json") => KeyClass::Mergeable,
        None => KeyClass::Immutable,
    }
}

/// The table a key under `<project>/tables/` belongs to: the segments up to the table's
/// layout (`data`, `requests`, `schema.json` or `_pointer.json`), so a nested name keeps its `/`.
pub fn table_of(key: &str) -> Option<String> {
    let segs: Vec<&str> = key.split('/').collect();
    if segs.get(1) != Some(&"tables") {
        return None;
    }
    let end = segs.iter().enumerate().skip(2).position(|(_, s)| ["data", "requests", "schema.json", POINTER_FILE].contains(s))? + 2;
    (end > 2).then(|| segs[2..end].join("/"))
}

/// What a push did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PushReport {
    pub uploaded: Vec<String>,
    pub entries: usize,
    /// Rounds the manifest commit took.
    pub rounds: u32,
    /// Refusals the merge answered by keeping an entry.
    pub refused: Vec<String>,
}

/// What a pull did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullReport {
    pub downloaded: Vec<String>,
    /// Local copies a tombstone deleted.
    pub removed: Vec<String>,
    pub pointers: Vec<String>,
    pub attempts: u32,
}

/// Which tables a pull reaches.
#[derive(Debug, Clone, Default)]
pub struct PullScope {
    /// The tables requested by name; empty requests every table.
    pub tables: Vec<String>,
    /// Tables declaring `replicate = false`.
    pub replicate_off: Vec<String>,
}

/// A lease this node holds.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Held {
    pub key: String,
    pub lease: BucketLease,
    pub etag: String,
}

/// One store's side of one bucket.
pub struct Syncer {
    pub store: Store,
    pub bucket: Arc<dyn ObjectStore>,
    pub config: SyncConfig,
    pub prefix: String,
    pub project: String,
    /// This node's id.
    pub node: String,
}

impl Syncer {
    fn key(&self, rel: &str) -> Result<String> {
        Ok(confine(&self.prefix, rel)?)
    }

    fn project_rel(&self, rel: &str) -> String {
        format!("{}/{rel}", self.project)
    }

    /// Every syncable file of the store, keyed under the project, with its entry and path.
    pub fn local_entries(&self) -> Result<BTreeMap<String, (Entry, PathBuf)>> {
        let root = self.store.root().to_path_buf();
        let mut out = BTreeMap::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(io(&dir, e)),
            };
            for entry in entries {
                let path = entry.map_err(|e| io(&dir, e))?.path();
                let rel = path.strip_prefix(&root).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                if path.is_dir() {
                    if syncable(&format!("{rel}/x")) {
                        stack.push(path);
                    }
                    continue;
                }
                if !syncable(&rel) {
                    continue;
                }
                let bytes = std::fs::read(&path).map_err(|e| io(&path, e))?;
                let key = self.project_rel(&rel);
                let owner = owner_of(&key).unwrap_or_default();
                out.insert(key, (Entry { sha256: sha256_hex(&bytes), size: bytes.len() as u64, owner }, path));
            }
        }
        Ok(out)
    }

    fn manifest(&self) -> Result<(BucketManifest, Option<String>)> {
        match self.bucket.get(&self.key(MANIFEST_KEY)?)? {
            Some((bytes, etag)) => Ok((
                serde_json::from_slice(&bytes).map_err(|e| SyncError::Context(ContextError::Invalid(format!("the bucket manifest: {e}"))))?,
                Some(etag),
            )),
            None => Ok((BucketManifest::default(), None)),
        }
    }

    /// Demonstrate conditional writes with a live sentinel inside the prefix (`store.probe`).
    /// A backend refusing the method, the credential or the transport is inconclusive.
    pub fn probe(&self) -> Result<Coordination> {
        let inconclusive = |e: ObjectError| StoreError::SyncProbeInconclusive(format!("the conditional-write probe met {e}; capability not demonstrated"));
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?;
        let key = self.key(&format!("{PROBE_PREFIX}{}", nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()))?;
        let run = || -> std::result::Result<bool, ObjectError> {
            let Put::Applied(etag) = self.bucket.put(&key, b"probe-1", Condition::IfNoneMatch)? else { return Ok(false) };
            let create_refused = self.bucket.put(&key, b"probe-2", Condition::IfNoneMatch)? == Put::ConditionFailed;
            let stale_refused = self.bucket.put(&key, b"probe-3", Condition::IfMatch("stale".into()))? == Put::ConditionFailed;
            let current_applies = matches!(self.bucket.put(&key, b"probe-4", Condition::IfMatch(etag))?, Put::Applied(_));
            let settled = self.bucket.get(&key)?.map(|(b, _)| b) == Some(b"probe-4".to_vec());
            Ok(create_refused && stale_refused && current_applies && settled)
        };
        let outcome = run();
        let _ = self.bucket.delete(&key);
        match outcome {
            Ok(true) => Ok(Coordination::Cas),
            Ok(false) => Ok(Coordination::SingleWriter),
            Err(e) => Err(inconclusive(e).into()),
        }
    }

    /// Push the store: upload each file whose digest the bucket lacks, then commit the
    /// bucket manifest by merge and compare-and-set within the retry bound.
    pub fn push(&self, now: Instant) -> Result<PushReport> {
        let _guard = PushGuard::take(self.store.root())?;
        let coordination = match self.probe() {
            Ok(c) => c,
            // A push declaring single-writer coordination proceeds without the demonstration.
            Err(SyncError::Store(StoreError::SyncProbeInconclusive(_))) if !self.config.declares_cas() => Coordination::SingleWriter,
            Err(e) => return Err(e),
        };
        coordination.admit(&self.config)?;
        let local = self.local_entries()?;
        let (remote, _) = self.manifest()?;
        let mut report = PushReport::default();
        // The entries this node commits: keys it owns and immutable unowned keys. A copy of
        // a key another node owns is never pushed; a shared mergeable key commits by its merge.
        let mut mine: BTreeMap<String, Entry> = BTreeMap::new();
        let mut shared: Vec<String> = Vec::new();
        for (key, (entry, path)) in &local {
            let bytes = || std::fs::read(path).map_err(|e| io(path, e));
            match class(key, &self.node) {
                KeyClass::Foreign => continue,
                KeyClass::Mergeable => {
                    if self.put_merged(key, &bytes()?)? {
                        report.uploaded.push(key.clone());
                    }
                    shared.push(key.clone());
                }
                KeyClass::Owned => {
                    if remote.entries.get(key).is_none_or(|e| e.sha256 != entry.sha256) {
                        self.put_owned(key, &bytes()?)?;
                        report.uploaded.push(key.clone());
                    }
                    mine.insert(key.clone(), entry.clone());
                }
                KeyClass::Immutable => {
                    if remote.entries.get(key).is_none_or(|e| e.sha256 != entry.sha256) && self.put_immutable(key, &bytes()?, &entry.sha256)? {
                        report.uploaded.push(key.clone());
                    }
                    mine.insert(key.clone(), entry.clone());
                }
            }
        }
        let retries = self.config.push_retries().max(1);
        for round in 1..=retries {
            let (remote, etag) = self.manifest()?;
            let mut entries = mine.clone();
            // A shared key lists the object the bucket holds now, whoever merged it last.
            for key in &shared {
                if let Some((bytes, _)) = self.bucket.get(&self.key(key)?)? {
                    entries.insert(key.clone(), Entry { sha256: sha256_hex(&bytes), size: bytes.len() as u64, owner: String::new() });
                }
            }
            // Entries of other projects in the bucket pass through untouched.
            let others = remote.entries.iter().filter(|(k, _)| !k.starts_with(&format!("{}/", self.project))).map(|(k, e)| (k.clone(), e.clone()));
            entries.extend(others);
            let merged = merge(&remote, &entries, &self.node, now)?;
            let bytes = serde_json::to_vec_pretty(&merged.manifest).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?;
            let condition = etag.map_or(Condition::IfNoneMatch, Condition::IfMatch);
            if let Put::Applied(_) = self.bucket.put(&self.key(MANIFEST_KEY)?, &bytes, condition)? {
                report.entries = merged.manifest.entries.len();
                report.rounds = round;
                report.refused = merged.refused.iter().map(ToString::to_string).collect();
                return Ok(report);
            }
        }
        Err(StoreError::SyncManifestRebaseExhausted(format!(
            "the manifest commit lost {retries} races; every uploaded object is already in the bucket ({}); run the push again",
            if report.uploaded.is_empty() { "none this push".to_string() } else { report.uploaded.join(", ") }
        ))
        .into())
    }

    /// Upload a key this node owns: a create where the bucket holds none, else a replace
    /// on the ETag the bucket holds, so no write lands blind.
    fn put_owned(&self, key: &str, bytes: &[u8]) -> Result<()> {
        let k = self.key(key)?;
        for _ in 0..PUT_ROUNDS {
            let condition = match self.bucket.get(&k)? {
                Some((_, etag)) => Condition::IfMatch(etag),
                None => Condition::IfNoneMatch,
            };
            if let Put::Applied(_) = self.bucket.put(&k, bytes, condition)? {
                return Ok(());
            }
        }
        Err(StoreError::SyncManifestRebaseExhausted(format!("`{key}` moved under every conditional put; run the push again")).into())
    }

    /// Upload an immutable key: a create only. A different object already under the key refuses.
    fn put_immutable(&self, key: &str, bytes: &[u8], sha256: &str) -> Result<bool> {
        let k = self.key(key)?;
        match self.bucket.put(&k, bytes, Condition::IfNoneMatch)? {
            Put::Applied(_) => Ok(true),
            Put::ConditionFailed => match self.bucket.get(&k)? {
                Some((existing, _)) if sha256_hex(&existing) == sha256 => Ok(false),
                _ => Err(SyncError::Context(ContextError::Invalid(format!("`{key}` is immutable and the bucket holds another object under it")))),
            },
        }
    }

    /// Merge a shared key into the bucket's copy by its merge rule, replacing on the ETag
    /// read; a lost condition re-reads and re-merges. Returns whether the object changed.
    fn put_merged(&self, key: &str, local: &[u8]) -> Result<bool> {
        let k = self.key(key)?;
        for _ in 0..PUT_ROUNDS {
            let (merged, condition) = match self.bucket.get(&k)? {
                Some((remote, etag)) => {
                    let merged = merge_schema(&remote, local)?;
                    if sha256_hex(&merged) == sha256_hex(&remote) {
                        return Ok(false);
                    }
                    (merged, Condition::IfMatch(etag))
                }
                None => (local.to_vec(), Condition::IfNoneMatch),
            };
            if let Put::Applied(_) = self.bucket.put(&k, &merged, condition)? {
                return Ok(true);
            }
        }
        Err(StoreError::SyncManifestRebaseExhausted(format!("`{key}` moved under every merge; run the push again")).into())
    }

    fn local_path(&self, key: &str) -> Option<PathBuf> {
        key.strip_prefix(&format!("{}/", self.project)).map(|rel| self.store.root().join(rel))
    }

    /// Pull the bucket into the store: download each entry whose digest differs, re-fetching
    /// the manifest when a key moves beneath the download, apply each tombstone, then write
    /// every table pointer the bucket advances, all after every object they reach has landed.
    pub fn pull(&self, scope: &PullScope) -> Result<PullReport> {
        let replica = self.store.replica_of().is_some();
        if replica {
            if let Some(t) = scope.tables.iter().find(|t| scope.replicate_off.contains(t)) {
                return Err(StoreError::ReplicaSensitiveTable(format!(
                    "table `{t}` replicates off; read it through the proxying face of `{}`",
                    self.store.replica_of().unwrap_or_default()
                ))
                .into());
            }
        }
        let reaches = |key: &str| -> bool {
            match table_of(key) {
                None => scope.tables.is_empty(),
                Some(t) => (scope.tables.is_empty() || scope.tables.contains(&t)) && !(replica && scope.replicate_off.contains(&t)),
            }
        };
        let in_project = |k: &str| k.starts_with(&format!("{}/", self.project));
        let mut report = PullReport::default();
        let mut shortfall: Option<String> = None;
        let mut manifest = BucketManifest::default();
        for attempt in 1..=PULL_CONVERGENCE {
            report.attempts = attempt;
            shortfall = None;
            manifest = self.manifest()?.0;
            for (key, entry) in manifest.entries.iter().filter(|(k, _)| in_project(k) && reaches(k)) {
                // This node's own keys are authoritative here.
                if owner_of(key).as_deref() == Some(self.node.as_str()) {
                    continue;
                }
                let Some(path) = self.local_path(key) else { continue };
                let local = std::fs::read(&path).ok();
                if local.as_deref().map(sha256_hex).as_deref() == Some(entry.sha256.as_str()) {
                    continue;
                }
                if local.is_some() && is_commit_log(key) {
                    return Err(StoreError::SyncCursorConflict(format!(
                        "commit-log entry `{key}` differs between this node and the bucket; a cursor resolves through its commit, not by recency"
                    ))
                    .into());
                }
                let Some((bytes, _)) = self.bucket.get(&self.key(key)?)? else {
                    shortfall = Some(key.clone());
                    continue;
                };
                if sha256_hex(&bytes) != entry.sha256 {
                    return Err(StoreError::SyncObjectDigestMismatch(format!("`{key}` arrived with a digest other than its entry's; the object is discarded")).into());
                }
                let merged = match (&local, key.ends_with("/schema.json")) {
                    (Some(mine), true) => merge_schema(mine, &bytes)?,
                    _ => bytes,
                };
                write(&path, &merged)?;
                report.downloaded.push(key.clone());
            }
            if shortfall.is_none() {
                break;
            }
        }
        if let Some(key) = shortfall {
            return Err(StoreError::SyncPullDidNotConverge(format!("`{key}` kept moving across {PULL_CONVERGENCE} attempts; no pointer is written")).into());
        }
        // A tombstone deletes the local copy of the key it names.
        for key in manifest.tombstones.keys().filter(|k| in_project(k) && reaches(k) && !manifest.entries.contains_key(*k)) {
            if let Some(path) = self.local_path(key) {
                match std::fs::remove_file(&path) {
                    Ok(()) => report.removed.push(key.clone()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(io(&path, e)),
                }
            }
        }
        // Pointers last: every advancing pointer is verified before any is written.
        let mut advancing: Vec<(String, PathBuf, Pointer)> = Vec::new();
        for bucket_key in self.bucket.list(&self.key(&format!("{}/tables/", self.project))?)?.into_iter().filter(|k| is_pointer(k)) {
            let rel_key = bucket_key.strip_prefix(&format!("{}/", self.prefix)).unwrap_or(&bucket_key).to_string();
            if !reaches(&rel_key) {
                continue;
            }
            let Some((bytes, _)) = self.bucket.get(&bucket_key)? else { continue };
            let pointer: BucketPointer = serde_json::from_slice(&bytes).map_err(|e| SyncError::Context(ContextError::Invalid(format!("`{rel_key}`: {e}"))))?;
            let Some(snapshot) = pointer.snapshot_id else { continue };
            let Some(pointer_path) = self.local_path(&rel_key) else { continue };
            let snapshot_id: SnapshotId = serde_json::from_value(serde_json::Value::String(snapshot.clone())).map_err(|e| SyncError::Context(ContextError::Invalid(format!("`{rel_key}`: {e}"))))?;
            let held: Option<Pointer> = std::fs::read(&pointer_path).ok().and_then(|b| serde_json::from_slice(&b).ok());
            // The bucket's pointer advances a local one only when its fence, then its snapshot, is newer.
            if let Some(h) = &held {
                if (h.fence.unwrap_or(0), &h.snapshot_id) >= (pointer.fence, &snapshot_id) {
                    continue;
                }
            }
            let table_dir = pointer_path.parent().map(Path::to_path_buf).unwrap_or_default();
            if let Some(missing) = missing_parts(&table_dir, &snapshot)? {
                if replica {
                    return Err(StoreError::ReplicaPartialParquet(format!(
                        "snapshot `{snapshot}` lacks `{missing}` on this replica; the snapshot stays unpublished here"
                    ))
                    .into());
                }
                return Err(StoreError::SyncPullDidNotConverge(format!("snapshot `{snapshot}` lacks `{missing}`; no pointer is written")).into());
            }
            advancing.push((rel_key, pointer_path, Pointer { snapshot_id, fence: Some(pointer.fence) }));
        }
        for (rel_key, path, pointer) in advancing {
            write(&path, &serde_json::to_vec_pretty(&pointer).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?)?;
            report.pointers.push(rel_key);
        }
        Ok(report)
    }

    fn held_path(&self, table: &str) -> PathBuf {
        let root = self.store.root();
        // `.contextful/context/<project>` → `.contextful/sync/<project>/held/`: machine-local, never synced.
        let base = root.parent().and_then(Path::parent).map(|p| p.join("sync").join(&self.project)).unwrap_or_else(|| root.join(".sync"));
        base.join("held").join(format!("{table}.json"))
    }

    /// The compaction lease this node recorded as held for `table`.
    pub fn held(&self, table: &str) -> Result<Option<Held>> {
        let path = self.held_path(table);
        match std::fs::read(&path) {
            Ok(b) => Ok(serde_json::from_slice(&b).ok()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io(&path, e)),
        }
    }

    fn pointer_key(&self, table: &str) -> Result<String> {
        self.key(&format!("{}/tables/{table}/{POINTER_FILE}", self.project))
    }

    fn read_pointer(&self, table: &str) -> Result<(BucketPointer, Option<String>)> {
        match self.bucket.get(&self.pointer_key(table)?)? {
            Some((b, etag)) => Ok((serde_json::from_slice(&b).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?, Some(etag))),
            None => Ok((BucketPointer::default(), None)),
        }
    }

    /// Take `table`'s compaction lease, raise the table pointer's fence to it, and record
    /// the lease as held on this machine.
    pub fn acquire(&self, table: &str, now: Instant) -> Result<Held> {
        let key = self.key(&compaction_key(&self.project, table))?;
        loop {
            let current = self.bucket.get(&key)?;
            let parsed: Option<(BucketLease, String)> = match &current {
                Some((b, etag)) => {
                    Some((serde_json::from_slice(b).map_err(|e| SyncError::Context(ContextError::Invalid(format!("lease `{key}`: {e}"))))?, etag.clone()))
                }
                None => None,
            };
            let (lease, condition) = BucketLease::acquire(parsed.as_ref().map(|(l, e)| (l, e.as_str())), &self.node, now)?;
            let bytes = serde_json::to_vec_pretty(&lease).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?;
            let Put::Applied(etag) = self.bucket.put(&key, &bytes, condition)? else { continue };
            self.raise_pointer_fence(table, lease.fence)?;
            let held = Held { key: key.clone(), lease, etag };
            let path = self.held_path(table);
            write(&path, &serde_json::to_vec_pretty(&held).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?)?;
            return Ok(held);
        }
    }

    /// Raise the stored fence of `table`'s pointer to `fence`, keeping the published snapshot.
    fn raise_pointer_fence(&self, table: &str, fence: u64) -> Result<()> {
        let key = self.pointer_key(table)?;
        loop {
            let (pointer, etag) = self.read_pointer(table)?;
            if pointer.fence >= fence {
                return Ok(());
            }
            let raised = BucketPointer { fence, ..pointer };
            let bytes = serde_json::to_vec_pretty(&raised).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?;
            let condition = etag.map_or(Condition::IfNoneMatch, Condition::IfMatch);
            if let Put::Applied(_) = self.bucket.put(&key, &bytes, condition)? {
                return Ok(());
            }
        }
    }

    /// Release `table`'s compaction lease: the holder clears and the fence stays.
    pub fn release(&self, table: &str) -> Result<()> {
        let key = self.key(&compaction_key(&self.project, table))?;
        loop {
            let Some((b, etag)) = self.bucket.get(&key)? else {
                return Err(StoreError::LeaseNotHeld(format!("no lease `{key}` exists")).into());
            };
            let current: BucketLease = serde_json::from_slice(&b).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?;
            let (released, condition) = BucketLease::release(&current, &etag, &self.node)?;
            let bytes = serde_json::to_vec_pretty(&released).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?;
            if let Put::Applied(_) = self.bucket.put(&key, &bytes, condition)? {
                let _ = std::fs::remove_file(self.held_path(table));
                return Ok(());
            }
        }
    }

    /// Refuse early when a later acquisition fenced `held` out of `table`'s pointer.
    pub fn check_fence(&self, table: &str, held: &Held) -> Result<()> {
        let (pointer, _) = self.read_pointer(table)?;
        Ok(pointer.admit(held.lease.fence)?)
    }

    /// Publish `snapshot_id` as `table`'s pointer under `held`'s fence: a replace
    /// conditional on the pointer as read, refused once a higher fence holds it.
    pub fn publish(&self, table: &str, snapshot_id: &str, held: &Held) -> Result<()> {
        let key = self.pointer_key(table)?;
        loop {
            let (pointer, etag) = self.read_pointer(table)?;
            pointer.admit(held.lease.fence)?;
            let next = BucketPointer { snapshot_id: Some(snapshot_id.to_string()), fence: held.lease.fence };
            let bytes = serde_json::to_vec_pretty(&next).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?;
            let condition = etag.map_or(Condition::IfNoneMatch, Condition::IfMatch);
            if let Put::Applied(_) = self.bucket.put(&key, &bytes, condition)? {
                return Ok(());
            }
        }
    }
}

/// The first part of `snapshot` absent under `table_dir`, or `None` when it is whole.
fn missing_parts(table_dir: &Path, snapshot: &str) -> Result<Option<String>> {
    let dir = table_dir.join("data").join("snapshots").join(snapshot);
    let manifest_path = dir.join(MANIFEST_FILE);
    let Ok(text) = std::fs::read_to_string(&manifest_path) else { return Ok(Some(MANIFEST_FILE.to_string())) };
    let m: SnapshotManifest = serde_json::from_str(&text).map_err(|e| SyncError::Context(ContextError::Invalid(format!("{}: {e}", manifest_path.display()))))?;
    let present: BTreeSet<String> = std::fs::read_dir(&dir).map_err(|e| io(&dir, e))?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    Ok(m.parts.iter().map(|p| p.name.clone()).find(|n| !present.contains(n)))
}

/// Two copies of a table's `schema.json` merged column by column.
fn merge_schema(mine: &[u8], theirs: &[u8]) -> Result<Vec<u8>> {
    use contextful_core::store::reconcile::Schema;
    match (serde_json::from_slice::<Schema>(mine), serde_json::from_slice::<Schema>(theirs)) {
        (Ok(a), Ok(b)) => {
            let merged = a.merge(&b, &[])?;
            serde_json::to_vec_pretty(&merged).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))
        }
        _ => Ok(theirs.to_vec()),
    }
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
    }
    Ok(contextful_context::store::replace_file(path, bytes)?)
}

/// The push guard: one push of one store per machine, held by an advisory lock the
/// kernel releases when its holder exits (`store.push.in-flight`).
struct PushGuard {
    _file: std::fs::File,
}

impl PushGuard {
    fn take(root: &Path) -> Result<PushGuard> {
        use std::io::Write;
        let path = root.join(".push.lock");
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| io(d, e))?;
        }
        let mut file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path).map_err(|e| io(&path, e))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                let holder = std::fs::read_to_string(&path).unwrap_or_default();
                return Err(StoreError::SyncPushInFlight(format!("process {} holds this store's push guard", holder.trim())).into());
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(io(&path, e)),
        }
        file.set_len(0).map_err(|e| io(&path, e))?;
        let _ = write!(file, "{}", std::process::id());
        Ok(PushGuard { _file: file })
    }
}
