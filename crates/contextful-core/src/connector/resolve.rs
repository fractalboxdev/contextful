//! `connector.resolve`: the provider port every credential backend sits behind.

use super::reference::{Hydrated, SecretName};
use crate::run::Failure;
use crate::time::Instant;

/// What a provider answers for a name: the material, an expiry for a lease, and a version
/// for a versioned store.
#[derive(Debug, PartialEq, Eq)]
pub struct Answer {
    pub value: Hydrated,
    pub expires_at: Option<Instant>,
    /// The store's version of the value, for an adapter that versions it; a rotation's put
    /// is conditioned on it (`connector.rotate.oauth-credential`).
    pub version: Option<String>,
}

/// One adapter answering a reference with material (`connector.resolve.provider-port`).
pub trait Provider: Send + Sync {
    /// The adapter's name, as the run's provider attribution records it.
    fn name(&self) -> &str;
    /// The material for `name`, `None` when this adapter holds none.
    fn answer(&self, name: &SecretName) -> Result<Option<Answer>, Failure>;
    /// Whether this adapter takes part in template hydration.
    fn serves_templates(&self) -> bool {
        true
    }
}
