//! `disclosure.record` and `disclosure.attest`: the hash-linked audit chain, its header, its
//! numbered segment files, the chain tip, the signed root closing each segment, inclusion
//! proofs, and verification.
//!
//! Layout under the audit directory:
//!
//! ```text
//! header.json                 {format, digest, segment_entries}, v1 chains alone
//! segments/000001.jsonl       one entry per line, seq ascending from 1
//! segments/000001.root.json   the signed root closing the segment
//! chain.tip                   a signed {seq, entry_hash} at or behind the chain end
//! chain.held                  the signed {seq, entry_hash} at which the issuer first held the log
//! audit.lock                  held by the one process writing the log
//! ```
//!
//! Two formats verify (`disclosure.record.v0-chain`). A v1 chain carries `header.json`,
//! which fixes the digest (SHA-256 or BLAKE3) and the segment size; each entry carries
//! `format: 1` and digests the RFC 8785 canonical JSON of its `format`, `seq`, `prev_hash`
//! and `attributes`, each attribute integer within ±(2^53 − 1)
//! (`disclosure.record.inexact-integer`); the first entry links to the header's digest; and a segment root is
//! the RFC 6962 Merkle tree hash of the segment's entry digests, so one entry proves its
//! membership with an audit path ([`prove`]). A v0 chain has no header: its entry digest is
//! SHA-256 over seq (8 bytes, little-endian), `prev_hash` and the sorted-key compact JSON of
//! the attributes, and its root is the segment's last entry digest.
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
//! (`authority.issue.signing-port`), under either scheme and stored as 64 raw Ed25519
//! bytes or ES256 DER, so a chain truncated under a rewritten tip fails signed
//! verification.
//!
//! A log opens under the custody its caller holds: held, signing through the port;
//! unanchored, linking entries under an unsigned tip and writing no root; or read-only,
//! verifying without the writer lock. Anchoring signs an unanchored chain's missing roots
//! and its tip, and is the one path from unanchored to held. A chain carrying `chain.held`
//! or a signed root is held for good (`disclosure.attest.broken-chain`): an absent or unsigned tip
//! over it breaks the chain under every check, so a truncation under a stripped tip never
//! reads as an unanchored interval. Deleting every root and `chain.held` as well leaves a
//! chain no local check tells from an unanchored one; that, and a replayed older signed
//! tip, are caught only against a replicated root (`disclosure.attest.root-replication`).

use crate::issue::{sign_through, SignerKey};
use contextful_core::issue::{SignatureAlgorithm, SignatureEncoding};
use contextful_core::ports::SigningPort;
use contextful_core::AuthorityError;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::fmt::Display;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The `prev_hash` of a v0 chain's first entry.
pub const GENESIS: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

/// The format a new chain writes.
pub const AUDIT_FORMAT: u32 = 1;

/// Entries a segment holds before its signed root closes it, unless the chain header names
/// another size; a v0 segment always holds this many.
pub const AUDIT_SEGMENT_ENTRIES: u64 = 4096;

/// The largest segment size a chain header names (`disclosure.record.chain-header`).
pub const AUDIT_SEGMENT_MAX: u64 = 65_536;

/// The largest integer magnitude a v1 attribute holds: 2^53 − 1, beyond which RFC 8785's
/// IEEE 754 number form maps two integers to one (`disclosure.record.inexact-integer`).
pub const AUDIT_EXACT_INTEGER: u64 = (1 << 53) - 1;

/// Quiet time after the last append at which the log signs its tip
/// (`disclosure.record.tip-signing`).
pub const AUDIT_TIP_IDLE: Duration = Duration::from_secs(1);

/// A refusal of the audit chain.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuditError {
    /// A disagreeing digest, entry format or Merkle root, a sequence gap, an absent chain
    /// beside a tip or root, entries without a tip, or an unsigned tip over a held chain;
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
    /// A chain header naming a format, digest or segment size this build does not verify.
    /// (`disclosure.record.header-unsupported`)
    #[error("AuditHeaderUnsupported: {0}")]
    AuditHeaderUnsupported(String),
    /// A v1 entry's attribute holding an integer beyond ±[`AUDIT_EXACT_INTEGER`], which the
    /// canonical form cannot write exactly. (`disclosure.record.inexact-integer`)
    #[error("AuditAttributeInexact: {0}")]
    AuditAttributeInexact(String),
    /// An inclusion proof that does not verify. (`disclosure.attest.proof-invalid`)
    #[error("AuditProofInvalid: {0}")]
    AuditProofInvalid(String),
    /// An entry no signed Merkle root covers. (`disclosure.attest.proof-unavailable`)
    #[error("AuditProofUnavailable: {0}")]
    AuditProofUnavailable(String),
    /// An append through a read-only handle. (`disclosure.record.read-only`)
    #[error("AuditLogReadOnly: {0}")]
    AuditLogReadOnly(String),
    /// An unanchored handle opened over a chain holding a signed tip, a signed root or `chain.held`.
    /// (`disclosure.record.unanchored-over-signed`)
    #[error("AuditLogAnchored: {0}")]
    AuditLogAnchored(String),
    /// A held open or signed check over a chain whose tip is unsigned, carrying no
    /// `chain.held` or signed root.
    /// (`disclosure.record.unsigned-tip`)
    #[error("AuditLogUnanchored: {0}")]
    AuditLogUnanchored(String),
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

fn unsupported(reason: impl Into<String>) -> AuditError {
    AuditError::AuditHeaderUnsupported(reason.into())
}

fn invalid(reason: impl Into<String>) -> AuditError {
    AuditError::AuditProofInvalid(reason.into())
}

/// The digest a v1 chain header names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DigestAlgorithm {
    Sha256,
    Blake3,
}

