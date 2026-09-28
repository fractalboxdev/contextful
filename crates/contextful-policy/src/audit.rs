//! `disclosure.record` and `disclosure.attest`: the hash-linked audit chain, its numbered
//! segment files, the chain tip, the signed root closing each segment, and verification.
//!
//! Layout under the audit directory:
//!
//! ```text
//! segments/000001.jsonl       one entry per line, seq ascending from 1
//! segments/000001.root.json   {root, count, signature} closing the segment
//! chain.tip                   a signed {seq, entry_hash} at or behind the chain end
//! audit.lock                  held by the one process writing the log
//! ```
//!
//! Appends commit in groups (`disclosure.record.group-commit`): concurrent appends queue
//! behind the group in flight, and the next group writes every queued entry and issues one
//! data sync of its segment file. A group creating a segment file adds one sync of the
//! segments directory (`disclosure.record.segment-open`). The tip signs at segment close,
//! after [`AUDIT_TIP_IDLE`] without an append, at export and when the log closes
//! (`disclosure.record.tip-signing`), so entries past the tip are an unsigned tail whose
//! truncation only a replicated root catches.
//!
//! Roots and the tip sign through the signing port a mint signs through
//! (`authority.issue.signing-port`), under either scheme, so a chain truncated under a
//! rewritten tip fails signed verification. A replayed older signed tip is caught only
//! against a replicated root (`disclosure.attest.root-replication`).
//!
//! Segment `n` holds seq `(n - 1) * 4096 + 1` through `n * 4096`. An entry's digest is
//! SHA-256 over its seq (8 bytes, little-endian), its `prev_hash` text and the compact
//! canonical JSON of its attributes: compact, object keys in byte order at every depth,
//! so the digest is independent of the order attributes were built in and of how the
//! JSON library orders a map.

use crate::issue::SignerKey;
use contextful_core::ports::SigningPort;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::fmt::Display;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The `prev_hash` of the first entry.
pub const GENESIS: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

/// Entries a segment holds before its signed root closes it.
pub const AUDIT_SEGMENT_ENTRIES: u64 = 4096;

/// Quiet time after the last append at which the log signs its tip
/// (`disclosure.record.tip-signing`).
pub const AUDIT_TIP_IDLE: Duration = Duration::from_secs(1);

/// A refusal of the audit chain.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuditError {
    /// A disagreeing digest, a sequence gap, or an absent chain beside a tip or root;
    /// `index` is the seq of the earliest failure. (`disclosure.attest.broken-chain`)
    #[error("AuditChainBroken: entry {index}: {reason}")]
    AuditChainBroken { index: u64, reason: String },
    /// An entry that did not reach local durable storage; the log keeps its prior tip.
    /// (`disclosure.record.unpersisted-entry`)
    #[error("AuditEntryUnpersisted: {0}")]
    AuditEntryUnpersisted(String),
    /// An audit log another process holds open. (`disclosure.record.single-writer`)
    #[error("AuditLogHeld: {0}")]
    AuditLogHeld(String),
    /// An audit file the process cannot read or parse.
    #[error("{0}")]
    Io(String),
}

fn broken(index: u64, reason: impl Into<String>) -> AuditError {
    AuditError::AuditChainBroken { index, reason: reason.into() }
}

fn unreadable(path: &Path) -> impl Fn(std::io::Error) -> AuditError + '_ {
    move |e| AuditError::Io(format!("{}: {e}", path.display()))
}

/// One entry: the audited attributes and their linkage to the entry before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub seq: u64,
    pub prev_hash: String,
    pub attributes: Value,
    pub entry_hash: String,
}

impl AuditEntry {
    /// The entry at `seq`, linked to `prev_hash`.
    pub fn link(seq: u64, prev_hash: &str, attributes: Value) -> AuditEntry {
        let entry_hash = entry_digest(seq, prev_hash, &attributes);
        AuditEntry { seq, prev_hash: prev_hash.to_string(), attributes, entry_hash }
    }

