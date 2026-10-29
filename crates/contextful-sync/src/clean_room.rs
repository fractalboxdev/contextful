//! The cross-owner store's boundary below the engine
//! (`disclosure.set-mode.clean-room-preconditions`). Opening one verifies three things
//! before any cross-prefix write: each owner holds its own signing key, each owner's
//! prefix holds a manifest that key signs, and the object store's access policy refuses
//! every owner's credential a write under every other owner's prefix. One missing raises
//! `DisclosureCleanRoomPreconditionUnmet`.
//!
//! The write probe is a put conditioned on an ETag no object carries: an access policy
//! refuses it with `Forbidden` before the condition is read, and a credential the policy
//! admits sees the condition fail, so no probe lands a byte either way.

use contextful_core::disclosure::DisclosureError;
use contextful_core::ports::SigningPort;
use contextful_core::store::object::{Condition, ObjectError, ObjectStore, Put};
use contextful_policy::issue::{sign_through, SignerKey};

/// The manifest object under an owner's prefix.
const MANIFEST: &str = "manifest.json";
/// The hex signature over [`manifest_message`] beside it.
const MANIFEST_SIGNATURE: &str = "manifest.sig";
/// The key the write probe targets under each other owner's prefix.
const PROBE: &str = "clean-room-probe";
/// An ETag no object carries: the probe's condition never holds.
const PROBE_ETAG: &str = "clean-room-probe-never-matches";

/// One owner of a cross-owner store, as the opener sees it.
pub struct OwnerBoundary<'a> {
    pub owner: String,
    /// The key prefix the owner alone writes under, ending in `/`.
    pub prefix: String,
    /// The bucket behind this owner's own credential.
    pub store: &'a dyn ObjectStore,
    /// The owner's signing key; `None` is a missing precondition.
    pub signer: Option<SignerKey>,
}

/// A cross-owner store whose boundary verified at open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossOwnerStore {
    owners: Vec<String>,
}

impl CrossOwnerStore {
    /// The owners, sorted.
    pub fn owners(&self) -> &[String] {
        &self.owners
    }
}

/// The bytes an owner signs for its manifest subtree: a domain tag, the owner and the manifest.
pub fn manifest_message(owner: &str, manifest: &[u8]) -> Vec<u8> {
    let mut m = format!("contextful.clean-room.manifest\0{owner}\0").into_bytes();
    m.extend_from_slice(manifest);
    m
}

