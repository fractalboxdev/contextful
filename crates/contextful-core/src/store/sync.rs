//! `store.push`, `store.pull`, `store.probe` and `store.merge`: the sync configuration,
//! prefix confinement, the bucket manifest, key ownership and the scoped-union merge.

use super::StoreError;
use crate::connector::attach::is_loopback_host;
use crate::connector::reference::{SecretName, SCHEME};
use crate::time::Instant;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Default re-commit rounds on a lost manifest compare-and-set: 5 attempts (`store.merge.retries`).
pub const PUSH_RETRIES: u32 = 5;
/// Manifest re-fetches a pull performs against keys moving beneath it: 3 attempts (`store.pull.convergence`).
pub const PULL_CONVERGENCE: u32 = 3;
/// Age at which a tombstone leaves the bucket manifest: 30 d (`store.merge.tombstone-ttl`).
pub const TOMBSTONE_TTL_SECS: u64 = 30 * 24 * 60 * 60;

/// The bucket manifest's key under the prefix.
pub const MANIFEST_KEY: &str = "manifest.json";
/// Where a probe writes its sentinel under the prefix.
pub const PROBE_PREFIX: &str = "_contextful/cas-probe/";

/// `[sync]` in a store's `config.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncConfig {
    pub endpoint: String,
    pub bucket: String,
    #[serde(default)]
    pub prefix: Option<String>,
    /// `env:<NAME>`: the variable holding the prefix.
    #[serde(default)]
    pub prefix_from: Option<String>,
    /// `cas`, or `single-writer`.
    #[serde(default)]
    pub coordination: Option<String>,
    #[serde(default)]
    pub push_retries: Option<u32>,
    /// Pull the bucket before a run or the tool server first reads (`store.pull.before-run`).
    #[serde(default)]
    pub pull_before_run: Option<bool>,
    /// The signing region of an `https://` or loopback `http://` endpoint; `us-east-1` when absent.
    #[serde(default)]
    pub region: Option<String>,
    /// `secret://<name>` or `env://NAME`: the S3 access key id (`store.endpoint.credentials`).
    #[serde(default)]
    pub access_key_id: Option<String>,
    /// `secret://<name>` or `env://NAME`: the S3 secret access key.
    #[serde(default)]
    pub secret_access_key: Option<String>,
    /// `secret://<name>` or `env://NAME`: an S3 session token, for temporary credentials.
    #[serde(default)]
    pub session_token: Option<String>,
}

/// The signing region of a generic S3-compatible endpoint that declares none.
pub const DEFAULT_REGION: &str = "us-east-1";

/// Where a `[sync] endpoint` points (`store.endpoint.schemes`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// A filesystem bucket under this directory.
    File(String),
    /// An S3-compatible endpoint: its base URL, addressed path-style, and its signing region.
    S3 { url: String, region: String },
}

/// Where a credential key's material comes from (`store.endpoint.credentials`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialRef {
    /// `secret://<name>`, hydrated through the provider chain.
    Secret(SecretName),
    /// `env://NAME`, read whole from the process environment.
    Env(String),
}

/// The credential references an S3 or R2 bucket signs with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialRefs {
    pub access_key_id: CredentialRef,
    pub secret_access_key: CredentialRef,
    pub session_token: Option<CredentialRef>,
}