impl DigestAlgorithm {
    /// The prefix naming the digest in an `<algorithm>:<hex>` value.
    pub fn prefix(self) -> &'static str {
        match self {
            DigestAlgorithm::Sha256 => "sha256",
            DigestAlgorithm::Blake3 => "blake3",
        }
    }

    fn hash(self, parts: &[&[u8]]) -> [u8; 32] {
        match self {
            DigestAlgorithm::Sha256 => {
                let mut h = Sha256::new();
                for part in parts {
                    h.update(part);
                }
                h.finalize().into()
            }
            DigestAlgorithm::Blake3 => {
                let mut h = blake3::Hasher::new();
                for part in parts {
                    h.update(part);
                }
                *h.finalize().as_bytes()
            }
        }
    }

    fn tagged(self, digest: [u8; 32]) -> String {
        format!("{}:{}", self.prefix(), hex::encode(digest))
    }

    /// The raw digest of a tagged value under this algorithm.
    fn raw(self, tagged: &str) -> Option<[u8; 32]> {
        let hex = tagged.strip_prefix(self.prefix())?.strip_prefix(':')?;
        hex::decode(hex).ok()?.try_into().ok()
    }
}

/// A v1 chain's header, `header.json`: the format, the digest and the segment size, fixed
/// for the chain's life (`disclosure.record.chain-header`). Any other field refuses to read
/// (`disclosure.record.header-unsupported`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainHeader {
    pub format: u32,
    pub digest: DigestAlgorithm,
    pub segment_entries: u64,
}

/// SHA-256 under 4096-entry segments.
impl Default for ChainHeader {
    fn default() -> ChainHeader {
        ChainHeader { format: AUDIT_FORMAT, digest: DigestAlgorithm::Sha256, segment_entries: AUDIT_SEGMENT_ENTRIES }
    }
}

impl ChainHeader {
    /// The header's digest under its own algorithm, over its RFC 8785 canonical JSON: the
    /// `prev_hash` of the chain's first entry, and part of every signed root.
    pub fn digest(&self) -> String {
        self.digest.tagged(self.digest.hash(&[&canonical(&json!(self))]))
    }

    /// Refuse a header this build does not write or verify.
    fn check(&self) -> Result<(), AuditError> {
        if self.format != AUDIT_FORMAT {
            return Err(unsupported(format!("the chain header names format {}; this build verifies v0 and v{AUDIT_FORMAT}", self.format)));
        }
        if !(1..=AUDIT_SEGMENT_MAX).contains(&self.segment_entries) {
            return Err(unsupported(format!(
                "the chain header names {} entries per segment, outside 1 to {AUDIT_SEGMENT_MAX}",
                self.segment_entries
            )));
        }
        Ok(())
    }

    /// Parse and check `header.json` text.
    fn parse(text: &str) -> Result<ChainHeader, AuditError> {
        let header: ChainHeader =
            serde_json::from_str(text).map_err(|e| unsupported(format!("the chain header does not read as a v{AUDIT_FORMAT} header: {e}")))?;
        header.check()?;
        Ok(header)
    }
}

/// The first integer in `value` beyond ±[`AUDIT_EXACT_INTEGER`], depth first.
fn inexact_integer(value: &Value) -> Option<&serde_json::Number> {
    match value {
        Value::Number(n) => {
            let beyond = n.as_u64().map(|u| u > AUDIT_EXACT_INTEGER).or_else(|| n.as_i64().map(|i| i.unsigned_abs() > AUDIT_EXACT_INTEGER));
            beyond.unwrap_or(false).then_some(n)
        }
        Value::Array(items) => items.iter().find_map(inexact_integer),
        Value::Object(map) => map.values().find_map(inexact_integer),
        _ => None,
    }
}

/// RFC 8785 canonical JSON of `value`.
fn canonical(value: &Value) -> Vec<u8> {
    serde_json_canonicalizer::to_vec(value).expect("a JSON value canonicalizes")
}

/// Which rules a chain verifies and appends under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainFormat {
    /// No header: SHA-256 over seq, `prev_hash` and sorted-key attributes; the root is the
    /// last entry digest; 4096-entry segments.
    V0,
    /// Under a chain header: the RFC 8785 whole-entry digest and a Merkle root.
    V1(ChainHeader),
}

impl ChainFormat {
    /// Entries a segment holds before its root closes it.
    pub fn segment_entries(&self) -> u64 {
        match self {
            ChainFormat::V0 => AUDIT_SEGMENT_ENTRIES,
            ChainFormat::V1(h) => h.segment_entries,
        }
    }

    /// The chain end of an empty chain: the `prev_hash` its first entry links to.
    pub fn genesis(&self) -> ChainTip {
        match self {
            ChainFormat::V0 => ChainTip::genesis(),
            ChainFormat::V1(h) => ChainTip { seq: 0, entry_hash: h.digest() },
        }
    }

    fn version(&self) -> u32 {
        match self {
            ChainFormat::V0 => 0,
            ChainFormat::V1(h) => h.format,
        }
    }

    /// The entry at `seq`, linked to `prev_hash`, under this format.
    pub fn link(&self, seq: u64, prev_hash: &str, attributes: Value) -> AuditEntry {
        let mut entry = AuditEntry { format: self.version(), seq, prev_hash: prev_hash.to_string(), attributes, entry_hash: String::new() };
        entry.entry_hash = self.entry_digest(&entry);
        entry
    }

    /// The digest of `entry`'s fields other than `entry_hash`, under this format's rules.
    pub fn entry_digest(&self, entry: &AuditEntry) -> String {
        match self {
            ChainFormat::V0 => {
                let mut h = Sha256::new();
                h.update(entry.seq.to_le_bytes());
                h.update(entry.prev_hash.as_bytes());
                let mut sorted = Vec::new();
                write_sorted(&entry.attributes, &mut sorted);
                h.update(sorted);
                format!("sha256:{}", hex::encode(h.finalize()))
            }
            ChainFormat::V1(header) => {
                let body = json!({
                    "format": entry.format,
                    "seq": entry.seq,
                    "prev_hash": entry.prev_hash,
                    "attributes": entry.attributes,
                });
                header.digest.tagged(header.digest.hash(&[&canonical(&body)]))
            }
        }
    }

    /// Whether `entry` carries this format and its `entry_hash` is the digest of its other
    /// fields; a v1 entry also holds every attribute exactly.
    pub fn digest_agrees(&self, entry: &AuditEntry) -> bool {
        entry.format == self.version() && self.inexact(&entry.attributes).is_none() && self.entry_digest(entry) == entry.entry_hash
    }

