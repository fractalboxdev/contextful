//! The read face's session pool (`read.cache.session-pool`): a resolved session and its
//! open connections, reused while the whole key they were built under holds.
//!
//! The key splits in two. The principal part digests the admitted authority — its token id
//! and revocation epoch among it — the request zone and the bounds. The store part digests
//! the table set under `tables/` and, per granted table, its `schema.json` bytes, pointer
//! bytes, committed run manifests, snapshot directories and request-ledger files, and the
//! commit logs under `cursors/`. A change to any of them is another key, so no reuse
//! crosses a snapshot, schema, table set or revocation. A connection is reached only
//! through its entry, and an entry only through a live key.

use super::engine::SqlEngine;
use super::fault::ReadFault;
use crate::store::Store;
use contextful_core::read::cache::{SESSION_POOL_CONNECTIONS, SESSION_POOL_ENTRIES};
use contextful_core::store::bound_time::Bounds;
use contextful_policy::enforce::session::{Request, Session};
use contextful_policy::verify::AdmittedAuthority;
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

type Digest32 = [u8; 32];

/// What the pool has served since its face opened.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoolCounts {
    /// Sessions served from a pooled entry.
    pub session_hits: u64,
    /// Sessions resolved from the store.
    pub session_misses: u64,
    /// Engine connections opened for a session.
    pub engine_opens: u64,
}

/// One pooled session: its key, the session its connections were built for, and the
/// connections idle between statements.
struct Entry {
    principal: Digest32,
    store: Digest32,
    session: Session,
    idle: Mutex<Vec<SqlEngine>>,
}

/// Resolved sessions and their connections, oldest evicted first
/// (`read.cache.pool-entries`).
pub struct SessionPool {
    entries: Mutex<VecDeque<Arc<Entry>>>,
    hits: AtomicU64,
    misses: AtomicU64,
    opens: AtomicU64,
}

impl Default for SessionPool {
    fn default() -> SessionPool {
        SessionPool { entries: Mutex::new(VecDeque::new()), hits: AtomicU64::new(0), misses: AtomicU64::new(0), opens: AtomicU64::new(0) }
    }
}

/// The principal part of a key: the admitted authority with its token id and revocation
/// epoch, the request zone and the bounds.
pub(crate) fn principal(authority: &AdmittedAuthority, request: &Request<'_>, bounds: Bounds) -> Digest32 {
    let mut h = Sha256::new();
    field(&mut h, authority.to_json().as_bytes());
    field(&mut h, authority.credential_id().as_bytes());
    field(&mut h, &authority.epoch().to_be_bytes());
    field(&mut h, request.zone.unwrap_or("\u{0}none").as_bytes());
    field(&mut h, format!("{bounds:?}").as_bytes());
    h.finalize().into()
}

/// The store part of a key: the table set, then per granted table everything a resolution
/// reads, then the commit logs that gate run visibility.
pub(crate) fn store_state(store: &Store, tables: &[String], granted: &[String]) -> Result<Digest32, ReadFault> {
    let mut h = Sha256::new();
    for t in tables {
        field(&mut h, t.as_bytes());
    }
    field(&mut h, b"\x00granted");
    for t in granted {
        let dir = store.table_dir(t)?;
        field(&mut h, t.as_bytes());
        content(&mut h, &dir.join(contextful_core::store::lay_out::SCHEMA_FILE));
        content(&mut h, &dir.join(contextful_core::store::lay_out::POINTER_FILE));
        tree(&mut h, &dir.join("data").join("runs"), 2)?;
        tree(&mut h, &dir.join("data").join("snapshots"), 1)?;
        for f in crate::ledger::files(store, t)? {
            stamp(&mut h, &f);
        }
    }
    field(&mut h, b"\x00cursors");
    tree(&mut h, &store.root().join("cursors"), 2)?;
    Ok(h.finalize().into())
}

/// Whether `a` and `b` build identical connections: [`SqlEngine::open`] writes the subject
/// and tenant rows and each relation's and ledger's name, statement and files, and nothing
/// else of a session.
fn same_setup(a: &Session, b: &Session) -> bool {
    a.subject_relation() == b.subject_relation()
        && a.subject_row() == b.subject_row()
        && a.tenant_rows() == b.tenant_rows()
        && a.relations().chain(a.ledgers()).eq(b.relations().chain(b.ledgers()))
}

/// A length-prefixed field, so adjacent fields never run together.
fn field(h: &mut Sha256, bytes: &[u8]) {
    h.update((bytes.len() as u64).to_be_bytes());
    h.update(bytes);
}