fn unmet(why: String) -> DisclosureError {
    DisclosureError::CleanRoomPreconditionUnmet(why)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

/// Write `manifest` and its signature under `prefix` through the owner's own credential.
pub fn write_owner_manifest(store: &dyn ObjectStore, prefix: &str, owner: &str, signer: &dyn SigningPort, manifest: &[u8]) -> Result<(), DisclosureError> {
    let signature = sign_through(signer, &manifest_message(owner, manifest)).map_err(|e| unmet(format!("owner `{owner}` cannot sign its manifest: {e}")))?;
    for (name, bytes) in [(MANIFEST, manifest.to_vec()), (MANIFEST_SIGNATURE, hex(&signature).into_bytes())] {
        store.put(&format!("{prefix}{name}"), &bytes, Condition::None).map_err(|e| unmet(format!("owner `{owner}` cannot write `{prefix}{name}`: {e}")))?;
    }
    Ok(())
}

/// Open a cross-owner store over `owners`, verifying the boundary before any cross-prefix
/// write. An unanswered probe or read counts as a missing precondition: the boundary is
/// unverified, so the store does not open.
pub fn open_cross_owner(owners: &[OwnerBoundary<'_>]) -> Result<CrossOwnerStore, DisclosureError> {
    // Distinct owners: one name twice is one owner, whatever keys and prefixes it carries.
    let mut names: Vec<String> = owners.iter().map(|o| o.owner.clone()).collect();
    names.sort();
    if let Some(twice) = names.windows(2).find(|w| w[0] == w[1]) {
        return Err(unmet(format!("the store names owner `{}` twice; each owner holds one boundary", twice[0])));
    }
    if names.len() < 2 {
        return Err(unmet(format!("a cross-owner store names at least two owners; {} named", names.len())));
    }
    // Per-owner signing keys: present, and no two owners sharing one in any encoding.
    let mut keys = Vec::with_capacity(owners.len());
    for o in owners {
        let Some(key) = &o.signer else { return Err(unmet(format!("owner `{}` declares no signing key", o.owner))) };
        let key = key.canonical();
        if let Some(i) = keys.iter().position(|k| *k == key) {
            return Err(unmet(format!("owners `{}` and `{}` share one signing key", owners[i].owner, o.owner)));
        }
        keys.push(key);
    }
    // Per-owner write prefixes: nonempty, ending in `/`, and disjoint.
    for (i, o) in owners.iter().enumerate() {
        if o.prefix.is_empty() || !o.prefix.ends_with('/') {
            return Err(unmet(format!("owner `{}` declares write prefix `{}`, not a `/`-terminated key prefix", o.owner, o.prefix)));
        }
        if let Some(other) = owners[..i].iter().find(|p| p.prefix.starts_with(&o.prefix) || o.prefix.starts_with(&p.prefix)) {
            return Err(unmet(format!("owners `{}` and `{}` declare overlapping prefixes `{}` and `{}`", other.owner, o.owner, other.prefix, o.prefix)));
        }
    }
    // Per-owner signed manifest subtrees.
    for o in owners {
        verify_manifest(o)?;
    }
    // The access policy refuses each owner's credential under every other owner's prefix:
    // at the probe key, and at every key already there, the manifest pair among them, so a
    // policy admitting a sub-path of the prefix does not pass.
    for (j, other) in owners.iter().enumerate() {
        let mut targets = other
            .store
            .list(&other.prefix)
            .map_err(|e| unmet(format!("owner `{}`'s write prefix `{}` is unlistable: {e}", other.owner, other.prefix)))?;
        targets.push(format!("{}{PROBE}", other.prefix));
        targets.sort();
        targets.dedup();
        for (_, writer) in owners.iter().enumerate().filter(|(i, _)| *i != j) {
            for key in &targets {
                probe(writer, other, key)?;
            }
        }
    }
    Ok(CrossOwnerStore { owners: names })
}

/// One put from `writer`'s credential at `key` under `other`'s prefix, conditioned on an
/// ETag no object carries; only the access policy's refusal passes.
fn probe(writer: &OwnerBoundary<'_>, other: &OwnerBoundary<'_>, key: &str) -> Result<(), DisclosureError> {
    match writer.store.put(key, b"", Condition::IfMatch(PROBE_ETAG.into())) {
        Err(ObjectError::Forbidden(_)) => Ok(()),
        Ok(Put::ConditionFailed) | Ok(Put::Applied(_)) => Err(unmet(format!(
            "owner `{}`'s credential writes `{key}` under owner `{}`'s write prefix `{}`; the object store's access policy does not enforce it",
            writer.owner, other.owner, other.prefix
        ))),
        Err(e) => Err(unmet(format!("the write prefix of owner `{}` is unverified from owner `{}`'s credential at `{key}`: {e}", other.owner, writer.owner))),
    }
}

fn verify_manifest(o: &OwnerBoundary<'_>) -> Result<(), DisclosureError> {
    let read = |name: &str| -> Result<Vec<u8>, DisclosureError> {
        let key = format!("{}{name}", o.prefix);
        match o.store.get(&key) {
            Ok(Some((bytes, _))) => Ok(bytes),
            Ok(None) => Err(unmet(format!("owner `{}`'s manifest subtree lacks `{key}`", o.owner))),
            Err(e) => Err(unmet(format!("owner `{}`'s manifest subtree is unreadable at `{key}`: {e}", o.owner))),
        }
    };
    let manifest = read(MANIFEST)?;
    let signature = read(MANIFEST_SIGNATURE)?;
    let signature = std::str::from_utf8(&signature).ok().and_then(unhex);
    let key = o.signer.as_ref().expect("signing keys verified first");
    if !signature.is_some_and(|s| key.verifies(&manifest_message(&o.owner, &manifest), &s)) {
        return Err(unmet(format!("owner `{}`'s manifest subtree does not verify under its signing key", o.owner)));
    }
    Ok(())
}
