//! The push plan, push, pull, the probe, bucket leases and fenced pointer publication for one store.

use contextful_context::{ContextError, Store};
use contextful_core::run::journal::sha256_hex;
use contextful_core::store::catalog::{DERIVED_CATALOG_FILE, MACHINE_CATALOG_FILE};
use contextful_core::store::lease::{compaction_key, BucketLease, BucketPointer, CLOCK_SKEW_SECS};
use contextful_core::store::lay_out::{Pointer, SnapshotId, SnapshotManifest, MANIFEST_FILE, POINTER_FILE, SNAPSHOT_ANCESTORS_MAX};
use contextful_core::store::object::{CasScope, Condition, ObjectError, ObjectStore, Put};
use contextful_core::store::sync::{
    admit_format, confine, generation_key, generation_of, is_commit_log, is_pointer, merge, owner_of, run_state_node, BucketManifest, Coordination, Entry, SyncConfig,
    GENERATION_PREFIX, MANIFEST_KEY, PROBE_PREFIX, PULL_CONVERGENCE,
};
use contextful_core::store::StoreError;
use contextful_core::surface::reside::{compare_sites, SiteRegions};
use contextful_core::surface::SurfaceError;
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
    Surface(SurfaceError),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Store(e) => e.fmt(f),
            SyncError::Object(e) => write!(f, "bucket: {e}"),
            SyncError::Context(e) => e.fmt(f),
            SyncError::Surface(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for SyncError {}

impl From<SurfaceError> for SyncError {
    fn from(e: SurfaceError) -> SyncError {
        SyncError::Surface(e)
    }
}

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
/// machine-local catalogs with the journal or log SQLite keeps beside each, locks, staging
/// directories, dot-files, and table pointers, which commit by a fenced compare-and-set
/// instead.
fn syncable(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let catalog = |file: &str| rel == file || rel.strip_prefix(file).is_some_and(|rest| ["-journal", "-wal", "-shm"].contains(&rest));
    !(rel == "config.toml"
        || catalog(DERIVED_CATALOG_FILE)
        || catalog(MACHINE_CATALOG_FILE)
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

/// Every syncable file of `store`, keyed under `project`, with its entry and path.
pub fn local_entries(store: &Store, project: &str) -> Result<BTreeMap<String, (Entry, PathBuf)>> {
    let root = store.root().to_path_buf();
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
            let key = format!("{project}/{rel}");
            let owner = owner_of(&key).unwrap_or_default();
            out.insert(key, (Entry { sha256: sha256_hex(&bytes), size: bytes.len() as u64, owner }, path));
        }
    }
    Ok(out)
}

/// What a push of one store commits, computed from its files alone (`store.emit`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManifestPlan {
    /// Keys the push commits at their local digests: this node's own and immutable unowned ones.
    pub mine: BTreeMap<String, Entry>,
    /// Unowned mergeable keys, a table's `schema.json`: the push commits the bucket's merged copy.
    pub shared: BTreeMap<String, Entry>,
    /// Keys another node owns, held locally and never pushed from here.
    pub foreign: BTreeMap<String, Entry>,
    paths: BTreeMap<String, PathBuf>,
}

impl ManifestPlan {
    /// The plan as a bucket manifest: every planned key at its local digest, no tombstone,
    /// no generation (`store.emit.plan`).
    pub fn manifest(&self) -> BucketManifest {
        let mut entries = self.mine.clone();
        entries.extend(self.shared.iter().map(|(k, e)| (k.clone(), e.clone())));
        BucketManifest { entries, ..BucketManifest::default() }
    }

    fn read(&self, key: &str) -> Result<Vec<u8>> {
        let path = &self.paths[key];
        std::fs::read(path).map_err(|e| io(path, e))
    }
}