    /// Under v1, the first attribute integer the canonical form writes inexactly
    /// (`disclosure.record.inexact-integer`); v0 digests every integer's exact decimal form.
    fn inexact<'a>(&self, attributes: &'a Value) -> Option<&'a serde_json::Number> {
        match self {
            ChainFormat::V0 => None,
            ChainFormat::V1(_) => inexact_integer(attributes),
        }
    }

    /// The root a full segment of `entries` closes under.
    fn root_of(&self, entries: &[AuditEntry]) -> Result<String, String> {
        match self {
            ChainFormat::V0 => Ok(entries.last().map_or_else(|| GENESIS.to_string(), |e| e.entry_hash.clone())),
            ChainFormat::V1(h) => Ok(h.digest.tagged(tree_hash(h.digest, &leaf_hashes(h.digest, entries)?))),
        }
    }
}

/// Compact JSON with object keys sorted in byte order at every depth: the v0 attribute form.
fn write_sorted(value: &Value, out: &mut Vec<u8>) {
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
                write_sorted(&map[key], out);
            }
            out.push(b'}');
        }
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_sorted(item, out);
            }
            out.push(b']');
        }
        leaf => serde_json::to_writer(&mut *out, leaf).expect("a JSON value serializes"),
    }
}

/// RFC 6962 leaf hashes of `entries`: each leaf input is an entry's raw digest.
fn leaf_hashes(digest: DigestAlgorithm, entries: &[AuditEntry]) -> Result<Vec<[u8; 32]>, String> {
    entries
        .iter()
        .map(|e| {
            let raw = digest.raw(&e.entry_hash).ok_or_else(|| format!("entry {} carries no {} digest", e.seq, digest.prefix()))?;
            Ok(digest.hash(&[&[0], &raw]))
        })
        .collect()
}

fn node_hash(digest: DigestAlgorithm, left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    digest.hash(&[&[1], left, right])
}

/// The largest power of two below `n`, for `n` of 2 or more.
fn split(n: usize) -> usize {
    1 << (usize::BITS - 1 - (n - 1).leading_zeros())
}

/// RFC 6962 Merkle tree hash over `leaves`, already leaf-hashed.
fn tree_hash(digest: DigestAlgorithm, leaves: &[[u8; 32]]) -> [u8; 32] {
    match leaves.len() {
        0 => digest.hash(&[]),
        1 => leaves[0],
        n => {
            let k = split(n);
            node_hash(digest, &tree_hash(digest, &leaves[..k]), &tree_hash(digest, &leaves[k..]))
        }
    }
}

/// RFC 6962 audit path of leaf `m` among `leaves`, from the leaf upward.
fn audit_path(digest: DigestAlgorithm, m: usize, leaves: &[[u8; 32]]) -> Vec<[u8; 32]> {
    let n = leaves.len();
    if n <= 1 {
        return Vec::new();
    }
    let k = split(n);
    let (mut path, sibling) = if m < k {
        (audit_path(digest, m, &leaves[..k]), tree_hash(digest, &leaves[k..]))
    } else {
        (audit_path(digest, m - k, &leaves[k..]), tree_hash(digest, &leaves[..k]))
    };
    path.push(sibling);
    path
}

/// The root an audit path yields for leaf `index` of a tree of `size` leaves, by the RFC 9162
/// verification algorithm; `None` when the path's length does not fit the tree.
fn root_from_path(digest: DigestAlgorithm, index: u64, size: u64, leaf: [u8; 32], path: &[[u8; 32]]) -> Option<[u8; 32]> {
    if index >= size {
        return None;
    }
    let (mut f, mut s, mut r) = (index, size - 1, leaf);
    for p in path {
        if s == 0 {
            return None;
        }
        if f & 1 == 1 || f == s {
            r = node_hash(digest, p, &r);
            while f & 1 == 0 && f != 0 {
                f >>= 1;
                s >>= 1;
            }
        } else {
            r = node_hash(digest, &r, p);
        }
        f >>= 1;
        s >>= 1;
    }
    (s == 0).then_some(r)
}

/// One entry: its format, the audited attributes and their linkage to the entry before. Any
/// other field sits outside the entry digest and refuses to read (`disclosure.record.entry-fields`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditEntry {
    /// `1` on a v1 entry; absent, read as `0`, on a v0 entry.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub format: u32,
    pub seq: u64,
    pub prev_hash: String,
    pub attributes: Value,
    pub entry_hash: String,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl AuditEntry {
    fn tip(&self) -> ChainTip {
        ChainTip { seq: self.seq, entry_hash: self.entry_hash.clone() }
    }
}

/// The last accepted entry: `{seq: 0, entry_hash: <genesis>}` for an empty chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainTip {
    pub seq: u64,
    pub entry_hash: String,
}

impl ChainTip {
    /// The v0 genesis; a v1 chain's is [`ChainFormat::genesis`].
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

/// The root closing one segment, signed. A v0 root is the last entry digest; a v1 root is
/// the segment's Merkle tree hash and names its format, signature algorithm, chain header
/// digest and segment number, all under the signature (`disclosure.attest.root-tag`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedRoot {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub format: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alg: Option<SignatureAlgorithm>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment: Option<u64>,
    pub root: String,
    pub count: u64,
    /// Hex-encoded signing-port signature over the root's message, in the port's encoding
    /// for its scheme.
    pub signature: String,
}

impl SignedRoot {
    /// The v0 signed bytes: `contextful.audit.root`, the count and the root, one per line,
    /// so a root cannot be replayed under another count.
    pub fn message(root: &str, count: u64) -> Vec<u8> {
        format!("contextful.audit.root\n{count}\n{root}").into_bytes()
    }

    /// The v1 signed bytes: `contextful.audit.root.v1`, the algorithm, the header digest, the
    /// segment number, the count and the root, one per line.
    pub fn message_v1(alg: SignatureAlgorithm, header: &str, segment: u64, count: u64, root: &str) -> Vec<u8> {
        format!("contextful.audit.root.v1\n{alg}\n{header}\n{segment}\n{count}\n{root}").into_bytes()
    }

    /// Sign a v0 `root` over `count` entries through `signer`.
    pub fn sign(root: &str, count: u64, signer: &dyn SigningPort) -> Result<SignedRoot, String> {
        let signature = sign_through(signer, &Self::message(root, count)).map_err(|e| e.to_string())?;
        Ok(SignedRoot { root: root.to_string(), count, signature: hex::encode(signature), ..SignedRoot::default() })
    }

