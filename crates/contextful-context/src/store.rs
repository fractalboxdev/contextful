//! A project's store root on a filesystem: its configuration, table directories,
//! schemas, committed runs, and the pointer chain.

use crate::error::{ContextError, IoPath, Result};
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::{
    is_path_segment, store_root, Pointer, RunManifest, SnapshotId, SnapshotManifest, MANIFEST_FILE, POINTER_FILE, SCHEMA_FILE,
};
use contextful_core::store::reconcile::Schema;
use contextful_core::store::resolve::TableState;
use contextful_core::store::commit_log;
use contextful_core::store::StoreError;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// The store root's `config.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreConfig {
    #[serde(default)]
    pub node: Option<NodeConfig>,
    #[serde(default)]
    pub encryption: Option<EncryptionConfig>,
    /// Bucket sync, read by the sync adapter.
    #[serde(default)]
    pub sync: Option<toml::Value>,
    /// Present on a consuming replica: the canonical store it reads from.
    #[serde(default)]
    pub replica: Option<ReplicaConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplicaConfig {
    /// The canonical store a replica answers for.
    pub of: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConfig {
    pub id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncryptionConfig {
    pub key_source: String,
}

/// An opened store root.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
    config_node_id: Option<String>,
    replica_of: Option<String>,
}

impl Store {
    /// Open the store of `project` under `project_dir`, resolving its configuration at
    /// startup. A declared key source whose binding is absent refuses with no
    /// cleartext fallback (`store.encrypt.key-unbound`); a bound one refuses too, since
    /// this build links no at-rest cipher and writes no cleartext in its place.
    pub fn open(project_dir: &Path, project: &str) -> Result<Store> {
        check_segment_path(project, "project")?;
        let root = project_dir.join(store_root(project));
        let config_path = root.join("config.toml");
        let config: StoreConfig = match fs::read_to_string(&config_path) {
            Ok(text) => toml::from_str(&text)
                .map_err(|e| ContextError::Invalid(format!("{}: {e}", config_path.display())))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => StoreConfig::default(),
            Err(e) => return Err(ContextError::Io { path: config_path, source: e }),
        };
        if let Some(enc) = &config.encryption {
            check_key_source(&enc.key_source)?;
        }
        Ok(Store { root, config_node_id: config.node.and_then(|n| n.id), replica_of: config.replica.map(|r| r.of) })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The canonical store this store replicates, if it is a replica.
    pub fn replica_of(&self) -> Option<&str> {
        self.replica_of.as_deref()
    }

    /// Refuse a write verb on a replica, naming the canonical store (`store.replicate.write-refused`).
    pub fn check_writable(&self, verb: &str) -> Result<()> {
        match &self.replica_of {
            Some(canonical) => Err(StoreError::ReplicaWriteRefused(format!("`{verb}` writes, and this store is a read-only replica of `{canonical}`")).into()),
            None => Ok(()),
        }
    }

    /// `[node] id` from the store's configuration.
    pub fn configured_node_id(&self) -> Option<String> {
        self.config_node_id.clone()
    }

    /// `tables/<t>/`, for a table name of path-safe segments.
    pub fn table_dir(&self, table: &str) -> Result<PathBuf> {
        check_segment_path(table, "table")?;
        check_table_layout(table)?;
        Ok(self.root.join("tables").join(table))
    }

    /// The table's merged schema. A table no `schema.json` declares refuses
    /// (`store.lay-out.unknown-table`).
    pub fn schema(&self, table: &str) -> Result<Schema> {
        self.try_schema(table)?.ok_or_else(|| {
            StoreError::StoreUnknownTable(format!("no table `{table}`: no `schema.json` in the store declares it")).into()
        })
    }

    /// The table's merged schema, or `None` before its first batch.
    pub fn try_schema(&self, table: &str) -> Result<Option<Schema>> {
        let path = self.table_dir(table)?.join(SCHEMA_FILE);
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(ContextError::Io { path, source: e }),
        };
        let schema = serde_json::from_str::<Schema>(&text).map_err(|e| {
            StoreError::StoreManifestUnreadable(format!("table `{table}`: file `{}`: {e}", path.display()))
        })?;
        Ok(Some(schema))
    }

    /// Lock the table's schema for a read-merge-replace, so concurrent landings each merge
    /// into the other's result.
    pub fn lock_schema(&self, table: &str) -> Result<FileLock> {
        let dir = self.table_dir(table)?;
        fs::create_dir_all(&dir).at(&dir)?;
        FileLock::acquire(&dir.join(format!("{SCHEMA_FILE}.lock")), std::time::Duration::from_secs(LOCK_WAIT_SECS))
    }

    /// Replace `schema.json` with the merged schema (`store.lay-out.schema-file`).
    pub fn write_schema(&self, table: &str, schema: &Schema) -> Result<()> {
        let dir = self.table_dir(table)?;
        fs::create_dir_all(&dir).at(&dir)?;
        let text = serde_json::to_string_pretty(schema).expect("a schema serializes");
        replace_file(&dir.join(SCHEMA_FILE), text.as_bytes())
    }

    /// Every table a `schema.json` declares, sorted.
    pub fn tables(&self) -> Result<Vec<String>> {
        let base = self.root.join("tables");
        let mut out = Vec::new();
        let mut stack = vec![base.clone()];
        while let Some(dir) = stack.pop() {
            let entries = match fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(ContextError::Io { path: dir, source: e }),
            };
            for entry in entries {
                let path = entry.at(&dir)?.path();
                if path.join(SCHEMA_FILE).is_file() {
                    let rel = path.strip_prefix(&base).expect("under the tables root");
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
                // A table name carries `/`, so a table directory still holds tables below it.
                if path.is_dir() && path.file_name().is_some_and(|n| n != "data" && n != "requests") {
                    stack.push(path);
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// Every committed run of a table: each `data/runs/<run>/<node>/` holding a
    /// `_manifest.json`. A directory without one is in flight and joins nothing
    /// (`store.lay-out.uncommitted-run`); a manifest that fails to parse, or whose
    /// `run_id`, `node_id` or `table` disagrees with its path, refuses the table
    /// (`store.lay-out.manifest-unreadable`).
    pub fn committed_runs(&self, table: &str) -> Result<Vec<RunManifest>> {
        let runs_dir = self.table_dir(table)?.join("data").join("runs");
        let mut out = Vec::new();
        // One read of each (pipeline, node) commit log per call.
        let mut logs: std::collections::HashMap<(String, String), Vec<commit_log::CommitEntry>> = std::collections::HashMap::new();
        for run_dir in sorted_dirs(&runs_dir)? {
            for node_dir in sorted_dirs(&run_dir)? {
                let path = node_dir.join(MANIFEST_FILE);
                let text = match fs::read_to_string(&path) {
                    Ok(t) => t,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(ContextError::Io { path, source: e }),
                };
                let unreadable =
                    |why: String| StoreError::StoreManifestUnreadable(format!("table `{table}`: file `{}`: {why}", path.display()));
                let m: RunManifest = serde_json::from_str(&text).map_err(|e| unreadable(e.to_string()))?;
                let (run_seg, node_seg) = (file_name(&run_dir), file_name(&node_dir));
                if m.run_id != run_seg || m.node_id != node_seg || m.table != table {
                    return Err(unreadable(format!(
                        "names run `{}` on node `{}` of table `{}`, but sits at `{run_seg}/{node_seg}`",
                        m.run_id, m.node_id, m.table
                    ))
                    .into());
                }
                // A run written under the commit-log protocol is readable once its node's log
                // records it under its fence; a manifest without the mark reads as committed.
                if let (true, Some(fence), Some(pipeline)) = (m.logged, m.fence, m.pipeline_id.as_deref()) {
                    let key = (pipeline.to_string(), m.node_id.clone());
                    if !logs.contains_key(&key) {
                        let log = crate::commit_log::read(self, pipeline, &m.node_id)?;
                        logs.insert(key.clone(), log);
                    }
                    if !commit_log::committed(&logs[&key], table, &m.run_id, fence) {
                        continue;
                    }
                }
                out.push(m);
            }
        }
        Ok(out)
    }

    /// The pointer and the ETag of its bytes, or `None` before the first fold.
    pub fn pointer(&self, table: &str) -> Result<Option<(Pointer, String)>> {
        let path = self.table_dir(table)?.join(POINTER_FILE);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(ContextError::Io { path, source: e }),
        };
        let p: Pointer = serde_json::from_slice(&bytes).map_err(|e| {
            StoreError::StoreManifestUnreadable(format!("table `{table}`: file `{}`: {e}", path.display()))
        })?;
        Ok(Some((p, etag(&bytes))))
    }

    /// The pointer's ETag: the SHA-256 of its bytes, or `absent`.
    pub fn pointer_etag(&self, table: &str) -> Result<String> {
        let path = self.table_dir(table)?.join(POINTER_FILE);
        match fs::read(&path) {
            Ok(b) => Ok(etag(&b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ABSENT_ETAG.into()),
            Err(e) => Err(ContextError::Io { path, source: e }),
        }
    }

    pub fn snapshot_dir(&self, table: &str, id: &SnapshotId) -> Result<PathBuf> {
        Ok(self.table_dir(table)?.join("data").join("snapshots").join(id.to_string()))
    }

    /// The snapshot the pointer names and its `parent` chain, newest first, and whether
    /// the chain ends at a parent retention has collected. A reachable manifest that
    /// fails to parse refuses the table, as does one whose `parent` returns to a
    /// snapshot the walk already passed.
    pub fn chain(&self, table: &str) -> Result<(Vec<SnapshotManifest>, bool)> {
        let mut chain = Vec::new();
        let Some((ptr, _)) = self.pointer(table)? else { return Ok((chain, false)) };
        let mut seen = std::collections::BTreeSet::new();
        let mut next = Some(ptr.snapshot_id);
        while let Some(id) = next {
            if !seen.insert(id.to_string()) {
                return Err(StoreError::StoreManifestUnreadable(format!(
                    "table `{table}`: snapshot {id} is its own ancestor; the parent chain does not terminate"
                ))
                .into());
            }
            let path = self.snapshot_dir(table, &id)?.join(MANIFEST_FILE);
            let text = match fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && !chain.is_empty() => return Ok((chain, true)),
                Err(e) => return Err(ContextError::Io { path, source: e }),
            };
            let m: SnapshotManifest = serde_json::from_str(&text).map_err(|e| {
                StoreError::StoreManifestUnreadable(format!("table `{table}`: file `{}`: {e}", path.display()))
            })?;
            next = m.parent.clone();
            chain.push(m);
        }
        Ok((chain, false))
    }

    /// Everything a read or a fold resolves against, for one table.
    pub fn state(&self, decl: &TableDecl) -> Result<TableState> {
        let table = decl.name.as_str();
        let runs = self.committed_runs(table)?;
        let (chain, parent_collected) = self.chain(table)?;
        let present: BTreeSet<String> = runs.iter().map(RunManifest::key).collect();
        let run_collected = chain.iter().flat_map(|s| &s.includes_runs).any(|r| !present.contains(r.as_str()));
        Ok(TableState {
            table: table.to_string(),
            write_mode: decl.write_mode(),
            runs,
            chain,
            history_collected: parent_collected || run_collected,
        })
    }
}

pub const ABSENT_ETAG: &str = "absent";

/// How long a landing waits for a lock another landing holds before refusing.
pub const LOCK_WAIT_SECS: u64 = 30;

/// An exclusive lock: a lock file holding an advisory lock of the operating system,
/// the filesystem's stand-in for a conditional write. The kernel releases the lock when
/// its holder exits, so a crashed holder's file blocks nobody, a live holder is never
/// mistaken for a crashed one however long it works, and no waiter takes a lock beside
/// its holder. The holder removes the file on drop, and only while it still names the
/// file the holder locked.
#[derive(Debug)]
pub struct FileLock {
    path: PathBuf,
    file: fs::File,
}

impl FileLock {
    /// Take the lock, or `None` while a live holder has it.
    pub fn try_acquire(path: &Path) -> Result<Option<FileLock>> {
        loop {
            let file = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path).at(path)?;
            match file.try_lock() {
                Ok(()) => {}
                Err(fs::TryLockError::WouldBlock) => return Ok(None),
                Err(fs::TryLockError::Error(e)) => return Err(ContextError::Io { path: path.to_path_buf(), source: e }),
            }
            // A releasing holder removes the file between this open and this lock; the lock
            // then covers a file no path names, and the next open meets the current one.
            if names_file(path, &file)? {
                return Ok(Some(FileLock { path: path.to_path_buf(), file }));
            }
        }
    }

    /// Take the lock, waiting up to `wait` for a live holder to release it, or `None`
    /// while one still holds it. The caller decides what contention means.
    pub fn acquire_within(path: &Path, wait: std::time::Duration) -> Result<Option<FileLock>> {
        let start = std::time::Instant::now();
        loop {
            if let Some(lock) = FileLock::try_acquire(path)? {
                return Ok(Some(lock));
            }
            if start.elapsed() >= wait {
                return Ok(None);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Take the lock, waiting up to `wait`, and refuse when a live holder outlasts it.
    pub fn acquire(path: &Path, wait: std::time::Duration) -> Result<FileLock> {
        FileLock::acquire_within(path, wait)?
            .ok_or_else(|| ContextError::Invalid(format!("{} is held by another process", path.display())))
    }
}

impl Drop for FileLock {
    /// Remove the file while still holding its lock, so a waiter that opened it finds no
    /// path naming it and reopens; the lock releases when the handle closes.
    fn drop(&mut self) {
        if names_file(&self.path, &self.file).unwrap_or(false) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Whether `path` still names the file `file` opened.
fn names_file(path: &Path, file: &fs::File) -> Result<bool> {
    let at_path = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(ContextError::Io { path: path.to_path_buf(), source: e }),
    };
    let held = file.metadata().at(path)?;
    Ok(same_file(&at_path, &held))
}

#[cfg(unix)]
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

/// A platform without inode identity removes no open file, so the path names the held one.
#[cfg(not(unix))]
fn same_file(_: &fs::Metadata, _: &fs::Metadata) -> bool {
    true
}

pub(crate) fn etag(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Lowercase hex of `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Subdirectories of `dir`, sorted; a missing `dir` has none and a stray file is skipped.
pub(crate) fn sorted_dirs(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(ContextError::Io { path: dir.to_path_buf(), source: e }),
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry.at(dir)?.path();
        if path.is_dir() {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

/// Replace `path` with `bytes` through a private temporary file and one rename, so a
/// reader meets the old file or the new one.
pub fn replace_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = tmp_sibling(path);
    fs::write(&tmp, bytes).at(&tmp)?;
    fs::rename(&tmp, path).at(path)
}

/// Create `path` holding `bytes` only if nothing is there: the bytes land in a sibling
/// temporary file and a hard link publishes them, failing where `path` exists.
pub(crate) fn create_new_file(path: &Path, bytes: &[u8]) -> Result<bool> {
    let tmp = tmp_sibling(path);
    fs::write(&tmp, bytes).at(&tmp)?;
    let linked = fs::hard_link(&tmp, path);
    let _ = fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(ContextError::Io { path: path.to_path_buf(), source: e }),
    }
}

fn tmp_sibling(path: &Path) -> PathBuf {
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce).expect("the platform supplies randomness");
    let name = format!(".{}.{}.tmp", file_name(path), hex(&nonce));
    path.with_file_name(name)
}

/// A table or project name: `/`-separated segments of `[A-Za-z0-9._-]`, none `.` or `..`.
fn check_segment_path(name: &str, what: &str) -> Result<()> {
    if name.split('/').all(is_path_segment) {
        Ok(())
    } else {
        Err(ContextError::Invalid(format!("{what} name `{name}` is not `/`-separated segments of [A-Za-z0-9._-]")))
    }
}

/// Segments a table directory's own layout uses: its data and ledger trees, its files, and
/// the lock and temporary siblings written beside them (`store.lay-out.table-directory`).
const TABLE_LAYOUT_SEGMENTS: [&str; 4] = ["data", "requests", SCHEMA_FILE, POINTER_FILE];

/// Refuse a table name one of whose segments the table layout uses, so a nested table never
/// lands inside another table's tree and [`Store::tables`] lists every table it holds.
fn check_table_layout(table: &str) -> Result<()> {
    match table.split('/').find(|seg| TABLE_LAYOUT_SEGMENTS.contains(seg) || seg.starts_with('.') || seg.ends_with(".lock")) {
        Some(seg) => Err(ContextError::Invalid(format!(
            "table name `{table}` carries segment `{seg}`, which a table directory's own layout uses"
        ))),
        None => Ok(()),
    }
}

fn check_key_source(source: &str) -> Result<()> {
    if let Some(var) = source.strip_prefix("env:") {
        if std::env::var_os(var).is_none_or(|v| v.is_empty()) {
            return Err(StoreError::StoreEncryptionKeyUnbound(format!(
                "`[encryption] key_source = \"{source}\"` names `{var}`, which this process lacks"
            ))
            .into());
        }
    } else {
        return Err(StoreError::StoreEncryptionKeyUnbound(format!(
            "`[encryption] key_source = \"{source}\"` names a key-management service this build has no client for"
        ))
        .into());
    }
    Err(ContextError::Invalid(format!(
        "`[encryption] key_source = \"{source}\"` is bound, and this build links no at-rest cipher; it refuses rather than write cleartext"
    )))
}