    /// Whether `entry_hash` is the digest of the entry's other fields.
    pub fn digest_agrees(&self) -> bool {
        entry_digest(self.seq, &self.prev_hash, &self.attributes) == self.entry_hash
    }

    fn tip(&self) -> ChainTip {
        ChainTip { seq: self.seq, entry_hash: self.entry_hash.clone() }
    }
}

fn entry_digest(seq: u64, prev_hash: &str, attributes: &Value) -> String {
    let mut h = Sha256::new();
    h.update(seq.to_le_bytes());
    h.update(prev_hash.as_bytes());
    let mut canonical = Vec::new();
    write_canonical(attributes, &mut canonical);
    h.update(canonical);
    format!("sha256:{}", hex::encode(h.finalize()))
}

/// Compact JSON with object keys sorted at every depth, independent of the map order
/// the JSON library's features select.
fn write_canonical(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push(b'{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                serde_json::to_writer(&mut *out, key).expect("a string serializes");
                out.push(b':');
                write_canonical(&map[key], out);
            }
            out.push(b'}');
        }
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_canonical(item, out);
            }
            out.push(b']');
        }
        leaf => serde_json::to_writer(&mut *out, leaf).expect("a JSON value serializes"),
    }
}

/// The last accepted entry: `{seq: 0, entry_hash: GENESIS}` for an empty chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainTip {
    pub seq: u64,
    pub entry_hash: String,
}

impl ChainTip {
    pub fn genesis() -> ChainTip {
        ChainTip { seq: 0, entry_hash: GENESIS.to_string() }
    }
}

/// `contextful.query.hash`: HMAC-SHA256 over the statement text under the project audit
/// key, so a guessable statement does not reverse by dictionary.
pub fn query_digest(audit_key: &[u8], statement: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(audit_key).expect("HMAC accepts a key of any length");
    mac.update(statement.as_bytes());
    format!("hmac-sha256:{}", hex::encode(mac.finalize().into_bytes()))
}

/// The root closing one segment: its last entry's digest and its entry count, signed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedRoot {
    pub root: String,
    pub count: u64,
    /// Hex-encoded signing-port signature over [`SignedRoot::message`], in the port's
    /// encoding for its scheme.
    pub signature: String,
}

impl SignedRoot {
    /// The signed bytes: `contextful.audit.root`, the count and the root, one per line,
    /// so a root cannot be replayed under another count.
    pub fn message(root: &str, count: u64) -> Vec<u8> {
        format!("contextful.audit.root\n{count}\n{root}").into_bytes()
    }

    /// Sign `root` over `count` entries through `signer`.
    pub fn sign(root: &str, count: u64, signer: &dyn SigningPort) -> Result<SignedRoot, String> {
        let signature = signer.sign(&Self::message(root, count)).map_err(|e| e.to_string())?;
        Ok(SignedRoot { root: root.to_string(), count, signature: hex::encode(signature) })
    }

    /// Whether the signature verifies under `key`.
    pub fn verify(&self, key: &SignerKey) -> bool {
        hex::decode(&self.signature).is_ok_and(|s| key.verifies(&Self::message(&self.root, self.count), &s))
    }
}

/// The last accepted entry as `chain.tip` records it, signed so a truncation under a
/// rewritten tip fails signed verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedTip {
    pub seq: u64,
    pub entry_hash: String,
    /// Hex-encoded signing-port signature over [`SignedTip::message`]; absent on a tip
    /// written by no signer, which signed verification refuses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

impl SignedTip {
    /// The signed bytes: `contextful.audit.tip`, the seq and the entry digest, one per
    /// line, apart from a root's so neither replays as the other.
    pub fn message(seq: u64, entry_hash: &str) -> Vec<u8> {
        format!("contextful.audit.tip\n{seq}\n{entry_hash}").into_bytes()
    }

    pub fn sign(seq: u64, entry_hash: &str, signer: &dyn SigningPort) -> Result<SignedTip, String> {
        let signature = signer.sign(&Self::message(seq, entry_hash)).map_err(|e| e.to_string())?;
        Ok(SignedTip { seq, entry_hash: entry_hash.to_string(), signature: Some(hex::encode(signature)) })
    }