    /// Sign the v1 root of segment `segment` of the chain whose header digest is `header`.
    pub fn sign_v1(header: &str, segment: u64, root: &str, count: u64, signer: &dyn SigningPort) -> Result<SignedRoot, String> {
        let alg = signer.algorithm();
        let signature = sign_through(signer, &Self::message_v1(alg, header, segment, count, root)).map_err(|e| e.to_string())?;
        Ok(SignedRoot {
            format: AUDIT_FORMAT,
            alg: Some(alg),
            header: Some(header.to_string()),
            segment: Some(segment),
            root: root.to_string(),
            count,
            signature: hex::encode(signature),
        })
    }

    /// Whether the signature verifies under `key`; a v1 root's algorithm tag must be the
    /// key's.
    pub fn verify(&self, key: &SignerKey) -> bool {
        let message = match (self.format, self.alg, &self.header, self.segment) {
            (0, None, None, None) => Self::message(&self.root, self.count),
            (AUDIT_FORMAT, Some(alg), Some(header), Some(segment)) if alg == key.algorithm => {
                Self::message_v1(alg, header, segment, self.count, &self.root)
            }
            _ => return false,
        };
        hex::decode(&self.signature).is_ok_and(|s| key.verifies(&message, &s))
    }
}

/// The last accepted entry as `chain.tip` records it, signed so a truncation under a
/// rewritten tip fails signed verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedTip {
    pub seq: u64,
    pub entry_hash: String,
    /// Hex-encoded signing-port signature over [`SignedTip::message`]; absent on a tip an
    /// unanchored handle writes, which signed verification refuses.
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
        let signature = sign_through(signer, &Self::message(seq, entry_hash)).map_err(|e| e.to_string())?;
        Ok(SignedTip { seq, entry_hash: entry_hash.to_string(), signature: Some(hex::encode(signature)) })
    }

    /// Whether the signature verifies under `key`.
    pub fn verify(&self, key: &SignerKey) -> bool {
        let Some(Ok(bytes)) = self.signature.as_ref().map(hex::decode) else { return false };
        key.verifies(&Self::message(self.seq, &self.entry_hash), &bytes)
    }
}

/// One entry's evidence of membership, checkable offline under the signer's public key
/// alone (`disclosure.attest.inclusion-proof`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InclusionProof {
    pub header: ChainHeader,
    pub entry: AuditEntry,
    /// The RFC 6962 audit path, hex digests from the leaf upward.
    pub path: Vec<String>,
    /// The signed root of the entry's segment.
    pub root: SignedRoot,
}

impl InclusionProof {
    /// Check the entry's format and digest under the header, recompute the segment root
    /// from the audit path, and verify the root's binding to the header, the segment and
    /// `key`; any disagreement raises `AuditProofInvalid`.
    pub fn verify(&self, key: &SignerKey) -> Result<(), AuditError> {
        let header = &self.header;
        header.check().map_err(|e| invalid(e.to_string()))?;
        let chain = ChainFormat::V1(*header);
        let seq = self.entry.seq;
        if seq == 0 || !chain.digest_agrees(&self.entry) {
            return Err(invalid(format!("entry {seq} disagrees with its digest under the header")));
        }
        let segment = segment_of(seq, header.segment_entries);
        let root = &self.root;
        if root.format != AUDIT_FORMAT
            || root.header.as_deref() != Some(header.digest().as_str())
            || root.segment != Some(segment)
            || root.count != header.segment_entries
        {
            return Err(invalid(format!("the root does not close segment {segment} of this header's chain")));
        }
        let digest = header.digest;
        let path = self
            .path
            .iter()
            .map(|h| hex::decode(h).ok().and_then(|b| <[u8; 32]>::try_from(b).ok()))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| invalid("an audit path hash is not a 32-byte hex digest"))?;
        let leaf = leaf_hashes(digest, std::slice::from_ref(&self.entry)).map_err(invalid)?[0];
        let index = seq - first_seq(segment, header.segment_entries);
        let computed = root_from_path(digest, index, root.count, leaf, &path)
            .ok_or_else(|| invalid(format!("an audit path of {} hashes does not fit a {}-entry segment", path.len(), root.count)))?;
        if digest.tagged(computed) != root.root {
            return Err(invalid(format!("the audit path of entry {seq} does not reach the signed root")));
        }
        if !root.verify(key) {
            return Err(invalid(format!("the root of segment {segment} does not verify under the {} key", key.algorithm)));
        }
        Ok(())
    }
}

/// The inclusion proof of entry `seq`: its entry, the chain header, its audit path and its
/// segment's signed root. An entry of a v0 chain, of a segment carrying no signed root, or
/// outside the chain raises `AuditProofUnavailable`; a segment disagreeing with its root
/// raises `AuditChainBroken`.
pub fn prove(dir: &Path, seq: u64) -> Result<InclusionProof, AuditError> {
    let unavailable = |reason: String| AuditError::AuditProofUnavailable(reason);
    let ChainFormat::V1(header) = read_format(dir)? else {
        return Err(unavailable("a v0 chain's roots are last-entry digests, not Merkle trees".into()));
    };
    if seq == 0 {
        return Err(unavailable("seq starts at 1".into()));
    }
    let n = segment_of(seq, header.segment_entries);
    let rpath = root_path(dir, n);
    let text = match fs::read_to_string(&rpath) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(unavailable(format!("segment {n}, holding entry {seq}, carries no signed root")))
        }
        Err(e) => return Err(unreadable(&rpath)(e)),
    };
    let closing = n * header.segment_entries;
    let root: SignedRoot = serde_json::from_str(&text).map_err(|e| broken(closing, format!("the root of segment {n} does not parse: {e}")))?;
    let entries = read_segment(dir, n, closing)?;
    let chain = ChainFormat::V1(header);
    let leaves = leaf_hashes(header.digest, &entries).map_err(|e| broken(closing, e))?;
    if entries.len() as u64 != header.segment_entries || header.digest.tagged(tree_hash(header.digest, &leaves)) != root.root {
        return Err(broken(closing, format!("segment {n} disagrees with its signed root")));
    }
    let index = (seq - first_seq(n, header.segment_entries)) as usize;
    let entry = entries[index].clone();
    if entry.seq != seq || !chain.digest_agrees(&entry) {
        return Err(broken(seq, "the entry disagrees with its digest or position"));
    }
    let path = audit_path(header.digest, index, &leaves).into_iter().map(hex::encode).collect();
    Ok(InclusionProof { header, entry, path, root })
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

