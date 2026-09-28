//! The ports the domain calls through: signing and the clock. Adapters live outside
//! this crate (`topology.package.domain-impurity`).

use crate::issue::SignatureAlgorithm;
use crate::time::Instant;
use crate::AuthorityError;

/// The one signing port every signature runs through: a credential's authority block and
/// the audit chain's segment roots and tip (`authority.issue.signing-port`). A seed file,
/// a secret reference resolved at mint time and a remote signing oracle (an HSM, a cloud
/// KMS, a platform enclave) are adapters behind it; the private key never crosses it.
///
/// Encodings, by [`SigningPort::algorithm`] (`authority.issue.signature-encoding`):
///
/// | Scheme | `public_key` | `sign` returns |
/// | --- | --- | --- |
/// | Ed25519 | the 32-byte key | the 64-byte RFC 8032 signature over `message` |
/// | ES256 | a SEC1 P-256 point, compressed (33 bytes) or not (65) | ECDSA over the SHA-256 digest of `message`, as an ASN.1 DER `Ecdsa-Sig-Value`, never the 64-byte `r ‖ s` form |
///
/// A port hashes `message` itself: it receives the payload, never a digest.
pub trait SigningPort {
    /// The scheme of the key behind the port; it is authoritative for the credential.
    fn algorithm(&self) -> SignatureAlgorithm;
    /// The public half of the signing key, in the scheme's encoding above.
    fn public_key(&self) -> Vec<u8>;
    /// A signature over `message`, in the scheme's encoding above.
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
