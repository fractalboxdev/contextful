//! The read face's result cache (`read.cache`): a statement's response, reused while its
//! whole key holds, under a byte budget the process declares.
//!
//! The key digests the session's principal — credential id, revocation epoch, grants,
//! subject row, tenant rows, zone and incognito state — then each touched relation's
//! name, compiled statement and the files it reads, every request-ledger file stamped by
//! length and modification time, then the bounds, the statement text, its bound parameter
//! values and the applied row ceiling (`read.cache.result-key`). The files a relation
//! reads are its read frontier: a landed run, a fold or a schema edit changes them, so an
//! entry filled under an older frontier is never computed as a key again
//! (`read.cache.frontier-invalidates`). A statement calling a function whose answer
//! moves with no change to that key executes uncached (`read.cache.volatile-bypasses`).

use super::engine::SqlEngine;
use super::pool::{field, stamp, Digest32};
use contextful_core::read::respond::Response;
use contextful_core::read::template::Bindings;
use contextful_core::store::bound_time::Bounds;
use contextful_policy::enforce::session::Session;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// What the result cache has served since its face opened.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResultCounts {
    /// Responses served from an entry; no statement executed.
    pub hits: u64,
    /// Cacheable statements executed because no live entry matched.
    pub misses: u64,
    /// Statements executed uncached: a touched table not opted in, or tagged private.
    pub bypassed: u64,
    /// Resident entries.
    pub entries: u64,
    /// Bytes the resident entries hold, at most the budget.
    pub resident_bytes: u64,
}

struct Entry {
    response: Response,
    bytes: u64,
    expires: Instant,
    used: u64,
}

#[derive(Default)]
struct Resident {
    entries: HashMap<Digest32, Entry>,
    /// Entries by last use, least recent first.
    recency: BTreeMap<u64, Digest32>,
    tick: u64,
    bytes: u64,
}

impl Resident {
    fn remove(&mut self, key: &Digest32) {
        if let Some(e) = self.entries.remove(key) {
            self.recency.remove(&e.used);
            self.bytes -= e.bytes;
        }
    }
}

/// Responses keyed on everything that decides them, least recently used evicted first
/// past the byte budget (`read.cache.budget`).
pub struct ResultCache {
    budget: u64,
    resident: Mutex<Resident>,
    hits: AtomicU64,
    misses: AtomicU64,
    bypassed: AtomicU64,
}

impl ResultCache {
    /// A cache holding at most `budget` bytes of responses.
    pub fn new(budget: u64) -> ResultCache {
        ResultCache {
            budget,
            resident: Mutex::new(Resident::default()),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            bypassed: AtomicU64::new(0),
        }
    }