fn header_path(dir: &Path) -> PathBuf {
    dir.join("header.json")
}

fn held_path(dir: &Path) -> PathBuf {
    dir.join("chain.held")
}

/// The signed bytes of `chain.held`: `contextful.audit.held`, the seq and the entry digest,
/// one per line, apart from a tip's and a root's so none replays as another.
fn held_message(seq: u64, entry_hash: &str) -> Vec<u8> {
    format!("contextful.audit.held\n{seq}\n{entry_hash}").into_bytes()
}

fn held_verifies(record: &SignedTip, key: &SignerKey) -> bool {
    let Some(Ok(bytes)) = record.signature.as_ref().map(hex::decode) else { return false };
    key.verifies(&held_message(record.seq, &record.entry_hash), &bytes)
}

/// The segment holding `seq` (1-based) under `size`-entry segments.
fn segment_of(seq: u64, size: u64) -> u64 {
    (seq - 1) / size + 1
}

fn first_seq(n: u64, size: u64) -> u64 {
    (n - 1) * size + 1
}

/// The chain header, when `header.json` exists.
fn read_header(dir: &Path) -> Result<Option<ChainHeader>, AuditError> {
    let path = header_path(dir);
    match fs::read_to_string(&path) {
        Ok(text) => ChainHeader::parse(&text).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(unreadable(&path)(e)),
    }
}

/// v1 under a header, v0 without one.
fn read_format(dir: &Path) -> Result<ChainFormat, AuditError> {
    Ok(read_header(dir)?.map_or(ChainFormat::V0, ChainFormat::V1))
}

/// The entries of segment `n`; a line that does not parse breaks the chain at `closing`.
fn read_segment(dir: &Path, n: u64, closing: u64) -> Result<Vec<AuditEntry>, AuditError> {
    let path = segment_path(dir, n);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(broken(closing, format!("segment {n} is absent"))),
        Err(e) => return Err(unreadable(&path)(e)),
    };
    text.lines()
        .map(|line| serde_json::from_str(line).map_err(|e| broken(closing, format!("segment {n}: a line does not parse: {e}"))))
        .collect()
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
    read_position(&tip_path(dir))
}

fn read_held(dir: &Path) -> Result<Option<SignedTip>, AuditError> {
    read_position(&held_path(dir))
}

/// A `{seq, entry_hash, signature?}` file, or `None` when absent.
fn read_position(path: &Path) -> Result<Option<SignedTip>, AuditError> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| AuditError::Io(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(unreadable(path)(e)),
    }
}

/// Verify the header, every entry's format, digest and linkage, sequence continuity across
/// segments, each root's digest, count and binding, and the tip against the chain. Returns
/// the chain end.
pub fn verify(dir: &Path) -> Result<ChainTip, AuditError> {
    walk(dir, Check::Linkage).map(|w| w.end)
}

/// [`verify`], and every root's signature and the tip's under `key`: the check opening a
/// log runs, and the offline verifier's. A non-empty chain needs a tip that verifies; an
/// unsigned one raises `AuditLogUnanchored` over an unheld chain and `AuditChainBroken` over
/// a held one.
pub fn verify_signed(dir: &Path, key: &SignerKey) -> Result<ChainTip, AuditError> {
    walk(dir, Check::Signed(key)).map(|w| w.end)
}

/// What a walk checks beyond digests, linkage, sequence and root counts.
#[derive(Clone, Copy)]
enum Check<'a> {
    Linkage,
    /// Every root and the tip verify under the key.
    Signed(&'a SignerKey),
    /// Every present root verifies under the key, and so does the tip when signed.
    Anchoring(&'a SignerKey),
}

impl<'a> Check<'a> {
    fn key(self) -> Option<&'a SignerKey> {
        match self {
            Check::Linkage => None,
            Check::Signed(key) | Check::Anchoring(key) => Some(key),
        }
    }
}

/// A walked chain: its end, and each full segment carrying no root.
struct Walked {
    end: ChainTip,
    unrooted: Vec<u64>,
}