/// Plan a push of `store` as `node` would make it under `project`, reading no bucket
/// object: a key another node owns stays out (`store.emit.plan-scope`).
pub fn plan_manifest(store: &Store, project: &str, node: &str) -> Result<ManifestPlan> {
    let mut plan = ManifestPlan::default();
    for (key, (entry, path)) in local_entries(store, project)? {
        match class(&key, node) {
            KeyClass::Foreign => {
                plan.foreign.insert(key, entry);
                continue;
            }
            KeyClass::Mergeable => plan.shared.insert(key.clone(), entry),
            KeyClass::Owned | KeyClass::Immutable => plan.mine.insert(key.clone(), entry),
        };
        plan.paths.insert(key, path);
    }
    Ok(plan)
}

/// One table pointer of the store: its key under the project, its table directory, and
/// the pointer, or why it reads as none.
struct LocalPointer {
    key: String,
    table: String,
    table_dir: PathBuf,
    pointer: std::result::Result<Pointer, String>,
}

/// Every table pointer under `store`'s `tables/`, a nested table name keeping its `/`.
fn local_pointers(store: &Store, project: &str) -> Result<Vec<LocalPointer>> {
    let tables = store.root().join("tables");
    let mut out = Vec::new();
    let mut stack = vec![tables.clone()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(io(&dir, e)),
        };
        for entry in entries {
            let path = entry.map_err(|e| io(&dir, e))?.path();
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if path.is_dir() {
                if !["data", "requests"].contains(&name.as_str()) && !name.starts_with('.') && !name.ends_with(".staging") {
                    stack.push(path);
                }
                continue;
            }
            if name != POINTER_FILE {
                continue;
            }
            let table = dir.strip_prefix(&tables).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default();
            let pointer = std::fs::read(&path).map_err(|e| e.to_string()).and_then(|b| serde_json::from_slice::<Pointer>(&b).map_err(|e| e.to_string()));
            out.push(LocalPointer { key: format!("{project}/tables/{table}/{POINTER_FILE}"), table, table_dir: dir.clone(), pointer });
        }
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(out)
}

/// What a push does with one local pointer against the bucket's.
enum Carry {
    /// Publish this pointer.
    Publish(BucketPointer),
    /// The bucket already names it, or its snapshot descends from none the bucket names.
    Keep,
    /// The pointer stays local, for the reason given (`store.push.pointer-fenced`,
    /// `store.push.pointer-unrooted`).
    Warn(String),
}

/// How a snapshot relates to the bucket pointer's, by its recorded ancestry.
enum Ancestry {
    Descends,
    /// The recorded ancestry reaches a root without it.
    Diverges,
    /// The recorded ancestry ends at a manifest no longer on disk.
    Unknown,
}

/// Whether `local` advances the bucket's pointer `remote` (`store.push.pointer-carry`):
/// its snapshot is whole here and the bucket's names none or an ancestor of it. The
/// bucket's fence carries over; a local fence below it keeps the pointer local.
fn carry(local: &LocalPointer, remote: Option<&BucketPointer>) -> Result<Carry> {
    let pointer = match &local.pointer {
        Ok(p) => p,
        Err(e) => return Ok(Carry::Warn(format!("`{}` does not parse ({e}); the pointer stays local", local.key))),
    };
    let snapshot = pointer.snapshot_id.to_string();
    let fence = remote.map_or(0, |r| r.fence);
    let published = remote.and_then(|r| r.snapshot_id.as_deref());
    if published == Some(snapshot.as_str()) {
        return Ok(Carry::Keep);
    }
    if let Some(f) = pointer.fence.filter(|f| *f < fence) {
        return Ok(Carry::Warn(format!("`{}` carries fence {f} and the bucket's pointer fence {fence}; the pointer stays local", local.key)));
    }
    if let Some(missing) = missing_parts(&local.table_dir, &snapshot)? {
        return Ok(Carry::Warn(format!("snapshot `{snapshot}` lacks `{missing}`; `{}` stays local", local.key)));
    }
    if let Some(ancestor) = published {
        match ancestry(&local.table_dir, &snapshot, ancestor) {
            Ancestry::Descends => {}
            Ancestry::Diverges => return Ok(Carry::Keep),
            Ancestry::Unknown => {
                return Ok(Carry::Warn(format!(
                    "the ancestry of snapshot `{snapshot}` ends at a collected manifest before reaching `{ancestor}`, which the bucket names; `{}` stays local",
                    local.key
                )))
            }
        }
    }
    Ok(Carry::Publish(BucketPointer { snapshot_id: Some(snapshot), fence: fence.max(pointer.fence.unwrap_or(0)) }))
}