    /// The live entry under `key`, marked most recently used. An expired entry leaves the
    /// cache and misses.
    pub(crate) fn get(&self, key: &Digest32) -> Option<Response> {
        let mut r = self.lock();
        let now = Instant::now();
        let live = r.entries.get(key).map(|e| e.expires > now);
        match live {
            Some(true) => {
                r.tick += 1;
                let tick = r.tick;
                let e = r.entries.get_mut(key).expect("a live entry is resident");
                let previous = std::mem::replace(&mut e.used, tick);
                let response = e.response.clone();
                r.recency.remove(&previous);
                r.recency.insert(tick, *key);
                self.hits.fetch_add(1, Ordering::Relaxed);
                Some(response)
            }
            found => {
                if found.is_some() {
                    r.remove(key);
                }
                self.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// Keep `response` under `key` for `ttl`, evicting least recently used entries until
    /// it fits. A response larger than the whole budget is not kept.
    pub(crate) fn put(&self, key: Digest32, response: &Response, ttl: Duration) {
        let bytes = serde_json::to_vec(response).expect("a response serializes").len() as u64
            + response.probe_row.as_ref().map_or(0, |row| serde_json::to_vec(row).expect("a probe row serializes").len() as u64)
            + key.len() as u64;
        if bytes > self.budget {
            return;
        }
        let mut r = self.lock();
        r.remove(&key);
        while r.bytes + bytes > self.budget {
            let Some((_, oldest)) = r.recency.pop_first() else { break };
            r.remove(&oldest);
        }
        r.tick += 1;
        let used = r.tick;
        r.recency.insert(used, key);
        r.bytes += bytes;
        r.entries.insert(key, Entry { response: response.clone(), bytes, expires: Instant::now() + ttl, used });
    }

    /// Count a statement executed outside the cache.
    pub(crate) fn bypass(&self) {
        self.bypassed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn budget(&self) -> u64 {
        self.budget
    }

    pub fn counts(&self) -> ResultCounts {
        let r = self.lock();
        ResultCounts {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            bypassed: self.bypassed.load(Ordering::Relaxed),
            entries: r.entries.len() as u64,
            resident_bytes: r.bytes,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Resident> {
        self.resident.lock().expect("the result cache lock")
    }
}

/// What one statement's result is keyed on (`read.cache.result-key`).
pub(crate) struct Statement<'a> {
    pub touched: &'a BTreeSet<String>,
    pub text: &'a str,
    pub parameters: &'a Bindings,
    pub ceiling: u64,
    pub bounds: Bounds,
}

/// The key of one statement's result under `session`.
pub(crate) fn key(session: &Session, statement: &Statement<'_>) -> Digest32 {
    let mut h = Sha256::new();
    field(&mut h, session.credential_id().as_bytes());
    field(&mut h, &session.epoch().to_be_bytes());
    field(&mut h, format!("{:?}", session.grants()).as_bytes());
    field(&mut h, format!("{:?}", session.subject_row()).as_bytes());
    field(&mut h, format!("{:?}", session.tenant_rows()).as_bytes());
    field(&mut h, format!("{:?}", session.zone()).as_bytes());
    field(&mut h, &[u8::from(session.incognito())]);
    for name in statement.touched {
        field(&mut h, name.as_bytes());
        if let Some(r) = session.relation(name) {
            field(&mut h, r.sql().as_bytes());
            for f in r.files() {
                field(&mut h, f.as_bytes());
            }
        } else if let Some(l) = session.ledgers().find(|l| l.name() == name) {
            field(&mut h, l.sql().as_bytes());
            for f in l.files() {
                stamp(&mut h, Path::new(f));
            }
        }
    }
    field(&mut h, format!("{:?}", statement.bounds).as_bytes());
    field(&mut h, statement.text.as_bytes());
    field(&mut h, format!("{:?}", statement.parameters).as_bytes());
    field(&mut h, &statement.ceiling.to_be_bytes());
    h.finalize().into()
}

/// SQL value keywords the engine parses as a bare column reference and binds to a
/// function of the session or the clock where no column carries the name.
const VALUE_KEYWORDS: [&str; 11] = [
    "current_catalog",
    "current_date",
    "current_role",
    "current_schema",
    "current_time",
    "current_timestamp",
    "current_user",
    "localtime",
    "localtimestamp",
    "session_user",
    "user",
];

/// Names whose call answers differently across two executions of one statement: every
/// function the engine's catalog marks other than `CONSISTENT`, the value keywords, and
/// each macro whose body reaches one of them. `None` where the catalog does not read.
fn volatile_names() -> Option<&'static BTreeSet<String>> {
    static NAMES: OnceLock<Option<BTreeSet<String>>> = OnceLock::new();
    NAMES
        .get_or_init(|| {
            let catalog = SqlEngine::bare().and_then(|e| e.function_catalog()).ok()?;
            let mut names: BTreeSet<String> = VALUE_KEYWORDS.iter().map(|k| k.to_string()).collect();
            names.extend(catalog.iter().filter(|(_, s, _)| s.as_deref().is_some_and(|s| s != "CONSISTENT")).map(|(n, _, _)| n.clone()));
            let macros: Vec<(&String, BTreeSet<String>)> = catalog
                .iter()
                .filter_map(|(n, _, body)| {
                    let words = body.as_deref()?.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).map(str::to_ascii_lowercase);
                    Some((n, words.collect()))
                })
                .collect();
            loop {
                let reached: Vec<String> =
                    macros.iter().filter(|(n, words)| !names.contains(*n) && words.iter().any(|w| names.contains(w))).map(|(n, _)| (*n).clone()).collect();
                if reached.is_empty() {
                    break Some(names);
                }
                names.extend(reached);
            }
        })
        .as_ref()
}

/// Whether the engine's serialization of a statement calls a volatile name anywhere in
/// its tree: a function or macro, or a one-part column reference spelling one. A catalog
/// that does not read counts every statement volatile.
pub(crate) fn volatile(tree: &Value) -> bool {
    let Some(names) = volatile_names() else { return true };
    fn walk(v: &Value, names: &BTreeSet<String>) -> bool {
        match v {
            Value::Array(a) => a.iter().any(|c| walk(c, names)),
            Value::Object(o) => {
                let called = o.get("function_name").and_then(Value::as_str);
                let bare = match o.get("column_names").and_then(Value::as_array).map(Vec::as_slice) {
                    Some([one]) if o.get("class").and_then(Value::as_str) == Some("COLUMN_REF") => one.as_str(),
                    _ => None,
                };
                [called, bare].into_iter().flatten().any(|n| names.contains(&n.to_ascii_lowercase())) || o.values().any(|c| walk(c, names))
            }
            _ => false,
        }
    }
    walk(tree, names)
}