fn walk(dir: &Path, check: Check) -> Result<Walked, AuditError> {
    let key = check.key();
    let chain = read_format(dir)?;
    let size = chain.segment_entries();
    let (segments, roots) = listing(dir)?;
    let tip_file = read_tip(dir)?;
    let held_file = read_held(dir)?;
    let Some(&last) = segments.last() else {
        if let Some(tip) = tip_file.as_ref().filter(|t| t.seq > 0) {
            return Err(broken(1, format!("chain.tip names seq {} and no segment exists", tip.seq)));
        }
        if let Some(held) = held_file.as_ref().filter(|h| h.seq > 0) {
            return Err(broken(1, format!("chain.held names seq {} and no segment exists", held.seq)));
        }
        if let Some(&n) = roots.first() {
            return Err(broken(first_seq(n, size), format!("a signed root closes segment {n} and no segment exists")));
        }
        return Ok(Walked { end: chain.genesis(), unrooted: Vec::new() });
    };
    // A held chain carries `chain.held` or a signed root; an unanchored one carries neither,
    // under an unsigned tip, and only it leaves a full segment before the last unrooted.
    let held = held_file.is_some() || !roots.is_empty();
    let unanchored = !held && tip_file.as_ref().is_some_and(|t| t.signature.is_none());
    let mut unrooted = Vec::new();
    let mut end = chain.genesis();
    for n in 1..=last {
        if !segments.contains(&n) {
            return Err(broken(first_seq(n, size), format!("segment {n} is absent")));
        }
        let path = segment_path(dir, n);
        let text = fs::read_to_string(&path).map_err(unreadable(&path))?;
        let mut entries = Vec::new();
        for line in text.lines() {
            let index = end.seq + 1;
            let entry: AuditEntry =
                serde_json::from_str(line).map_err(|e| broken(index, format!("segment {n}: the line does not parse: {e}")))?;
            if entry.seq != index {
                return Err(broken(index, format!("segment {n}: seq {} where {index} follows", entry.seq)));
            }
            if segment_of(entry.seq, size) != n {
                return Err(broken(index, format!("seq {index} lies outside segment {n}")));
            }
            if entry.format != chain.version() {
                return Err(broken(index, format!("a format {} entry in a v{} chain", entry.format, chain.version())));
            }
            if entry.prev_hash != end.entry_hash {
                return Err(broken(index, "prev_hash does not link to the entry before"));
            }
            if let Some(n) = chain.inexact(&entry.attributes) {
                return Err(broken(index, format!("an attribute holds {n}, an integer beyond ±{AUDIT_EXACT_INTEGER}")));
            }
            if !chain.digest_agrees(&entry) {
                return Err(broken(index, "entry_hash disagrees with the entry"));
            }
            if let Some(tip) = tip_file.as_ref().filter(|t| t.seq == index) {
                if tip.entry_hash != entry.entry_hash {
                    return Err(broken(index, "chain.tip disagrees with the entry it names"));
                }
            }
            if let Some(record) = held_file.as_ref().filter(|h| h.seq == index) {
                if record.entry_hash != entry.entry_hash {
                    return Err(broken(index, "chain.held disagrees with the entry it names"));
                }
            }
            end = entry.tip();
            entries.push(entry);
        }
        let count = entries.len() as u64;
        let closing = end.seq.max(first_seq(n, size));
        if roots.contains(&n) {
            let rpath = root_path(dir, n);
            let text = fs::read_to_string(&rpath).map_err(unreadable(&rpath))?;
            let root: SignedRoot = serde_json::from_str(&text)
                .map_err(|e| broken(closing, format!("the root of segment {n} does not parse: {e}")))?;
            let bound = match chain {
                ChainFormat::V0 => root.format == 0,
                ChainFormat::V1(h) => {
                    root.format == h.format && root.header.as_deref() == Some(h.digest().as_str()) && root.segment == Some(n)
                }
            };
            let expected = chain.root_of(&entries).map_err(|e| broken(closing, e))?;
            if !bound || root.count != size || count != size || root.root != expected {
                return Err(broken(closing, format!("the signed root of segment {n} disagrees with its entries")));
            }
            if key.is_some_and(|k| !root.verify(k)) {
                return Err(broken(closing, format!("the root signature of segment {n} does not verify")));
            }
        } else if n < last && !unanchored {
            return Err(broken(closing, format!("segment {n} is followed by another and carries no signed root")));
        } else if count == size {
            unrooted.push(n);
        }
    }
    if let Some(&n) = roots.range(last + 1..).next() {
        return Err(broken(first_seq(n, size), format!("a signed root closes segment {n} and the segment is absent")));
    }
    if let Some(tip) = tip_file.as_ref().filter(|t| t.seq > end.seq) {
        return Err(broken(end.seq + 1, format!("chain.tip names seq {} beyond the chain end", tip.seq)));
    }
    if let Some(record) = held_file.as_ref().filter(|h| h.seq > end.seq) {
        return Err(broken(end.seq + 1, format!("chain.held names seq {} beyond the chain end", record.seq)));
    }
    // Held and unanchored logs both write `chain.tip` before their first entry.
    if end.seq > 0 {
        match &tip_file {
            None => return Err(broken(1, "the chain carries entries and no chain.tip")),
            Some(tip) if held && tip.signature.is_none() => {
                return Err(broken(tip.seq.max(1), "chain.tip is unsigned over a held chain"));
            }
            Some(_) => {}
        }
    }
    if let Some(record) = held_file.as_ref().filter(|h| key.is_some_and(|k| !held_verifies(h, k))) {
        return Err(broken(record.seq.max(1), "the chain.held signature does not verify"));
    }
    if let Some(key) = key.filter(|_| end.seq > 0) {
        match &tip_file {
            None => return Err(broken(1, "the chain carries entries and no chain.tip")),
            Some(tip) if tip.signature.is_none() => {
                if let Check::Signed(_) = check {
                    return Err(AuditError::AuditLogUnanchored(format!(
                        "{}: chain.tip at seq {} is unsigned; anchor the chain through the issuer's signing port",
                        dir.display(),
                        tip.seq
                    )));
                }
            }
            Some(tip) if !tip.verify(key) => {
                return Err(broken(tip.seq.max(1), "the chain.tip signature does not verify"));
            }
            Some(_) => {}
        }
    }
    Ok(Walked { end, unrooted })
}

/// Take the directory's writer lock, refusing one another handle holds.
fn take_lock(dir: &Path) -> Result<File, AuditError> {
    let seg_dir = segments_dir(dir);
    fs::create_dir_all(&seg_dir).map_err(unreadable(&seg_dir))?;
    let lock_path = dir.join("audit.lock");
    let lock = OpenOptions::new().create(true).truncate(false).write(true).open(&lock_path).map_err(unreadable(&lock_path))?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(fs::TryLockError::WouldBlock) => {
            Err(AuditError::AuditLogHeld(format!("{} is held by another writer", lock_path.display())))
        }
        Err(fs::TryLockError::Error(e)) => Err(unreadable(&lock_path)(e)),
    }
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
    /// `header.json`, or its directory after the header is renamed in.
    Header,
    /// `chain.held`, or its directory after the record is renamed in.
    Held,
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

/// How a log commits, when it signs its tip, and the header a new chain writes.
#[derive(Clone)]
pub struct AuditOptions {
    /// Quiet time after the last append at which the tip signs.
    pub idle: Duration,
    pub fsync: Arc<dyn FsyncPort>,
    /// The header a log opened over an empty directory writes; an existing chain keeps its
    /// own format.
    pub header: ChainHeader,
}

impl Default for AuditOptions {
    fn default() -> AuditOptions {
        AuditOptions { idle: AUDIT_TIP_IDLE, fsync: Arc::new(FileFsync), header: ChainHeader::default() }
    }
}

impl std::fmt::Debug for AuditOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditOptions").field("idle", &self.idle).field("header", &self.header).finish_non_exhaustive()
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