/// Whether `ancestor` precedes `snapshot` under `table_dir`, read from each manifest's
/// `ancestors` (`store.lay-out.ancestors`), else its `parent`, so retention collecting
/// the snapshots between them leaves the answer intact.
fn ancestry(table_dir: &Path, snapshot: &str, ancestor: &str) -> Ancestry {
    let mut seen = BTreeSet::new();
    let mut at = snapshot.to_string();
    while seen.insert(at.clone()) {
        let path = table_dir.join("data").join("snapshots").join(&at).join(MANIFEST_FILE);
        let Some(manifest) = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<SnapshotManifest>(&b).ok()) else {
            return Ancestry::Unknown;
        };
        let next = match (&manifest.ancestors, &manifest.parent) {
            (Some(recorded), _) => {
                if recorded.iter().any(|a| a.to_string() == ancestor) {
                    return Ancestry::Descends;
                }
                // A list short of the bound reaches a root.
                if recorded.len() < SNAPSHOT_ANCESTORS_MAX {
                    return Ancestry::Diverges;
                }
                recorded.last().map(ToString::to_string)
            }
            (None, Some(parent)) => {
                if parent.to_string() == ancestor {
                    return Ancestry::Descends;
                }
                Some(parent.to_string())
            }
            (None, None) => None,
        };
        let Some(next) = next else { return Ancestry::Diverges };
        at = next;
    }
    Ancestry::Unknown
}

/// A bucket or generation manifest read off `key`, its `format` held to the newest this
/// build parses before any other field is read (`store.push.format-unsupported`).
fn parse_manifest(key: &str, bytes: &[u8]) -> Result<BucketManifest> {
    let invalid = |e: serde_json::Error| SyncError::Context(ContextError::Invalid(format!("`{key}`: {e}")));
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(invalid)?;
    let format = value.get("format").and_then(serde_json::Value::as_u64).unwrap_or(1);
    admit_format(key, u32::try_from(format).unwrap_or(u32::MAX))?;
    serde_json::from_value(value).map_err(invalid)
}

/// What a push did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PushReport {
    pub uploaded: Vec<String>,
    pub entries: usize,
    /// The generation the push committed.
    pub generation: u64,
    /// Rounds the manifest commit took.
    pub rounds: u32,
    /// Refusals the merge answered by keeping an entry, and local pointers the push kept
    /// local (`store.push.pointer-fenced`).
    pub refused: Vec<String>,
    /// Table pointers the push published (`store.push.pointer-carry`).
    pub pointers: Vec<String>,
    /// Local keys another node owns that the committed manifest neither lists nor
    /// tombstones, with their owner (`store.push.stranded`).
    pub stranded: Vec<(String, String)>,
}

/// What one manifest commit did.
struct Commit {
    entries: usize,
    generation: u64,
    rounds: u32,
    refused: Vec<String>,
    stranded: Vec<(String, String)>,
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
    /// The generation to restore; absent pulls the bucket manifest (`store.pull.generation`).
    pub generation: Option<u64>,
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
    /// The pushing site and its residency allow-set; `None` skips the cross-site check.
    pub residency: Option<SiteResidency>,
}

/// A site's id and the allow-set it declares, `None` when it declares no `[residency]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteResidency {
    pub site_id: String,
    pub regions: Option<Vec<String>>,
}

impl SiteResidency {
    /// Hold a push to the set `recorded` in the bucket manifest (`surface.reside.site-regions`).
    fn check(&self, recorded: &BucketManifest) -> Result<()> {
        Ok(compare_sites(&self.site_id, self.regions.as_deref(), recorded.residency.as_ref())?)
    }

    /// The record this site's push leaves: its own set, none when it held the record and
    /// dropped its `[residency]`, or the one already recorded.
    fn record(&self, recorded: &BucketManifest) -> Option<SiteRegions> {
        match &self.regions {
            Some(r) => Some(SiteRegions { site_id: self.site_id.clone(), regions: r.clone() }),
            None => recorded.residency.clone().filter(|r| r.site_id != self.site_id),
        }
    }
}

