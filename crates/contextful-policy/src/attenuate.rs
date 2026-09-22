//! `authority.attenuate`, chain side: a holder appends a signed block offline.
//!
//! The parent's bytes stay unchanged. The deriving holder checks the proposal against
//! the parent's chain-final authority through the domain's narrowing rules; admission
//! checks the whole chain again. The library gives each block its own revocation
//! identifier and advances an ephemeral key per block, so a truncated chain verifies as
//! nothing.

use crate::profile::{hop_facts, read_chain, Hop, SUPPORTED_PROFILE_VERSIONS};
use crate::verify::token_bytes;
use biscuit_auth::{BlockBuilder, UnverifiedBiscuit};
use contextful_core::claims::Confirmation;
use contextful_core::grant::{Action, Grant, TablePattern};
use contextful_core::identify::SubjectDerivation;
use contextful_core::time::Instant;
use contextful_core::AuthorityError;

/// What a derivation proposes. An absent dimension inherits the parent's.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Derivation {
    pub grants: Option<Vec<Grant>>,
    pub expires_at: Option<Instant>,
    pub subject: SubjectDerivation,
    /// The thumbprint of the key the child binds, one per sub-agent
    /// (`authority.attenuate.per-sub-agent`).
    pub confirmation: Option<String>,
}

impl Derivation {
    /// Each of `parent`'s grants with its actions and tables replaced where named, every
    /// other dimension kept.
    pub fn narrowing(parent: &[Grant], actions: Option<&[Action]>, tables: Option<&[TablePattern]>) -> Derivation {
        let grants = parent
            .iter()
            .map(|g| {
                let mut g = g.clone();
                if let Some(actions) = actions {
                    g.actions = actions.to_vec();
                }
                if let Some(tables) = tables {
                    g.tables = tables.to_vec();
                }
                g
            })
            .collect();
        Derivation { grants: Some(grants), ..Derivation::default() }
    }

    fn hop(&self) -> Hop {
        Hop {
            grants: self.grants.clone(),
            exp: self.expires_at.map(Instant::unix_secs),
            sub: self.subject.clone(),
            cnf: self.confirmation.clone().map(|jkt| Confirmation { jkt }),
        }
    }
}

/// Append a block proposing `derivation` to `credential`, with no issuer round trip.
pub fn attenuate(credential: &str, derivation: &Derivation) -> Result<String, AuthorityError> {
    let bytes = token_bytes(credential)?;
    let chain = read_chain(&bytes, SUPPORTED_PROFILE_VERSIONS)?;
    let parent = chain.effective()?;
    let hop = derivation.hop();
    contextful_core::attenuate::attenuate(&parent, &hop.proposal())?;
    let mut block = BlockBuilder::new();
    for f in hop_facts(&hop)? {
        block = block.fact(f).map_err(|e| AuthorityError::ProfileElementUnrecognized(e.to_string()))?;
    }
    let unsigned = |e: biscuit_auth::error::Token| AuthorityError::SignatureInvalid(format!("the parent takes no block: {e}"));
    UnverifiedBiscuit::from(&bytes)
        .and_then(|t| t.append(block))
        .and_then(|t| t.to_base64())
        .map_err(unsigned)
}