    /// Whether the signature verifies under `key`.
    pub fn verify(&self, key: &SignerKey) -> bool {
        let Some(Ok(bytes)) = self.signature.as_ref().map(hex::decode) else { return false };
        key.verifies(&Self::message(self.seq, &self.entry_hash), &bytes)
    }
}

fn segments_dir(dir: &Path) -> PathBuf {
    dir.join("segments")
}

fn segment_path(dir: &Path, n: u64) -> PathBuf {
    segments_dir(dir).join(format!("{n:06}.jsonl"))
}

fn root_path(dir: &Path, n: u64) -> PathBuf {
    segments_dir(dir).join(format!("{n:06}.root.json"))
}

fn tip_path(dir: &Path) -> PathBuf {
    dir.join("chain.tip")
}

/// The segment holding `seq` (1-based).
fn segment_of(seq: u64) -> u64 {
    (seq - 1) / AUDIT_SEGMENT_ENTRIES + 1
}

fn first_seq(n: u64) -> u64 {
    (n - 1) * AUDIT_SEGMENT_ENTRIES + 1
}

/// Segment numbers present as entry files and as root files.
fn listing(dir: &Path) -> Result<(BTreeSet<u64>, BTreeSet<u64>), AuditError> {
    let (mut segments, mut roots) = (BTreeSet::new(), BTreeSet::new());
    let seg_dir = segments_dir(dir);
    let read = match fs::read_dir(&seg_dir) {
        Ok(read) => read,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((segments, roots)),
        Err(e) => return Err(unreadable(&seg_dir)(e)),
    };
    for item in read {
        let name = item.map_err(unreadable(&seg_dir))?.file_name();
        let name = name.to_string_lossy();
        if let Some(n) = name.strip_suffix(".root.json").and_then(|s| s.parse::<u64>().ok()) {
            roots.insert(n);
        } else if let Some(n) = name.strip_suffix(".jsonl").and_then(|s| s.parse::<u64>().ok()) {
            segments.insert(n);
        }
    }
    Ok((segments, roots))
}

fn read_tip(dir: &Path) -> Result<Option<SignedTip>, AuditError> {
    let path = tip_path(dir);
    match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| AuditError::Io(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(unreadable(&path)(e)),
    }
}

/// Verify every entry's digest and linkage, sequence continuity across segments, each
/// root's digest and count, and the tip against the chain. Returns the chain end.
pub fn verify(dir: &Path) -> Result<ChainTip, AuditError> {
    walk(dir, None)
}

/// [`verify`], and every root's signature and the tip's under `key`: the check opening a
/// log runs, and the offline verifier's. A non-empty chain needs a tip that verifies.
pub fn verify_signed(dir: &Path, key: &SignerKey) -> Result<ChainTip, AuditError> {
    walk(dir, Some(key))
}