/// One `[a-z0-9-]+` host label of at most 63 chars: a region or an account id.
fn label(s: &str) -> bool {
    !s.is_empty() && s.len() <= 63 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The host of `authority`: without its port, and an IPv6 literal without its brackets.
fn host_of(authority: &str) -> &str {
    match authority.strip_prefix('[') {
        Some(rest) => rest.split_once(']').map_or(authority, |(h, _)| h),
        None => authority.split(':').next().unwrap_or(authority),
    }
}

impl SyncConfig {
    /// Resolve the prefix: `prefix` or the variable `prefix_from` names, never both and
    /// never the bucket root by default (`store.push.prefix-overspecified`, `store.push.prefix-unbound`).
    pub fn resolve_prefix(&self, env: impl Fn(&str) -> Option<String>) -> Result<String, StoreError> {
        let raw = match (&self.prefix, &self.prefix_from) {
            (Some(_), Some(_)) => {
                return Err(StoreError::SyncPrefixOverspecified("`[sync]` declares `prefix` and `prefix_from`; declare one".into()))
            }
            (Some(p), None) => p.clone(),
            (None, Some(from)) => {
                let var = from.strip_prefix("env:").unwrap_or(from);
                env(var).filter(|v| !v.is_empty()).ok_or_else(|| {
                    StoreError::SyncPrefixUnbound(format!("`prefix_from` names `{var}`, which is unset; the bucket root is no fallback"))
                })?
            }
            (None, None) => String::new(),
        };
        Ok(raw.trim_matches('/').to_string())
    }

    pub fn push_retries(&self) -> u32 {
        self.push_retries.unwrap_or(PUSH_RETRIES)
    }

    /// Resolve `endpoint` to a bucket adapter's address (`store.endpoint.schemes`): `file://`,
    /// `s3://<region>`, `r2://<account-id>`, `https://` and loopback `http://`.
    pub fn resolve_endpoint(&self) -> Result<Endpoint, StoreError> {
        let e = self.endpoint.as_str();
        let unsupported = |why: &str| {
            StoreError::SyncEndpointUnsupported(format!(
                "endpoint `{e}`: {why}; the schemes are `file://<directory>`, `s3://<region>`, `r2://<account-id>`, `https://<host>` and loopback `http://<host>`"
            ))
        };
        let Some((scheme, rest)) = e.split_once("://") else { return Err(unsupported("no scheme")) };
        match scheme {
            "file" if !rest.is_empty() => Ok(Endpoint::File(rest.to_string())),
            "s3" => {
                let r = rest.trim_end_matches('/');
                if !label(r) {
                    return Err(unsupported("`s3://` takes one region, such as `s3://eu-west-1`"));
                }
                Ok(Endpoint::S3 { url: format!("https://s3.{r}.amazonaws.com"), region: r.to_string() })
            }
            "r2" => {
                let account = rest.trim_end_matches('/');
                if !label(account) {
                    return Err(unsupported("`r2://` takes one account id"));
                }
                Ok(Endpoint::S3 { url: format!("https://{account}.r2.cloudflarestorage.com"), region: "auto".to_string() })
            }
            "https" | "http" => {
                let authority = rest.split('/').next().unwrap_or_default();
                if authority.is_empty() || authority.contains('@') {
                    return Err(unsupported("the endpoint names no host, or carries user information"));
                }
                let host = host_of(authority);
                if scheme == "http" && !is_loopback_host(host) {
                    return Err(StoreError::SyncEndpointInsecure(format!(
                        "endpoint `{e}`: plain `http://` reaches a loopback host only, and `{host}` is not one; use `https://`"
                    )));
                }
                let region = self.region.clone().unwrap_or_else(|| DEFAULT_REGION.to_string());
                Ok(Endpoint::S3 { url: e.trim_end_matches('/').to_string(), region })
            }
            _ => Err(unsupported("no bucket adapter answers this scheme")),
        }
    }

    /// The credential references an S3 or R2 bucket signs with (`store.endpoint.credentials`):
    /// each key binds `secret://<name>` or `env://NAME`, and a missing required key or any
    /// other value refuses (`store.endpoint.credential-unbound`). A refusal names the key,
    /// never its value, which may be material.
    pub fn credential_refs(&self) -> Result<CredentialRefs, StoreError> {
        let parse = |key: &str, value: &Option<String>| -> Result<Option<CredentialRef>, StoreError> {
            let Some(v) = value else { return Ok(None) };
            if let Some(name) = v.strip_prefix(SCHEME) {
                return SecretName::parse(name)
                    .map(|n| Some(CredentialRef::Secret(n)))
                    .map_err(|_| StoreError::SyncCredentialUnbound(format!("`[sync] {key}` names no well-formed `secret://<name>`")));
            }
            if let Some(var) = v.strip_prefix("env://") {
                let well_formed =
                    var.bytes().next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_') && var.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
                if well_formed {
                    return Ok(Some(CredentialRef::Env(var.to_string())));
                }
                return Err(StoreError::SyncCredentialUnbound(format!("`[sync] {key}` names no well-formed `env://NAME`")));
            }
            Err(StoreError::SyncCredentialUnbound(format!("`[sync] {key}` holds a literal; bind it as `secret://<name>` or `env://NAME`")))
        };
        let required = |key: &str, value: &Option<String>| -> Result<CredentialRef, StoreError> {
            parse(key, value)?.ok_or_else(|| StoreError::SyncCredentialUnbound(format!("endpoint `{}` signs its requests and `[sync]` binds no `{key}`", self.endpoint)))
        };
        Ok(CredentialRefs {
            access_key_id: required("access_key_id", &self.access_key_id)?,
            secret_access_key: required("secret_access_key", &self.secret_access_key)?,
            session_token: parse("session_token", &self.session_token)?,
        })
    }

    /// Whether the push is declared to coordinate by compare-and-set.
    pub fn declares_cas(&self) -> bool {
        self.coordination.as_deref().is_none_or(|c| c == "cas")
    }
}

/// `prefix` joined with `rel`, refusing a relative key that climbs or starts absolute
/// out of the prefix (`store.push.prefix-escape`).
pub fn confine(prefix: &str, rel: &str) -> Result<String, StoreError> {
    let escapes = rel.starts_with('/') || rel.split('/').any(|seg| seg == ".." || seg == ".") || rel.contains('\\') || rel.is_empty();
    if escapes {
        return Err(StoreError::SyncPrefixEscape(format!("key `{rel}` resolves outside the prefix `{prefix}`")));
    }
    Ok(if prefix.is_empty() { rel.to_string() } else { format!("{prefix}/{rel}") })
}

/// One object the bucket manifest lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub sha256: String,
    pub size: u64,
    /// The node that owns the key, or empty for an unowned key.
    #[serde(default)]
    pub owner: String,
}