impl Syncer {
    fn key(&self, rel: &str) -> Result<String> {
        Ok(confine(&self.prefix, rel)?)
    }

    /// Every syncable file of the store, keyed under the project, with its entry and path.
    pub fn local_entries(&self) -> Result<BTreeMap<String, (Entry, PathBuf)>> {
        local_entries(&self.store, &self.project)
    }

    /// What a push of this store commits, reading no bucket object (`store.emit`).
    pub fn plan_manifest(&self) -> Result<ManifestPlan> {
        plan_manifest(&self.store, &self.project, &self.node)
    }

    fn manifest(&self) -> Result<(BucketManifest, Option<String>)> {
        Ok(self.manifest_bytes()?.map_or((BucketManifest::default(), None), |(m, _, etag)| (m, Some(etag))))
    }

    /// The bucket manifest with the bytes and ETag it was read at.
    fn manifest_bytes(&self) -> Result<Option<(BucketManifest, Vec<u8>, String)>> {
        match self.bucket.get(&self.key(MANIFEST_KEY)?)? {
            Some((bytes, etag)) => Ok(Some((parse_manifest(MANIFEST_KEY, &bytes)?, bytes, etag))),
            None => Ok(None),
        }
    }

    /// Generation `n`'s manifest; a generation the bucket lacks refuses, naming the newest
    /// (`store.pull.generation-absent`).
    fn generation_manifest(&self, n: u64) -> Result<BucketManifest> {
        let key = generation_key(n);
        match self.bucket.get(&self.key(&key)?)? {
            Some((bytes, _)) => parse_manifest(&key, &bytes),
            None => {
                let newest = self.generations()?.last().copied().unwrap_or(0).max(self.manifest()?.0.generation);
                Err(StoreError::SyncGenerationAbsent(format!("the bucket holds no `{key}`; its newest generation is {newest}")).into())
            }
        }
    }

    /// Every generation the bucket holds a generation manifest for, ascending.
    fn generations(&self) -> Result<Vec<u64>> {
        let root = self.key(GENERATION_PREFIX)?;
        let under = root.strip_suffix(GENERATION_PREFIX).unwrap_or_default().to_string();
        let mut out: Vec<u64> = self.bucket.list(&root)?.iter().filter_map(|k| generation_of(k.strip_prefix(&under)?)).collect();
        out.sort_unstable();
        Ok(out)
    }

    /// Create generation `n`'s immutable copy holding `bytes`; a copy already there holds
    /// the same commit, or the push refuses after its commit (`store.push.generation-conflict`).
    fn put_generation(&self, n: u64, bytes: &[u8]) -> Result<()> {
        let key = generation_key(n);
        match self.bucket.put(&self.key(&key)?, bytes, Condition::IfNoneMatch)? {
            Put::Applied(_) => Ok(()),
            Put::ConditionFailed => match self.bucket.get(&self.key(&key)?)? {
                Some((existing, _)) if existing == bytes => Ok(()),
                _ => Err(StoreError::SyncGenerationConflict(format!(
                    "generation {n} committed to the bucket manifest, and `{key}` already holds another commit; `pull --generation {n}` restores that one, and the next push numbers past it"
                ))
                .into()),
            },
        }
    }

    /// Every table pointer of this project the bucket holds, keyed under the project.
    fn project_pointers(&self) -> Result<BTreeMap<String, BucketPointer>> {
        let mut out = BTreeMap::new();
        for bucket_key in self.bucket.list(&self.key(&format!("{}/tables/", self.project))?)?.into_iter().filter(|k| is_pointer(k)) {
            let rel_key = bucket_key.strip_prefix(&format!("{}/", self.prefix)).unwrap_or(&bucket_key).to_string();
            let Some((bytes, _)) = self.bucket.get(&bucket_key)? else { continue };
            let pointer: BucketPointer = serde_json::from_slice(&bytes).map_err(|e| SyncError::Context(ContextError::Invalid(format!("`{rel_key}`: {e}"))))?;
            out.insert(rel_key, pointer);
        }
        Ok(out)
    }

