//! The ports the domain calls through: signing and the clock. Adapters live outside
//! this crate (`topology.package.domain-impurity`).

use crate::issue::SignatureAlgorithm;
use crate::time::Instant;
use crate::AuthorityError;

/// The one signing port every mint runs through (`authority.issue.signing-port`). A seed
/// file, a secret reference resolved at mint time and a remote signing oracle are
/// adapters behind it.
pub trait SigningPort {
    /// The scheme of the key behind the port; it is authoritative for the credential.
    fn algorithm(&self) -> SignatureAlgorithm;
    /// The public half of the signing key, in the scheme's raw encoding.
    fn public_key(&self) -> Vec<u8>;
    /// A signature over `message`.
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, AuthorityError>;
}

/// The source of the evaluation instant.
pub trait Clock {
    fn now(&self) -> Instant;
}

/// A clock that always reads one instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedClock(pub Instant);

impl Clock for FixedClock {
    fn now(&self) -> Instant {
        self.0
    }
}
