//! The self-hosted control plane (`topology.package.control-profile`).
//!
//! Team state is the configuration — ingest sources, models and access policy — held as one
//! CRDT document whose replicas merge concurrent edits (`topology.package.control-team-state`).
//! An apply admits the operator by the console's attestation alone
//! (`topology.package.control-identity`), materializes canonical TOML
//! (`topology.package.canonical-toml`) and claims it through the shared snapshot directory
//! (`topology.package.apply-home`).

mod canonical;

pub use canonical::canonical;

use contextful_policy::operator::{verify, Signed};
use contextful_snapshot::{ControlError, SnapshotDir};
use loro::{ExportMode, LoroDoc, LoroText, UpdateOptions};

/// The route an apply's attestation signs, as the console signs the served apply.
pub const APPLY_TARGET: &str = "/control/apply";

/// The text container holding the configuration inside the document.
const CONFIGURATION: &str = "configuration";

fn storage(e: impl std::fmt::Display) -> ControlError {
    ControlError::Storage(format!("the configuration document: {e}"))
}

/// One replica of the configuration document.
pub struct ConfigDoc {
    doc: LoroDoc,
}

impl ConfigDoc {
    /// An empty replica writing as `peer`.
    pub fn new(peer: u64) -> Result<ConfigDoc, ControlError> {
        let doc = LoroDoc::new();
        doc.set_peer_id(peer).map_err(storage)?;
        Ok(ConfigDoc { doc })
    }

    /// A replica writing as `peer`, holding the state `bytes` exported.
    pub fn load(peer: u64, bytes: &[u8]) -> Result<ConfigDoc, ControlError> {
        let replica = ConfigDoc::new(peer)?;
        replica.merge(bytes)?;
        Ok(replica)
    }

    fn text_container(&self) -> LoroText {
        self.doc.get_text(CONFIGURATION)
    }

    /// The configuration text this replica holds.
    pub fn text(&self) -> String {
        self.text_container().to_string()
    }

    /// Replace the configuration with `text` as the smallest edit from the current text, so
    /// a concurrent edit elsewhere in the document survives the merge.
    pub fn edit(&self, text: &str) -> Result<(), ControlError> {
        self.text_container().update(text, UpdateOptions::default()).map_err(storage)?;
        self.doc.commit();
        Ok(())
    }

    /// This replica's state, for another replica to merge.
    pub fn export(&self) -> Result<Vec<u8>, ControlError> {
        self.doc.export(ExportMode::Snapshot).map_err(storage)
    }

    /// Merge another replica's exported state into this one.
    pub fn merge(&self, bytes: &[u8]) -> Result<(), ControlError> {
        self.doc.import(bytes).map_err(storage)?;
        Ok(())
    }
}

/// The console's signature over one apply, as its operator headers carry it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attestation {
    pub operator: String,
    pub time: String,
    pub nonce: String,
    pub signature: String,
}

/// Apply the document's configuration: canonicalize it, admit the operator whose attestation
/// signs the canonical text at `now`, claim the nonce, then claim the version after
/// `expected` — the guarded import when `expected` is `None`. Returns the claimed version.
pub fn apply(
    dir: &SnapshotDir,
    doc: &ConfigDoc,
    expected: Option<u64>,
    secret: &str,
    attestation: &Attestation,
    now: i64,
) -> Result<u64, ControlError> {
    dir.admit()?;
    let document = canonical(&doc.text())?;
    let signed = Signed {
        method: "POST",
        target: APPLY_TARGET,
        body: document.as_bytes(),
        operator: &attestation.operator,
        time: &attestation.time,
        nonce: &attestation.nonce,
        signature: &attestation.signature,
    };
    let refused = || ControlError::OperatorAttestationInvalid("no fresh console signature over this apply".into());
    let operator = verify(secret, &signed, now).ok_or_else(refused)?;
    if !dir.claim_attestation_nonce(&operator.nonce, operator.signed_at, now)? {
        return Err(ControlError::OperatorAttestationInvalid("the console signature's nonce was already used".into()));
    }
    match expected {
        None => dir.import(&document),
        Some(version) => {
            dir.initialized()?;
            dir.claim(Some(version), &document)
        }
    }
}