    /// Demonstrate conditional writes with a live sentinel inside the prefix (`store.probe`).
    /// A backend refusing the method, the credential or the transport is inconclusive, and
    /// a bucket whose conditional put holds on one machine only resolves `single-writer`
    /// without the sentinel (`store.probe.network-volume`).
    pub fn probe(&self) -> Result<Coordination> {
        Ok(self.probe_with_reason()?.0)
    }

    /// The probe's coordination and the reason it resolved so: the sentinel's verdict, or
    /// the mount type of a bucket whose conditional put holds on one machine only.
    pub fn probe_with_reason(&self) -> Result<(Coordination, String)> {
        if let CasScope::Machine(why) = self.bucket.cas_scope() {
            return Ok((Coordination::SingleWriter, why));
        }
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
            Ok(true) => Ok((Coordination::Cas, "conditional writes demonstrated".into())),
            Ok(false) => Ok((Coordination::SingleWriter, "conditional writes not demonstrated".into())),
            Err(e) => Err(inconclusive(e).into()),
        }
    }

    /// Push the store: upload each file whose digest the bucket lacks, then commit the
    /// bucket manifest as the next generation by merge and compare-and-set within the retry
    /// bound, and write that generation's immutable copy.
    pub fn push(&self, now: Instant) -> Result<PushReport> {
        let _guard = PushGuard::take(self.store.root())?;
        let coordination = match self.probe() {
            Ok(c) => c,
            // A push declaring single-writer coordination proceeds without the demonstration.
            Err(SyncError::Store(StoreError::SyncProbeInconclusive(_))) if !self.config.declares_cas() => Coordination::SingleWriter,
            Err(e) => return Err(e),
        };
        coordination.admit(&self.config).map_err(|e| match (e, self.bucket.cas_scope()) {
            (StoreError::SyncCoordinationUnproven(m), CasScope::Machine(why)) => StoreError::SyncCoordinationUnproven(format!("{m}: {why}")),
            (e, _) => e,
        })?;
        let plan = self.plan_manifest()?;
        let remote = self.manifest()?.0;
        if let Some(r) = &self.residency {
            r.check(&remote)?;
        }
        let mut report = PushReport::default();
        // A copy of a key another node owns is never pushed; a shared mergeable key commits by its merge.
        for (key, entry) in &plan.mine {
            if remote.entries.get(key).is_some_and(|e| e.sha256 == entry.sha256) {
                continue;
            }
            let uploaded = match class(key, &self.node) {
                KeyClass::Owned => {
                    self.put_owned(key, &plan.read(key)?)?;
                    true
                }
                _ => self.put_immutable(key, &plan.read(key)?, &entry.sha256)?,
            };
            if uploaded {
                report.uploaded.push(key.clone());
            }
        }
        for key in plan.shared.keys() {
            if self.put_merged(key, &plan.read(key)?)? {
                report.uploaded.push(key.clone());
            }
        }
        // A table under a standing compaction lease is the holder's to publish (`store.push.pointer-leased`).
        let mut locals = Vec::new();
        for local in local_pointers(&self.store, &self.project)? {
            if !self.lease_stands(&local.table, now)? {
                locals.push(local);
            }
        }
        let first = self.commit(&plan, now, &report.uploaded)?;
        report.entries = first.entries;
        report.generation = first.generation;
        report.rounds = first.rounds;
        report.refused = first.refused;
        report.stranded = first.stranded;
        // Pointers after the commit: every object a carried snapshot reaches is listed (`store.push.pointer-carry`).
        for local in &locals {
            match self.publish_local(local)? {
                Carry::Publish(_) => report.pointers.push(local.key.clone()),
                Carry::Keep => {}
                Carry::Warn(w) => report.refused.push(w),
            }
        }
        // A generation names a pointer only once the bucket holds it.
        if !report.pointers.is_empty() {
            let second = self.commit(&plan, now, &report.uploaded)?;
            report.entries = second.entries;
            report.generation = second.generation;
            report.rounds += second.rounds;
            report.stranded = second.stranded;
        }
        Ok(report)
    }

    /// Commit the bucket manifest listing `plan` and the project's table pointers as read
    /// before the commit, then write its generation (`store.push.manifest-commit`,
    /// `store.push.generation`).
    fn commit(&self, plan: &ManifestPlan, now: Instant, uploaded: &[String]) -> Result<Commit> {
        let retries = self.config.push_retries().max(1);
        let in_project = |k: &str| k.starts_with(&format!("{}/", self.project));
        for round in 1..=retries {
            let (remote, etag) = match self.manifest_bytes()? {
                Some((remote, bytes, etag)) => (Some((remote, bytes)), Some(etag)),
                None => (None, None),
            };
            let generations = self.generations()?;
            let newest = generations.last().copied().unwrap_or(0);
            // No commit lands past a generation whose copy is absent (`store.push.generation-heal`).
            if let Some((remote, bytes)) = &remote {
                if remote.generation > 0 && generations.binary_search(&remote.generation).is_err() {
                    // A copy a concurrent writer created between the listing and this put already stands.
                    self.bucket.put(&self.key(&generation_key(remote.generation))?, bytes, Condition::IfNoneMatch)?;
                }
            }
            let remote = remote.map(|(m, _)| m).unwrap_or_default();
            // Pointers read before the commit name snapshots whose files earlier commits listed.
            let pointers = self.project_pointers()?;
            let mut entries = plan.mine.clone();
            // A shared key lists the object the bucket holds now, whoever merged it last.
            for key in plan.shared.keys() {
                if let Some((bytes, _)) = self.bucket.get(&self.key(key)?)? {
                    entries.insert(key.clone(), Entry { sha256: sha256_hex(&bytes), size: bytes.len() as u64, owner: String::new() });
                }
            }
            // Entries of other projects in the bucket pass through untouched.
            let others = remote.entries.iter().filter(|(k, _)| !in_project(k)).map(|(k, e)| (k.clone(), e.clone()));
            entries.extend(others);
            if let Some(r) = &self.residency {
                r.check(&remote)?;
            }
            let merged = merge(&remote, &entries, &self.node, now)?;
            let mut committed = merged.manifest;
            // A manifest rewritten by a writer predating `generation` reads 0; the files keep the count (`store.push.generation-floor`).
            committed.generation = remote.generation.max(newest) + 1;
            committed.pointers = remote.pointers.iter().filter(|(k, _)| !in_project(k)).map(|(k, p)| (k.clone(), p.clone())).collect();
            committed.pointers.extend(pointers);
            if let Some(r) = &self.residency {
                committed.residency = r.record(&remote);
            }
            let bytes = serde_json::to_vec_pretty(&committed).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?;
            let condition = etag.map_or(Condition::IfNoneMatch, Condition::IfMatch);
            if let Put::Applied(_) = self.bucket.put(&self.key(MANIFEST_KEY)?, &bytes, condition)? {
                self.put_generation(committed.generation, &bytes)?;
                return Ok(Commit {
                    entries: committed.entries.len(),
                    generation: committed.generation,
                    rounds: round,
                    refused: merged.refused.iter().map(ToString::to_string).collect(),
                    stranded: plan
                        .foreign
                        .iter()
                        .filter(|(k, _)| !committed.entries.contains_key(*k) && !committed.tombstones.contains_key(*k))
                        .map(|(k, e)| (k.clone(), e.owner.clone()))
                        .collect(),
                });
            }
        }
        Err(StoreError::SyncManifestRebaseExhausted(format!(
            "the manifest commit lost {retries} races; every uploaded object is already in the bucket ({}); run the push again",
            if uploaded.is_empty() { "none this push".to_string() } else { uploaded.join(", ") }
        ))
        .into())
    }

    /// Whether a holder keeps `table`'s compaction lease unexpired at `now`, this node included.
    fn lease_stands(&self, table: &str, now: Instant) -> Result<bool> {
        let Some((bytes, _)) = self.bucket.get(&self.key(&compaction_key(&self.project, table))?)? else { return Ok(false) };
        let lease: BucketLease = serde_json::from_slice(&bytes).map_err(|e| SyncError::Context(ContextError::Invalid(format!("lease on `{table}`: {e}"))))?;
        Ok(lease.holder.is_some() && lease.expires_at.is_some_and(|e| now < e.plus_secs(CLOCK_SKEW_SECS)))
    }

    /// Publish `local` as its table's bucket pointer by a conditional put on the pointer as
    /// read, re-deciding on a lost condition. Returns the decision the bucket's pointer took.
    fn publish_local(&self, local: &LocalPointer) -> Result<Carry> {
        let key = self.pointer_key(&local.table)?;
        for _ in 0..PUT_ROUNDS {
            let (remote, etag) = self.read_pointer(&local.table)?;
            let decision = carry(local, etag.is_some().then_some(&remote))?;
            let Carry::Publish(next) = &decision else { return Ok(decision) };
            let bytes = serde_json::to_vec_pretty(next).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?;
            let condition = etag.map_or(Condition::IfNoneMatch, Condition::IfMatch);
            if let Put::Applied(_) = self.bucket.put(&key, &bytes, condition)? {
                return Ok(decision);
            }
        }
        Err(StoreError::SyncManifestRebaseExhausted(format!("`{}` moved under every conditional put; run the push again", local.key)).into())
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
    /// Under `scope.generation`, restore that generation's entries and pointers instead
    /// (`store.pull.generation`).
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
        let identity_key = format!("{}/{}", self.project, contextful_core::store::lay_out::STORE_ID_FILE);
        let reaches = |key: &str| -> bool {
            if key == identity_key {
                return true;
            }
            match table_of(key) {
                None => scope.tables.is_empty(),
                Some(t) => (scope.tables.is_empty() || scope.tables.contains(&t)) && !(replica && scope.replicate_off.contains(&t)),
            }
        };
        let in_project = |k: &str| k.starts_with(&format!("{}/", self.project));
        // A generation is immutable: read once, and held against every local file in scope.
        let generation = match scope.generation {
            Some(n) => {
                let listed = self.generation_manifest(n)?;
                let local = self.local_entries()?;
                // A run state is a summary, not rows: one the generation does not list stays (`store.pull.generation-run-state`).
                let run_state = |k: &str| run_state_node(k).is_some_and(|n| k == format!("{}/nodes/{n}/run-state.json", self.project));
                if let Some(extra) = local.keys().find(|k| in_project(k) && reaches(k) && !run_state(k) && !listed.entries.contains_key(*k)) {
                    return Err(StoreError::SyncGenerationDiverged(format!(
                        "`{extra}` is in this store and generation {n} does not list it; restore generation {n} into a store holding none of its files"
                    ))
                    .into());
                }
                Some((listed, local.keys().filter_map(|k| table_of(k)).collect::<BTreeSet<String>>()))
            }
            None => None,
        };
        let mut report = PullReport::default();
        let mut shortfall: Option<String> = None;
        let mut manifest = BucketManifest::default();
        for attempt in 1..=PULL_CONVERGENCE {
            report.attempts = attempt;
            shortfall = None;
            manifest = match &generation {
                Some((listed, _)) => listed.clone(),
                None => self.manifest()?.0,
            };
            if let Some(remote) = manifest.entries.get(&identity_key) {
                let local_path = self.store.root().join(contextful_core::store::lay_out::STORE_ID_FILE);
                match std::fs::read(&local_path) {
                    Ok(local) if sha256_hex(&local) != remote.sha256 => {
                        return Err(StoreError::StoreIdentityConflict(format!(
                            "`{}` holds a different store UUID from the bucket; clone into an empty store root",
                            local_path.display()
                        )).into());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(io(&local_path, error)),
                    Ok(_) => {}
                }
            }
            for (key, entry) in manifest.entries.iter().filter(|(k, _)| in_project(k) && reaches(k)) {
                // This node's own keys are authoritative here, except in a restore.
                if generation.is_none() && owner_of(key).as_deref() == Some(self.node.as_str()) {
                    continue;
                }
                let Some(path) = self.local_path(key) else { continue };
                let local = std::fs::read(&path).ok();
                let schema = key.ends_with("/schema.json");
                // A restore takes a schema as the bucket holds it now (`store.pull.generation-schema`).
                let current_schema = generation.is_some() && schema;
                if !current_schema && local.as_deref().map(sha256_hex).as_deref() == Some(entry.sha256.as_str()) {
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
                if !current_schema && sha256_hex(&bytes) != entry.sha256 {
                    return Err(StoreError::SyncObjectDigestMismatch(format!("`{key}` arrived with a digest other than its entry's; the object is discarded")).into());
                }
                let merged = match (&local, schema) {
                    (Some(mine), true) => merge_schema(mine, &bytes)?,
                    _ => bytes,
                };
                if local.as_deref() == Some(merged.as_slice()) {
                    continue;
                }
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
        // A tombstone deletes the local copy of the key it names; a restore lists its set whole.
        if generation.is_none() {
            for key in manifest.tombstones.keys().filter(|k| in_project(k) && reaches(k) && !manifest.entries.contains_key(*k)) {
                if let Some(path) = self.local_path(key) {
                    match std::fs::remove_file(&path) {
                        Ok(()) => report.removed.push(key.clone()),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(io(&path, e)),
                    }
                }
            }
        }
        // Pointers last: every pointer to write is verified before any is written.
        let candidates: Vec<(String, BucketPointer)> = match &generation {
            Some((listed, _)) => listed.pointers.iter().map(|(k, p)| (k.clone(), p.clone())).collect(),
            None => self.project_pointers()?.into_iter().collect(),
        };
        let mut advancing: Vec<(String, PathBuf, Pointer)> = Vec::new();
        for (rel_key, pointer) in candidates {
            if !in_project(&rel_key) || !reaches(&rel_key) {
                continue;
            }
            let Some(snapshot) = pointer.snapshot_id else { continue };
            let Some(pointer_path) = self.local_path(&rel_key) else { continue };
            let snapshot_id: SnapshotId = serde_json::from_value(serde_json::Value::String(snapshot.clone())).map_err(|e| SyncError::Context(ContextError::Invalid(format!("`{rel_key}`: {e}"))))?;
            let held: Option<Pointer> = std::fs::read(&pointer_path).ok().and_then(|b| serde_json::from_slice(&b).ok());
            // The bucket's pointer advances a local one only when its fence, then its snapshot,
            // is newer; a restore writes the generation's pointer whatever its fence.
            if let Some(h) = &held {
                let ahead = (h.fence.unwrap_or(0), &h.snapshot_id) >= (pointer.fence, &snapshot_id);
                if (generation.is_none() && ahead) || (h.fence == Some(pointer.fence) && h.snapshot_id == snapshot_id) {
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
        // A restored table the generation publishes no snapshot for keeps no local pointer.
        if let Some((listed, tables)) = &generation {
            for table in tables.iter().filter(|t| reaches(&format!("{}/tables/{t}/schema.json", self.project))) {
                let key = format!("{}/tables/{table}/{POINTER_FILE}", self.project);
                if listed.pointers.get(&key).is_none_or(|p| p.snapshot_id.is_none()) {
                    if let Some(path) = self.local_path(&key) {
                        match std::fs::remove_file(&path) {
                            Ok(()) => report.removed.push(key),
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                            Err(e) => return Err(io(&path, e)),
                        }
                    }
                }
            }
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
    /// the lease as held on this machine. A bucket whose conditional put holds on one
    /// machine only grants no lease (`store.lease.network-volume`).
    pub fn acquire(&self, table: &str, now: Instant) -> Result<Held> {
        if let CasScope::Machine(why) = self.bucket.cas_scope() {
            return Err(StoreError::SyncCoordinationUnproven(format!("the compaction lease on `{table}` needs conditional writes across clients: {why}")).into());
        }
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
    let dir = table_dir.join(contextful_core::store::lay_out::SNAPSHOTS_DIR).join(snapshot);
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