fn walk(dir: &Path, key: Option<&SignerKey>) -> Result<ChainTip, AuditError> {
    let (segments, roots) = listing(dir)?;
    let tip_file = read_tip(dir)?;
    let Some(&last) = segments.last() else {
        if let Some(tip) = tip_file.as_ref().filter(|t| t.seq > 0) {
            return Err(broken(1, format!("chain.tip names seq {} and no segment exists", tip.seq)));
        }
        if let Some(&n) = roots.first() {
            return Err(broken(first_seq(n), format!("a signed root closes segment {n} and no segment exists")));
        }
        return Ok(ChainTip::genesis());
    };
    let mut end = ChainTip::genesis();
    for n in 1..=last {
        let path = segment_path(dir, n);
        if !segments.contains(&n) {
            return Err(broken(first_seq(n), format!("segment {n} is absent")));
        }
        let text = fs::read_to_string(&path).map_err(unreadable(&path))?;
        for line in text.lines() {
            let index = end.seq + 1;
            let entry: AuditEntry =
                serde_json::from_str(line).map_err(|e| broken(index, format!("segment {n}: the line does not parse: {e}")))?;
            if entry.seq != index {
                return Err(broken(index, format!("segment {n}: seq {} where {index} follows", entry.seq)));
            }
            if segment_of(entry.seq) != n {
                return Err(broken(index, format!("seq {index} lies outside segment {n}")));
            }
            if entry.prev_hash != end.entry_hash {
                return Err(broken(index, "prev_hash does not link to the entry before"));
            }
            if !entry.digest_agrees() {
                return Err(broken(index, "entry_hash disagrees with the entry"));
            }
            if let Some(tip) = tip_file.as_ref().filter(|t| t.seq == index) {
                if tip.entry_hash != entry.entry_hash {
                    return Err(broken(index, "chain.tip disagrees with the entry it names"));
                }
            }
            end = entry.tip();
        }
        let count = end.seq + 1 - first_seq(n);
        let closing = end.seq.max(first_seq(n));
        if roots.contains(&n) {
            let rpath = root_path(dir, n);
            let text = fs::read_to_string(&rpath).map_err(unreadable(&rpath))?;
            let root: SignedRoot = serde_json::from_str(&text)
                .map_err(|e| broken(closing, format!("the root of segment {n} does not parse: {e}")))?;
            if root.count != AUDIT_SEGMENT_ENTRIES || root.count != count || root.root != end.entry_hash {
                return Err(broken(closing, format!("the signed root of segment {n} disagrees with its entries")));
            }
            if key.is_some_and(|k| !root.verify(k)) {
                return Err(broken(closing, format!("the root signature of segment {n} does not verify")));
            }
        } else if n < last {
            return Err(broken(closing, format!("segment {n} is followed by another and carries no signed root")));
        }
    }
    if let Some(&n) = roots.range(last + 1..).next() {
        return Err(broken(first_seq(n), format!("a signed root closes segment {n} and the segment is absent")));
    }
    if let Some(tip) = tip_file.as_ref().filter(|t| t.seq > end.seq) {
        return Err(broken(end.seq + 1, format!("chain.tip names seq {} beyond the chain end", tip.seq)));
    }
    if let Some(key) = key.filter(|_| end.seq > 0) {
        match &tip_file {
            None => return Err(broken(1, "the chain carries entries and no chain.tip")),
            Some(tip) if !tip.verify(key) => {
                return Err(broken(tip.seq.max(1), "the chain.tip signature does not verify"));
            }
            Some(_) => {}
        }
    }
    Ok(end)
}

/// What one audit sync makes durable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fsync {
    /// The lines an append group wrote to segment `n`.
    Segment(u64),
    /// The segments directory, after the file of segment `n` is created.
    SegmentOpen(u64),
    /// Segment `n`, cut back to its last whole line on open or to its length before a
    /// failed group.
    Truncate(u64),
    /// The signed root of segment `n`, or its directory after the root is renamed in.
    Root(u64),
    /// `chain.tip`, or its directory after the tip is renamed in.
    Tip,
}

/// The port every audit sync runs through.
pub trait FsyncPort: Send + Sync {
    /// Make `file`, opened on what `what` names, durable.
    fn sync(&self, what: Fsync, file: &File) -> std::io::Result<()>;
}

/// Syncs on the local filesystem: file data alone for segment lines, the whole file
/// otherwise. On macOS each is an `F_FULLFSYNC`.
#[derive(Debug, Clone, Copy, Default)]
pub struct FileFsync;

impl FsyncPort for FileFsync {
    fn sync(&self, what: Fsync, file: &File) -> std::io::Result<()> {
        match what {
            Fsync::Segment(_) | Fsync::Truncate(_) => file.sync_data(),
            _ => file.sync_all(),
        }
    }
}

/// How a log commits and when it signs its tip.
#[derive(Clone)]
pub struct AuditOptions {
    /// Quiet time after the last append at which the tip signs.
    pub idle: Duration,
    pub fsync: Arc<dyn FsyncPort>,
}