/// A file's bytes, or its absence.
fn content(h: &mut Sha256, path: &Path) {
    match std::fs::read(path) {
        Ok(b) => field(h, &Sha256::digest(&b)),
        Err(_) => field(h, b"\x00absent"),
    }
}

/// A file's path, length and modification time, or its absence.
fn stamp(h: &mut Sha256, path: &Path) {
    field(h, path.to_string_lossy().as_bytes());
    match std::fs::metadata(path) {
        Ok(m) => {
            let at = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
            h.update(m.len().to_be_bytes());
            h.update(at.to_be_bytes());
        }
        Err(_) => field(h, b"\x00absent"),
    }
}

/// Every entry `depth` directories below `dir`, sorted, each stamped with its files.
fn tree(h: &mut Sha256, dir: &Path, depth: usize) -> Result<(), ReadFault> {
    let mut entries: Vec<std::path::PathBuf> = match std::fs::read_dir(dir) {
        Ok(e) => e.filter_map(|e| e.ok().map(|e| e.path())).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(crate::ContextError::Io { path: dir.to_path_buf(), source: e }.into()),
    };
    entries.sort();
    for p in entries {
        if depth > 0 && p.is_dir() {
            field(h, p.to_string_lossy().as_bytes());
            tree(h, &p, depth - 1)?;
        } else {
            stamp(h, &p);
        }
    }
    Ok(())
}

impl SessionPool {
    /// The pooled session under `principal` and `store`, or `resolve`'s, pooled. A miss
    /// evicts the principal's entries under an older store state, then the oldest entries
    /// past capacity.
    pub(crate) fn session(
        &self,
        principal: Digest32,
        store: Digest32,
        resolve: impl FnOnce() -> Result<Session, ReadFault>,
    ) -> Result<Session, ReadFault> {
        let found = self.lock().iter().find(|e| e.principal == principal && e.store == store).map(|e| e.session.clone());
        if let Some(session) = found {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(session);
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        let session = resolve()?;
        let entry = Arc::new(Entry { principal, store, session: session.clone(), idle: Mutex::new(Vec::new()) });
        let mut entries = self.lock();
        entries.retain(|e| e.principal != principal);
        entries.push_back(entry);
        while entries.len() > SESSION_POOL_ENTRIES {
            entries.pop_front();
        }
        Ok(session)
    }

    /// A connection for `session`: an idle one of the pooled entry building the same
    /// connection, or a new one, returned to that entry after the statement. A session no
    /// entry holds gets a connection of its own.
    pub(crate) fn engine(&self, session: &Session) -> Result<Lease, ReadFault> {
        let home = self.lock().iter().rev().find(|e| same_setup(&e.session, session)).cloned();
        let idle = home.as_ref().and_then(|e| e.idle.lock().expect("the idle lock").pop());
        let engine = match idle {
            Some(engine) => engine,
            None => {
                self.opens.fetch_add(1, Ordering::Relaxed);
                SqlEngine::open(session)?
            }
        };
        Ok(Lease { engine: Some(engine), home })
    }

    pub fn counts(&self) -> PoolCounts {
        PoolCounts {
            session_hits: self.hits.load(Ordering::Relaxed),
            session_misses: self.misses.load(Ordering::Relaxed),
            engine_opens: self.opens.load(Ordering::Relaxed),
        }
    }

    /// Pooled entries.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Idle connections across every entry.
    pub fn idle(&self) -> usize {
        self.lock().iter().map(|e| e.idle.lock().expect("the idle lock").len()).sum()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<Arc<Entry>>> {
        self.entries.lock().expect("the pool lock")
    }
}

/// A connection held for one call. On drop it returns to its entry while the entry keeps
/// fewer than `SESSION_POOL_CONNECTIONS` idle (`read.cache.pool-connections`), and closes
/// otherwise.
pub(crate) struct Lease {
    engine: Option<SqlEngine>,
    home: Option<Arc<Entry>>,
}

impl std::ops::Deref for Lease {
    type Target = SqlEngine;
    fn deref(&self) -> &SqlEngine {
        self.engine.as_ref().expect("a lease holds its engine until dropped")
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let (Some(engine), Some(home)) = (self.engine.take(), self.home.as_ref()) else { return };
        let mut idle = home.idle.lock().expect("the idle lock");
        if idle.len() < SESSION_POOL_CONNECTIONS {
            idle.push(engine);
        }
    }
}