/// The format of the chain under `dir`: its header's, v0 for a chain holding anything
/// without one, and for an empty directory the options' header, checked and written
/// durably before any entry.
fn open_format(dir: &Path, options: &AuditOptions) -> Result<ChainFormat, AuditError> {
    if let Some(header) = read_header(dir)? {
        return Ok(ChainFormat::V1(header));
    }
    let (segments, roots) = listing(dir)?;
    let tip = read_tip(dir)?;
    if !segments.is_empty() || !roots.is_empty() || tip.is_some_and(|t| t.seq > 0) {
        return Ok(ChainFormat::V0);
    }
    options.header.check()?;
    let path = header_path(dir);
    let bytes = serde_json::to_vec(&options.header).map_err(|e| AuditError::Io(format!("{}: {e}", path.display())))?;
    write_durable(&path, &bytes, &*options.fsync, Fsync::Header).map_err(AuditError::AuditEntryUnpersisted)?;
    Ok(ChainFormat::V1(options.header))
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

/// The signer of a log opened with no issuer key: unanchored or read-only. It has no
/// values, so no such log signs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoIssuerKey {}

impl SigningPort for NoIssuerKey {
    fn encoding(&self) -> SignatureEncoding {
        match *self {}
    }
    fn public_key(&self) -> Vec<u8> {
        match *self {}
    }
    fn sign(&self, _: &[u8]) -> Result<Vec<u8>, AuthorityError> {
        match *self {}
    }
}

/// The issuer key custody a log opens under.
enum Custody<S> {
    /// Roots and the tip sign through the port.
    Held(S),
    /// Entries link under an unsigned tip and no root is written.
    Unanchored,
    /// The chain verifies and nothing appends.
    ReadOnly,
}

struct Inner<S> {
    dir: PathBuf,
    chain: ChainFormat,
    custody: Custody<S>,
    fsync: Arc<dyn FsyncPort>,
    idle: Duration,
    /// The exclusive lock on `audit.lock`, held for the log's lifetime by every handle
    /// that appends.
    _lock: Option<File>,
    state: Mutex<State>,
    turn: Condvar,
}

/// The node's audit log: appends link to the tip, commit in groups sharing one segment
/// sync, return only once durable, and, held, close each full segment under a signed root.
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
    /// it. A tip that is unsigned raises `AuditLogUnanchored`: [`AuditLog::anchor`] signs it.
    pub fn open_with(dir: impl Into<PathBuf>, signer: S, options: AuditOptions) -> Result<AuditLog<S>, AuditError> {
        AuditLog::start(dir.into(), Custody::Held(signer), options, false)
    }

    /// [`AuditLog::anchor_with`] under the default options.
    pub fn anchor(dir: impl Into<PathBuf>, signer: S) -> Result<AuditLog<S>, AuditError> {
        AuditLog::anchor_with(dir, signer, AuditOptions::default())
    }

    /// Anchor the chain under `dir` through `signer`: verify its linkage and every present
    /// root and signed tip under the signer's key, sign each full segment's missing root
    /// and the tip, and open it held. An anchored chain opens as [`AuditLog::open_with`]
    /// does.
    pub fn anchor_with(dir: impl Into<PathBuf>, signer: S, options: AuditOptions) -> Result<AuditLog<S>, AuditError> {
        AuditLog::start(dir.into(), Custody::Held(signer), options, true)
    }

    fn start(dir: PathBuf, custody: Custody<S>, options: AuditOptions, anchoring: bool) -> Result<AuditLog<S>, AuditError> {
        let lock = match custody {
            Custody::ReadOnly => None,
            _ => Some(take_lock(&dir)?),
        };
        if let Custody::Unanchored = custody {
            let anchored = |why: String| AuditError::AuditLogAnchored(format!("{}: {why}; open it with the issuer's signing port", dir.display()));
            if read_tip(&dir)?.is_some_and(|t| t.signature.is_some()) {
                return Err(anchored("chain.tip is signed".into()));
            }
            if let Some(n) = listing(&dir)?.1.first() {
                return Err(anchored(format!("a signed root closes segment {n}")));
            }
            if read_held(&dir)?.is_some() {
                return Err(anchored("chain.held records the chain held".into()));
            }
        }
        let chain = match lock {
            Some(_) => {
                drop_torn_tail(&dir, &*options.fsync)?;
                open_format(&dir, &options)?
            }
            None => read_format(&dir)?,
        };
        let walked = match &custody {
            Custody::Held(signer) if anchoring => walk(&dir, Check::Anchoring(&SignerKey::of(signer)))?,
            Custody::Held(signer) => walk(&dir, Check::Signed(&SignerKey::of(signer)))?,
            Custody::Unanchored | Custody::ReadOnly => walk(&dir, Check::Linkage)?,
        };
        let end = walked.end;
        let recorded = read_tip(&dir)?;
        let held = matches!(custody, Custody::Held(_));
        let stale_tip = match &recorded {
            None => true,
            Some(t) => t.seq != end.seq || t.entry_hash != end.entry_hash || (held && t.signature.is_none()),
        };
        let state = State {
            tip: end.clone(),
            signed: recorded.map(|t| ChainTip { seq: t.seq, entry_hash: t.entry_hash }).unwrap_or_else(ChainTip::genesis),
            queue: Vec::new(),
            next_ticket: 0,
            done: HashMap::new(),
            busy: false,
            poisoned: None,
            last_append: Instant::now(),
            closing: false,
        };
        let read_only = lock.is_none();
        let inner = Arc::new(Inner {
            dir,
            chain,
            custody,
            fsync: options.fsync,
            idle: options.idle,
            _lock: lock,
            state: Mutex::new(state),
            turn: Condvar::new(),
        });
        if read_only {
            return Ok(AuditLog { inner, idler: None });
        }
        if held && read_held(&inner.dir)?.is_none() {
            inner.write_held(&end).map_err(AuditError::AuditEntryUnpersisted)?;
        }
        if held {
            for n in &walked.unrooted {
                inner.close(*n).map_err(AuditError::AuditEntryUnpersisted)?;
            }
        }
        if stale_tip {
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
    ///
    /// A v1 batch holding an attribute integer beyond ±[`AUDIT_EXACT_INTEGER`] raises
    /// `AuditAttributeInexact` before it joins a group, and nothing of it appends.
    pub fn append_all(&self, batch: Vec<Value>) -> Result<Vec<AuditEntry>, AuditError> {
        let inner = &*self.inner;
        inner.writable()?;
        if let Some(n) = batch.iter().find_map(|v| inner.chain.inexact(v)) {
            return Err(AuditError::AuditAttributeInexact(format!(
                "an attribute holds {n}, an integer beyond ±{AUDIT_EXACT_INTEGER} that RFC 8785 writes inexactly"
            )));
        }
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
        inner.writable()?;
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

    /// Refuse a write through a read-only handle.
    fn writable(&self) -> Result<(), AuditError> {
        match self.custody {
            Custody::ReadOnly => Err(AuditError::AuditLogReadOnly(format!("{} is open read-only; nothing appends through it", self.dir.display()))),
            _ => Ok(()),
        }
    }

    /// Write one group after `tip`, reversing it on failure. The error carries the reason
    /// and, when the reversal itself failed, the reason the log is poisoned.
    fn commit(&self, tip: &ChainTip, values: Vec<Value>) -> Result<Committed, (String, Option<String>)> {
        let mut undo = Undo::default();
        self.persist(tip, values, &mut undo).map_err(|e| (e, undo.apply(&*self.fsync).err()))
    }

    fn persist(&self, start: &ChainTip, values: Vec<Value>, undo: &mut Undo) -> Result<Committed, String> {
        let seg_dir = segments_dir(&self.dir);
        let size = self.chain.segment_entries();
        let mut tip = start.clone();
        let mut entries = Vec::with_capacity(values.len());
        let mut closed = false;
        let mut pending = values.into_iter().peekable();
        while pending.peek().is_some() {
            let n = segment_of(tip.seq + 1, size);
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
            let room = n * size - tip.seq;
            let mut buf = Vec::new();
            for attributes in pending.by_ref().take(room as usize) {
                let entry = self.chain.link(tip.seq + 1, &tip.entry_hash, attributes);
                serde_json::to_writer(&mut buf, &entry).map_err(fail(&path))?;
                buf.push(b'\n');
                tip = entry.tip();
                entries.push(entry);
            }
            file.write_all(&buf).and_then(|_| self.fsync.sync(Fsync::Segment(n), &file)).map_err(fail(&path))?;
            if !existed {
                File::open(&seg_dir).and_then(|d| self.fsync.sync(Fsync::SegmentOpen(n), &d)).map_err(fail(&seg_dir))?;
            }
            if tip.seq == n * size {
                if let Custody::Held(_) = self.custody {
                    undo.created.push(root_path(&self.dir, n));
                    self.close(n)?;
                }
                closed = true;
            }
        }
        if closed {
            self.write_tip(&tip)?;
        }
        let signed = closed.then(|| tip.clone());
        Ok((entries, tip, signed))
    }

    /// Write `tip` durably as `chain.tip`: signed when held, unsigned when unanchored.
    fn write_tip(&self, tip: &ChainTip) -> Result<SignedTip, String> {
        let signed = match &self.custody {
            Custody::Held(signer) => SignedTip::sign(tip.seq, &tip.entry_hash, signer)?,
            Custody::Unanchored => SignedTip { seq: tip.seq, entry_hash: tip.entry_hash.clone(), signature: None },
            Custody::ReadOnly => return Err("a read-only handle writes no tip".into()),
        };
        let tpath = tip_path(&self.dir);
        write_durable(&tpath, &serde_json::to_vec(&signed).map_err(fail(&tpath))?, &*self.fsync, Fsync::Tip)?;
        Ok(signed)
    }

    /// Record the chain held from `at`, signed, as `chain.held`.
    fn write_held(&self, at: &ChainTip) -> Result<(), String> {
        let Custody::Held(signer) = &self.custody else {
            return Err("only a held log records the chain held".into());
        };
        let signature = sign_through(signer, &held_message(at.seq, &at.entry_hash)).map_err(|e| e.to_string())?;
        let record = SignedTip { seq: at.seq, entry_hash: at.entry_hash.clone(), signature: Some(hex::encode(signature)) };
        let path = held_path(&self.dir);
        write_durable(&path, &serde_json::to_vec(&record).map_err(fail(&path))?, &*self.fsync, Fsync::Held)
    }

    /// Close full segment `n` under a signed root, read back from its synced file.
    fn close(&self, n: u64) -> Result<(), String> {
        let Custody::Held(signer) = &self.custody else {
            return Err(format!("segment {n} closes under no issuer key"));
        };
        let size = self.chain.segment_entries();
        let entries = read_segment(&self.dir, n, n * size).map_err(|e| e.to_string())?;
        let root = self.chain.root_of(&entries)?;
        let root = match self.chain {
            ChainFormat::V0 => SignedRoot::sign(&root, size, signer)?,
            ChainFormat::V1(h) => SignedRoot::sign_v1(&h.digest(), n, &root, size, signer)?,
        };
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

impl AuditLog<NoIssuerKey> {
    /// [`AuditLog::unanchored_with`] under the default options.
    pub fn unanchored(dir: impl Into<PathBuf>) -> Result<AuditLog<NoIssuerKey>, AuditError> {
        AuditLog::unanchored_with(dir, AuditOptions::default())
    }

    /// Open the log under `dir` with no issuer key: take the writer lock and verify the
    /// chain's linkage. Appends link under an unsigned tip and write no root. A chain
    /// holding a signed tip or root raises `AuditLogAnchored`.
    pub fn unanchored_with(dir: impl Into<PathBuf>, options: AuditOptions) -> Result<AuditLog<NoIssuerKey>, AuditError> {
        AuditLog::start(dir.into(), Custody::Unanchored, options, false)
    }

    /// Open the log under `dir` to verify it: no writer lock, the chain's linkage checked,
    /// and every append and export refused with `AuditLogReadOnly`.
    pub fn read_only(dir: impl Into<PathBuf>) -> Result<AuditLog<NoIssuerKey>, AuditError> {
        AuditLog::start(dir.into(), Custody::ReadOnly, AuditOptions::default(), false)
    }
}