impl Default for AuditOptions {
    fn default() -> AuditOptions {
        AuditOptions { idle: AUDIT_TIP_IDLE, fsync: Arc::new(FileFsync) }
    }
}

impl std::fmt::Debug for AuditOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditOptions").field("idle", &self.idle).finish_non_exhaustive()
    }
}

/// Truncate the last segment to its last newline when it ends mid-line. An entry line is
/// written whole and newline-terminated, and an append returns only after its segment
/// syncs, so unterminated bytes are a write a crash cut short and never acknowledged. A
/// newline-terminated line that does not parse is left for [`verify`].
fn drop_torn_tail(dir: &Path, fsync: &dyn FsyncPort) -> Result<(), AuditError> {
    let (segments, _) = listing(dir)?;
    let Some(&last) = segments.last() else { return Ok(()) };
    let path = segment_path(dir, last);
    let bytes = fs::read(&path).map_err(unreadable(&path))?;
    if bytes.last().is_none_or(|b| *b == b'\n') {
        return Ok(());
    }
    let keep = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1) as u64;
    let f = OpenOptions::new().write(true).open(&path).map_err(unreadable(&path))?;
    f.set_len(keep).and_then(|_| fsync.sync(Fsync::Truncate(last), &f)).map_err(unreadable(&path))
}

/// Write `bytes` to `path` durably: a sibling temporary file, synced, renamed into
/// place, and the directory synced, each sync as `what`.
fn write_durable(path: &Path, bytes: &[u8], fsync: &dyn FsyncPort, what: Fsync) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    let attempt = (|| -> std::io::Result<()> {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        fsync.sync(what, &f)?;
        fs::rename(&tmp, path)?;
        fsync.sync(what, &File::open(path.parent().unwrap_or(Path::new(".")))?)
    })();
    if attempt.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    attempt.map_err(fail(path))
}

fn fail<E: Display>(path: &Path) -> impl Fn(E) -> String + '_ {
    move |e| format!("{}: {e}", path.display())
}

/// What an append group touched on disk, reversed when it fails to persist.
#[derive(Default)]
struct Undo {
    truncate: Option<(u64, PathBuf, u64)>,
    created: Vec<PathBuf>,
}

impl Undo {
    /// Reverse the group, reporting the first step that could not be reversed.
    fn apply(self, fsync: &dyn FsyncPort) -> Result<(), String> {
        let mut first = None;
        if let Some((n, path, len)) = self.truncate {
            let r = OpenOptions::new().write(true).open(&path).and_then(|f| f.set_len(len).and_then(|_| fsync.sync(Fsync::Truncate(n), &f)));
            if let Err(e) = r {
                first = Some(format!("{}: {e}", path.display()));
            }
        }
        for path in self.created.iter().rev() {
            match fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound && first.is_none() => first = Some(format!("{}: {e}", path.display())),
                _ => {}
            }
        }
        first.map_or(Ok(()), Err)
    }
}

/// An append waiting for its group.
struct Member {
    ticket: u64,
    batch: Vec<Value>,
}

/// What an append group left behind: its entries, the new chain end, and the tip it signed
/// when it closed a segment.
type Committed = (Vec<AuditEntry>, ChainTip, Option<ChainTip>);

struct State {
    /// The last durable entry.
    tip: ChainTip,
    /// The entry `chain.tip` names.
    signed: ChainTip,
    queue: Vec<Member>,
    next_ticket: u64,
    done: HashMap<u64, Result<Vec<AuditEntry>, AuditError>>,
    /// Set while one thread writes: a group's leader, an export, or the idle signer.
    busy: bool,
    /// Set when a failed group could not be reversed: the files may hold lines past the
    /// tip, so every later append refuses until the log is reopened and verified.
    poisoned: Option<String>,
    last_append: Instant,
    closing: bool,
}

