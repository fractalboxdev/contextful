//! The ports the domain calls through: signing and the clock. Adapters live outside
//! this crate (`topology.package.domain-impurity`).

use crate::issue::{SignatureAlgorithm, SignatureEncoding};
use crate::time::Instant;
use crate::AuthorityError;

/// The one signing port every signature runs through: a credential's authority block and
/// the audit chain's segment roots and tip (`authority.issue.signing-port`). A seed file,
/// a secret reference resolved at mint time and a remote signing oracle (an HSM, a cloud
/// KMS, a platform enclave) are adapters behind it; the private key never crosses it.
///
/// A port names the encoding it answers in (`authority.issue.signature-encoding`):
///
/// | [`SignatureEncoding`] | `public_key` | `sign` returns |
/// | --- | --- | --- |
/// | `ed25519` | the 32-byte key | the 64-byte RFC 8032 signature over `message` |
/// | `es256-der` | a SEC1 P-256 point, compressed (33 bytes) or not (65) | ECDSA over the SHA-256 digest of `message`, as an ASN.1 DER `Ecdsa-Sig-Value` |
/// | `es256-raw` | as `es256-der` | the same signature as the 64-byte `r ‖ s` |
///
/// A port hashes `message` itself: it receives the payload, never a digest. An adapter
/// answers in its custodian's native encoding; the caller converts `es256-raw` to DER
/// where the signature leaves the port (`authority.issue.der-at-the-edge`).
pub trait SigningPort {
    /// The encoding of the port's signatures, which names the key's scheme.
    fn encoding(&self) -> SignatureEncoding;
    /// The public half of the signing key, in the encoding above.
    fn public_key(&self) -> Vec<u8>;
    /// A signature over `message`, in the encoding above.
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, AuthorityError>;
    /// The scheme of the key behind the port; it is authoritative for the credential.
    fn algorithm(&self) -> SignatureAlgorithm {
        self.encoding().scheme()
    }
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
