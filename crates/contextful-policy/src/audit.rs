//! `disclosure.record` and `disclosure.attest`: the hash-linked audit chain, its numbered
//! segment files, the chain tip, the signed root closing each segment, and verification.
//!
//! Layout under the audit directory:
//!
//! ```text
//! segments/000001.jsonl       one entry per line, seq ascending from 1
//! segments/000001.root.json   {root, count, signature} closing the segment
//! chain.tip                   last accepted {seq, entry_hash, signature}
//! audit.lock                  held by the one process writing the log
//! ```
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
use std::collections::BTreeSet;
use std::fmt::Display;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The `prev_hash` of the first entry.
pub const GENESIS: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

/// Entries a segment holds before its signed root closes it.
pub const AUDIT_SEGMENT_ENTRIES: u64 = 4096;

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

/// Truncate the last segment to its last newline when it ends mid-line. An entry line is
/// written whole and newline-terminated, and the tip moves only after the segment syncs,
/// so unterminated bytes are a write a crash cut short, past the tip and never
/// acknowledged. A newline-terminated line that does not parse is left for [`verify`].
fn drop_torn_tail(dir: &Path) -> Result<(), AuditError> {
    let (segments, _) = listing(dir)?;
    let Some(&last) = segments.last() else { return Ok(()) };
    let path = segment_path(dir, last);
    let bytes = fs::read(&path).map_err(unreadable(&path))?;
    if bytes.last().is_none_or(|b| *b == b'\n') {
        return Ok(());
    }
    let keep = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1) as u64;
    let f = OpenOptions::new().write(true).open(&path).map_err(unreadable(&path))?;
    f.set_len(keep).and_then(|_| f.sync_all()).map_err(unreadable(&path))
}

/// Write `bytes` to `path` durably: a sibling temporary file, synced, renamed into
/// place, and the directory synced.
fn write_durable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    let attempt = (|| -> std::io::Result<()> {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)?;
        sync_dir(path.parent().unwrap_or(Path::new(".")))
    })();
    if attempt.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    attempt.map_err(fail(path))
}

fn sync_dir(dir: &Path) -> std::io::Result<()> {
    File::open(dir)?.sync_all()
}

fn fail<E: Display>(path: &Path) -> impl Fn(E) -> String + '_ {
    move |e| format!("{}: {e}", path.display())
}

/// What an append touched on disk, reversed when it fails to persist.
#[derive(Default)]
struct Undo {
    truncate: Option<(PathBuf, u64)>,
    created: Vec<PathBuf>,
}