struct Inner<S> {
    dir: PathBuf,
    signer: S,
    fsync: Arc<dyn FsyncPort>,
    idle: Duration,
    /// The exclusive lock on `audit.lock`, held for the log's lifetime.
    _lock: File,
    state: Mutex<State>,
    turn: Condvar,
}

/// The node's audit log: appends link to the tip, commit in groups sharing one segment
/// sync, return only once durable, and close each full segment under a signed root.
pub struct AuditLog<S: SigningPort + Send + Sync + 'static> {
    inner: Arc<Inner<S>>,
    idler: Option<JoinHandle<()>>,
}

/// The signer stays out of the debug form.
impl<S: SigningPort + Send + Sync + 'static> std::fmt::Debug for AuditLog<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditLog").field("dir", &self.inner.dir).field("tip", &self.tip()).finish_non_exhaustive()
    }
}

impl<S: SigningPort + Send + Sync + 'static> AuditLog<S> {
    /// [`AuditLog::open_with`] under the default options.
    pub fn open(dir: impl Into<PathBuf>, signer: S) -> Result<AuditLog<S>, AuditError> {
        AuditLog::open_with(dir, signer, AuditOptions::default())
    }

    /// Open the log under `dir`: take the directory's writer lock, then verify the chain
    /// and every root and tip signature under the signer's key. A full last segment left
    /// without its root closes now, and a tip absent or behind the chain end advances to
    /// it.
    pub fn open_with(dir: impl Into<PathBuf>, signer: S, options: AuditOptions) -> Result<AuditLog<S>, AuditError> {
        let dir = dir.into();
        let seg_dir = segments_dir(&dir);
        fs::create_dir_all(&seg_dir).map_err(unreadable(&seg_dir))?;
        let lock_path = dir.join("audit.lock");
        let lock = OpenOptions::new().create(true).truncate(false).write(true).open(&lock_path).map_err(unreadable(&lock_path))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                return Err(AuditError::AuditLogHeld(format!("{} is held by another writer", lock_path.display())))
            }
            Err(fs::TryLockError::Error(e)) => return Err(unreadable(&lock_path)(e)),
        }
        drop_torn_tail(&dir, &*options.fsync)?;
        let end = verify_signed(&dir, &SignerKey::of(&signer))?;
        let recorded = read_tip(&dir)?.map(|t| ChainTip { seq: t.seq, entry_hash: t.entry_hash });
        let state = State {
            tip: end.clone(),
            signed: recorded.clone().unwrap_or_else(ChainTip::genesis),
            queue: Vec::new(),
            next_ticket: 0,
            done: HashMap::new(),
            busy: false,
            poisoned: None,
            last_append: Instant::now(),
            closing: false,
        };
        let inner = Arc::new(Inner {
            dir,
            signer,
            fsync: options.fsync,
            idle: options.idle,
            _lock: lock,
            state: Mutex::new(state),
            turn: Condvar::new(),
        });
        if end.seq > 0 && end.seq % AUDIT_SEGMENT_ENTRIES == 0 {
            let n = segment_of(end.seq);
            if !root_path(&inner.dir, n).exists() {
                inner.close(n, &end).map_err(AuditError::AuditEntryUnpersisted)?;
            }
        }
        if recorded.as_ref() != Some(&end) {
            inner.write_tip(&end).map_err(AuditError::AuditEntryUnpersisted)?;
            inner.lock().signed = end;
        }
        let idler = {
            let inner = Arc::clone(&inner);
            std::thread::Builder::new()
                .name("audit-tip".into())
                .spawn(move || inner.sign_when_idle())
                .map_err(|e| AuditError::Io(format!("starting the idle tip signer: {e}")))?
        };
        Ok(AuditLog { inner, idler: Some(idler) })
    }

    /// The last durable entry.
    pub fn tip(&self) -> ChainTip {
        self.inner.lock().tip.clone()
    }

    /// Appends waiting for the group in flight.
    pub fn queued(&self) -> usize {
        self.inner.lock().queue.len()
    }

    /// Append one entry. See [`AuditLog::append_all`].
    pub fn append(&self, attributes: Value) -> Result<AuditEntry, AuditError> {
        let mut entries = self.append_all(vec![attributes])?;
        Ok(entries.pop().expect("one entry appended"))
    }

    /// Append a batch as one member of an append group. The call returns once one data
    /// sync of the segment covers every member's lines, or raises `AuditEntryUnpersisted`
    /// with every other member when that sync or any write before it fails; the files then
    /// return to their prior length and the tip stays where it was.
    pub fn append_all(&self, batch: Vec<Value>) -> Result<Vec<AuditEntry>, AuditError> {
        let inner = &*self.inner;
        let mut st = inner.lock();
        let ticket = st.next_ticket;
        st.next_ticket += 1;
        st.queue.push(Member { ticket, batch });
        loop {
            if let Some(result) = st.done.remove(&ticket) {
                return result;
            }
            if !st.busy {
                break;
            }
            st = inner.turn.wait(st).unwrap_or_else(PoisonError::into_inner);
        }
        // This append leads the next group: every append queued so far, its own included.
        st.busy = true;
        let group = std::mem::take(&mut st.queue);
        let (tip, poisoned) = (st.tip.clone(), st.poisoned.clone());
        drop(st);
        let sizes: Vec<(u64, usize)> = group.iter().map(|m| (m.ticket, m.batch.len())).collect();
        let values: Vec<Value> = group.into_iter().flat_map(|m| m.batch).collect();
        let outcome = match poisoned {
            Some(reason) => Err((format!("an earlier append could not be reversed ({reason}); reopen the log"), None)),
            None => inner.commit(&tip, values),
        };
        let mut st = inner.lock();
        match outcome {
            Ok((entries, end, signed)) => {
                st.tip = end;
                if let Some(signed) = signed {
                    st.signed = signed;
                }
                let mut entries = entries.into_iter();
                for (t, len) in sizes {
                    st.done.insert(t, Ok(entries.by_ref().take(len).collect()));
                }
            }
            Err((reason, poison)) => {
                if let Some(poison) = poison {
                    st.poisoned = Some(poison);
                }
                for (t, _) in sizes {
                    st.done.insert(t, Err(AuditError::AuditEntryUnpersisted(reason.clone())));
                }
            }
        }
        st.last_append = Instant::now();
        st.busy = false;
        inner.turn.notify_all();
        st.done.remove(&ticket).expect("the leader records its own result")
    }

    /// Sign the tip at the chain end and write it as `chain.tip`, for a verifier reading
    /// the log now.
    pub fn export(&self) -> Result<SignedTip, AuditError> {
        let inner = &*self.inner;
        let mut st = inner.lock();
        while st.busy {
            st = inner.turn.wait(st).unwrap_or_else(PoisonError::into_inner);
        }
        st.busy = true;
        let tip = st.tip.clone();
        drop(st);
        let written = inner.write_tip(&tip);
        let mut st = inner.lock();
        if written.is_ok() {
            st.signed = tip;
        }
        st.busy = false;
        inner.turn.notify_all();
        written.map_err(AuditError::Io)
    }
}