/// A deletion a writer records for an entry it owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tombstone {
    pub owner: String,
    pub deleted_at: Instant,
}

/// The bucket manifest: key (relative to the prefix) to entry, and tombstones.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BucketManifest {
    #[serde(default)]
    pub entries: BTreeMap<String, Entry>,
    #[serde(default)]
    pub tombstones: BTreeMap<String, Tombstone>,
}

/// The node owning a key, read off the key itself: a run directory's node segment, or a
/// request-ledger file's node. Every other key is unowned.
pub fn owner_of(key: &str) -> Option<String> {
    let segs: Vec<&str> = key.split('/').collect();
    // `<project>/cursors/<pipeline-id>/<node-id>/<seq>.json`: a node's own commit log.
    if segs.len() == 5 && segs[1] == "cursors" {
        return Some(segs[3].to_string());
    }
    // The last `data/runs` pair: `.../data/runs/<run-id>/<node-id>/<file>`.
    if let Some(i) = segs.windows(2).rposition(|w| w == ["data", "runs"]) {
        return (segs.len() > i + 4).then(|| segs[i + 3].to_string());
    }
    // `.../requests/<run-id>.<node-id>.parquet`, a node id holding no dot.
    if segs.len() >= 2 && segs[segs.len() - 2] == "requests" {
        let stem = segs[segs.len() - 1].strip_suffix(".parquet")?;
        return stem.rsplit_once('.').map(|(_, node)| node.to_string());
    }
    None
}

/// Whether `key` is a table pointer, which commits by a fenced compare-and-set and never
/// through the manifest.
pub fn is_pointer(key: &str) -> bool {
    key.ends_with("/_pointer.json")
}

/// Whether `key` is a commit-log entry, which resolves through its commit, never by recency.
pub fn is_commit_log(key: &str) -> bool {
    key.split('/').nth(1) == Some("cursors")
}

/// What a merge produced: the manifest to commit, and the refusals it answered by keeping an entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    pub manifest: BucketManifest,
    pub refused: Vec<StoreError>,
}

/// Merge this writer's local entries into the remote manifest by a scoped union
/// (`store.merge`): a local entry counts only for a key `me` owns or an unowned one, and
/// the remote contributes an entry only where `me` has never written,
/// so an entry `me` owns and no longer holds leaves with a tombstone. A tombstone naming
/// another owner's entry refuses and keeps the entry; a tombstone past its TTL leaves. A
/// commit-log entry whose copies differ refuses outright: a cursor resolves through its
/// commit, never through whichever copy was written last.
pub fn merge(remote: &BucketManifest, local: &BTreeMap<String, Entry>, me: &str, now: Instant) -> Result<Merged, StoreError> {
    let mut out = BucketManifest::default();
    let mut refused = Vec::new();
    for (key, entry) in local {
        // A key another node owns lists the owner's entry, never a local copy of it.
        if !entry.owner.is_empty() && entry.owner != me {
            continue;
        }
        if let Some(theirs) = remote.entries.get(key) {
            if is_commit_log(key) && theirs.sha256 != entry.sha256 {
                return Err(StoreError::SyncCursorConflict(format!(
                    "commit-log entry `{key}` differs between this node and the bucket; a cursor resolves through its commit, not by recency"
                )));
            }
        }
        out.entries.insert(key.clone(), entry.clone());
    }
    for (key, entry) in &remote.entries {
        if out.entries.contains_key(key) {
            continue;
        }
        if entry.owner == me {
            // Owned here and no longer held here: its deletion propagates.
            out.tombstones.insert(key.clone(), Tombstone { owner: me.to_string(), deleted_at: now });
            continue;
        }
        out.entries.insert(key.clone(), entry.clone());
    }
    for (key, t) in &remote.tombstones {
        if out.tombstones.contains_key(key) || t.deleted_at.secs_until(now) >= TOMBSTONE_TTL_SECS {
            continue;
        }
        match out.entries.get(key) {
            Some(e) if e.owner != t.owner => {
                refused.push(StoreError::SyncTombstoneForeign(format!(
                    "a tombstone by `{}` names `{key}`, which `{}` owns; the entry stays",
                    t.owner, e.owner
                )));
                continue;
            }
            Some(e) if e.owner == me => continue,
            Some(_) => {
                out.entries.remove(key);
            }
            None => {}
        }
        out.tombstones.insert(key.clone(), t.clone());
    }
    Ok(Merged { manifest: out, refused })
}

/// The coordination a probe outcome resolves (`store.probe`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coordination {
    /// Conditional writes were demonstrated.
    Cas,
    /// They were not; one writer at a time.
    SingleWriter,
}

impl Coordination {
    /// Hold a declared `cas` to what the probe demonstrated (`store.probe.unproven`).
    pub fn admit(self, config: &SyncConfig) -> Result<(), StoreError> {
        if config.declares_cas() && self != Coordination::Cas {
            return Err(StoreError::SyncCoordinationUnproven(format!(
                "`coordination = \"cas\"` and the probe did not demonstrate conditional writes against `{}`",
                config.endpoint
            )));
        }
        Ok(())
    }
}