impl Undo {
    /// Reverse the append, reporting the first step that could not be reversed.
    fn apply(self) -> Result<(), String> {
        let mut first = None;
        if let Some((path, len)) = self.truncate {
            let r = OpenOptions::new().write(true).open(&path).and_then(|f| f.set_len(len).and_then(|_| f.sync_all()));
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

/// The node's audit log: appends link to the tip, reach local durable storage before
/// they return, and close each full segment under a signed root.
pub struct AuditLog<S: SigningPort> {
    dir: PathBuf,
    signer: S,
    tip: ChainTip,
    /// The exclusive lock on `audit.lock`, held for the log's lifetime.
    _lock: File,
    /// Set when a failed append could not be reversed: the files may hold lines past the
    /// tip, so every later append refuses until the log is reopened and verified.
    poisoned: Option<String>,
}

/// The signer stays out of the debug form.
impl<S: SigningPort> std::fmt::Debug for AuditLog<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditLog").field("dir", &self.dir).field("tip", &self.tip).finish_non_exhaustive()
    }
}

impl<S: SigningPort> AuditLog<S> {
    /// Open the log under `dir`: take the directory's writer lock, then verify the chain
    /// and every root and tip signature under the signer's key. A full last segment left
    /// without its root closes now, and a tip behind the chain end advances to it.
    pub fn open(dir: impl Into<PathBuf>, signer: S) -> Result<AuditLog<S>, AuditError> {
        let dir = dir.into();
        fs::create_dir_all(&dir).map_err(unreadable(&dir))?;
        let lock_path = dir.join("audit.lock");
        let lock = OpenOptions::new().create(true).truncate(false).write(true).open(&lock_path).map_err(unreadable(&lock_path))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                return Err(AuditError::AuditLogHeld(format!("{} is held by another writer", lock_path.display())))
            }
            Err(fs::TryLockError::Error(e)) => return Err(unreadable(&lock_path)(e)),
        }
        drop_torn_tail(&dir)?;
        let end = verify_signed(&dir, &SignerKey::of(&signer))?;
        let log = AuditLog { dir, signer, tip: end, _lock: lock, poisoned: None };
        if log.tip.seq > 0 && log.tip.seq % AUDIT_SEGMENT_ENTRIES == 0 {
            let n = segment_of(log.tip.seq);
            if !root_path(&log.dir, n).exists() {
                log.close(n).map_err(AuditError::AuditEntryUnpersisted)?;
            }
        }
        let recorded = read_tip(&log.dir)?.map(|t| ChainTip { seq: t.seq, entry_hash: t.entry_hash });
        if recorded.as_ref() != Some(&log.tip) && log.tip.seq > 0 {
            log.write_tip(&log.tip).map_err(AuditError::AuditEntryUnpersisted)?;
        }
        Ok(log)
    }

    /// The last accepted entry.
    pub fn tip(&self) -> &ChainTip {
        &self.tip
    }

    /// Append one entry. See [`AuditLog::append_all`].
    pub fn append(&mut self, attributes: Value) -> Result<AuditEntry, AuditError> {
        let mut entries = self.append_all(vec![attributes])?;
        Ok(entries.pop().expect("one entry appended"))
    }

    /// Append a batch under one commit: one sync per segment file the batch reaches,
    /// then the tip. On any failure nothing in the batch persists, the files return to
    /// their prior length, and the in-memory tip stays where it was.
    pub fn append_all(&mut self, batch: Vec<Value>) -> Result<Vec<AuditEntry>, AuditError> {
        if let Some(reason) = &self.poisoned {
            return Err(AuditError::AuditEntryUnpersisted(format!("an earlier append could not be reversed ({reason}); reopen the log")));
        }
        let mut undo = Undo::default();
        match self.persist(batch, &mut undo) {
            Ok((entries, tip)) => {
                self.tip = tip;
                Ok(entries)
            }
            Err(e) => {
                if let Err(reason) = undo.apply() {
                    self.poisoned = Some(reason);
                }
                Err(AuditError::AuditEntryUnpersisted(e))
            }
        }
    }

    fn persist(&self, batch: Vec<Value>, undo: &mut Undo) -> Result<(Vec<AuditEntry>, ChainTip), String> {
        let seg_dir = segments_dir(&self.dir);
        fs::create_dir_all(&seg_dir).map_err(fail(&seg_dir))?;
        let mut tip = self.tip.clone();
        let mut entries = Vec::with_capacity(batch.len());
        let mut pending = batch.into_iter().peekable();
        while pending.peek().is_some() {
            let n = segment_of(tip.seq + 1);
            let path = segment_path(&self.dir, n);
            let existed = path.exists();
            let mut file = OpenOptions::new().create(true).append(true).open(&path).map_err(fail(&path))?;
            if existed {
                if undo.truncate.is_none() {
                    undo.truncate = Some((path.clone(), file.metadata().map_err(fail(&path))?.len()));
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
            file.write_all(&buf).and_then(|_| file.sync_data()).map_err(fail(&path))?;
            if !existed {
                sync_dir(&seg_dir).map_err(fail(&seg_dir))?;
            }
            if tip.seq == n * AUDIT_SEGMENT_ENTRIES {
                undo.created.push(root_path(&self.dir, n));
                self.close_at(n, &tip)?;
            }
        }
        self.write_tip(&tip)?;
        Ok((entries, tip))
    }

    /// Sign `tip` and write it durably as `chain.tip`.
    fn write_tip(&self, tip: &ChainTip) -> Result<(), String> {
        let signed = SignedTip::sign(tip.seq, &tip.entry_hash, &self.signer)?;
        let tpath = tip_path(&self.dir);
        write_durable(&tpath, &serde_json::to_vec(&signed).map_err(fail(&tpath))?)
    }

    /// Close segment `n`, whose last entry is the current tip.
    fn close(&self, n: u64) -> Result<(), String> {
        self.close_at(n, &self.tip)
    }

    fn close_at(&self, n: u64, last: &ChainTip) -> Result<(), String> {
        let root = SignedRoot::sign(&last.entry_hash, AUDIT_SEGMENT_ENTRIES, &self.signer)?;
        let path = root_path(&self.dir, n);
        write_durable(&path, &serde_json::to_vec(&root).map_err(fail(&path))?)
    }
}