/// Closing the log stops the idle signer and signs the tip at the chain end.
impl<S: SigningPort + Send + Sync + 'static> Drop for AuditLog<S> {
    fn drop(&mut self) {
        self.inner.lock().closing = true;
        self.inner.turn.notify_all();
        if let Some(idler) = self.idler.take() {
            let _ = idler.join();
        }
        let st = self.inner.lock();
        if st.signed != st.tip && st.poisoned.is_none() {
            let tip = st.tip.clone();
            drop(st);
            let _ = self.inner.write_tip(&tip);
        }
    }
}

impl<S: SigningPort> Inner<S> {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Write one group after `tip`, reversing it on failure. The error carries the reason
    /// and, when the reversal itself failed, the reason the log is poisoned.
    fn commit(&self, tip: &ChainTip, values: Vec<Value>) -> Result<Committed, (String, Option<String>)> {
        let mut undo = Undo::default();
        self.persist(tip, values, &mut undo).map_err(|e| (e, undo.apply(&*self.fsync).err()))
    }

    fn persist(&self, start: &ChainTip, values: Vec<Value>, undo: &mut Undo) -> Result<Committed, String> {
        let seg_dir = segments_dir(&self.dir);
        let mut tip = start.clone();
        let mut entries = Vec::with_capacity(values.len());
        let mut closed = false;
        let mut pending = values.into_iter().peekable();
        while pending.peek().is_some() {
            let n = segment_of(tip.seq + 1);
            let path = segment_path(&self.dir, n);
            let existed = path.exists();
            let mut file = OpenOptions::new().create(true).append(true).open(&path).map_err(fail(&path))?;
            if existed {
                if undo.truncate.is_none() {
                    undo.truncate = Some((n, path.clone(), file.metadata().map_err(fail(&path))?.len()));
                }
            } else {
                undo.created.push(path.clone());
            }
            let room = n * AUDIT_SEGMENT_ENTRIES - tip.seq;
            let mut buf = Vec::new();
            for attributes in pending.by_ref().take(room as usize) {
                let entry = AuditEntry::link(tip.seq + 1, &tip.entry_hash, attributes);
                serde_json::to_writer(&mut buf, &entry).map_err(fail(&path))?;
                buf.push(b'\n');
                tip = entry.tip();
                entries.push(entry);
            }
            file.write_all(&buf).and_then(|_| self.fsync.sync(Fsync::Segment(n), &file)).map_err(fail(&path))?;
            if !existed {
                File::open(&seg_dir).and_then(|d| self.fsync.sync(Fsync::SegmentOpen(n), &d)).map_err(fail(&seg_dir))?;
            }
            if tip.seq == n * AUDIT_SEGMENT_ENTRIES {
                undo.created.push(root_path(&self.dir, n));
                self.close(n, &tip)?;
                closed = true;
            }
        }
        if closed {
            self.write_tip(&tip)?;
        }
        let signed = closed.then(|| tip.clone());
        Ok((entries, tip, signed))
    }

    /// Sign `tip` and write it durably as `chain.tip`.
    fn write_tip(&self, tip: &ChainTip) -> Result<SignedTip, String> {
        let signed = SignedTip::sign(tip.seq, &tip.entry_hash, &self.signer)?;
        let tpath = tip_path(&self.dir);
        write_durable(&tpath, &serde_json::to_vec(&signed).map_err(fail(&tpath))?, &*self.fsync, Fsync::Tip)?;
        Ok(signed)
    }

    /// Close segment `n`, whose last entry is `last`, under a signed root.
    fn close(&self, n: u64, last: &ChainTip) -> Result<(), String> {
        let root = SignedRoot::sign(&last.entry_hash, AUDIT_SEGMENT_ENTRIES, &self.signer)?;
        let path = root_path(&self.dir, n);
        write_durable(&path, &serde_json::to_vec(&root).map_err(fail(&path))?, &*self.fsync, Fsync::Root(n))
    }

    /// The idle signer: once no append has arrived for `idle` and the tip lags the chain
    /// end, sign the tip at the end. A failed write retries after another idle interval.
    fn sign_when_idle(&self) {
        let mut st = self.lock();
        loop {
            if st.closing {
                return;
            }
            if st.busy || st.signed == st.tip || st.poisoned.is_some() {
                st = self.turn.wait(st).unwrap_or_else(PoisonError::into_inner);
                continue;
            }
            let quiet = st.last_append.elapsed();
            if quiet < self.idle {
                st = self.turn.wait_timeout(st, self.idle - quiet).unwrap_or_else(PoisonError::into_inner).0;
                continue;
            }
            st.busy = true;
            let tip = st.tip.clone();
            drop(st);
            let written = self.write_tip(&tip);
            st = self.lock();
            match written {
                Ok(_) => st.signed = tip,
                Err(_) => st.last_append = Instant::now(),
            }
            st.busy = false;
            self.turn.notify_all();
        }
    }
}
