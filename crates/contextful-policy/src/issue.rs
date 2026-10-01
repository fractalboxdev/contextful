//! `authority.issue`, credential side: the seed-file signing adapter and the mint that
//! encodes a checked [`MintPlan`] as a signed authority block.
//!
//! A seed file holds the issuer private key in the library's text form
//! (`ed25519-private/<hex>` or `secp256r1-private/<hex>`); its public half prints as
//! `ed25519/<hex>` or `secp256r1/<hex>`, the static-pin form a checkpoint reads.

use crate::profile::authority_facts;
use crate::verify::TOKEN_BASE64;
use base64::Engine;
use biscuit_auth::format::schema;
use biscuit_auth::{Algorithm, BiscuitBuilder, KeyPair, PrivateKey};
use contextful_core::claims::{AuthorityBlock, Confirmation, Revocation};
use contextful_core::issue::{MintPlan, SignatureAlgorithm, SignatureEncoding};
use contextful_core::ports::SigningPort;
use contextful_core::AuthorityError;
use prost::Message;
use rand::RngCore;
use std::path::Path;

/// The command that writes a seed file, printed wherever an issuer key is missing.
pub const KEYGEN_COMMAND: &str = "contextful token keygen --out .contextful/issuer.seed";

/// The seed file a mint naming no issuer key reads (`authority.issue.default-key`).
pub const DEFAULT_SEED_PATH: &str = ".contextful/issuer.seed";

/// An issuer key held in-process, loaded from a seed file or generated. It signs only
/// through [`SigningPort`]; no accessor hands out the key pair
/// (`authority.issue.signing-port`):
///
/// ```compile_fail
/// use contextful_core::issue::SignatureAlgorithm;
/// let signer = contextful_policy::issue::SeedSigner::generate(SignatureAlgorithm::Ed25519);
/// let _ = signer.key_pair();
/// ```
pub struct SeedSigner {
    key: KeyPair,
}

impl std::fmt::Debug for SeedSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeedSigner").field("public_key", &self.public_key_text()).finish_non_exhaustive()
    }
}

impl SeedSigner {
    /// A fresh key under `algorithm`.
    pub fn generate(algorithm: SignatureAlgorithm) -> SeedSigner {
        let algorithm = match algorithm {
            SignatureAlgorithm::Ed25519 => Algorithm::Ed25519,
            SignatureAlgorithm::Es256 => Algorithm::Secp256r1,
        };
        SeedSigner { key: KeyPair::new_with_algorithm(algorithm) }
    }

    /// Decode a seed file's text; anything else raises `IssuerKeyUnresolvable`.
    pub fn from_seed(text: &str) -> Result<SeedSigner, AuthorityError> {
        let private: PrivateKey = text
            .trim()
            .parse()
            .map_err(|e| AuthorityError::IssuerKeyUnresolvable(format!("the seed holds no issuer key: {e}")))?;
        Ok(SeedSigner { key: KeyPair::from(&private) })
    }

    /// Resolve a configured issuer key reference: none configured raises `IssuerKeyMissing`
    /// naming the command that writes one; a reference resolving to no material raises
    /// `IssuerKeyUnresolvable`. Nothing is written: no local key is fabricated.
    pub fn resolve(reference: Option<&Path>) -> Result<SeedSigner, AuthorityError> {
        let Some(path) = reference else {
            return Err(AuthorityError::IssuerKeyMissing(format!("no issuer key is configured; run `{KEYGEN_COMMAND}`")));
        };
        let text = std::fs::read_to_string(path).map_err(|e| {
            AuthorityError::IssuerKeyUnresolvable(format!("issuer key `{}` resolves to no material: {e}", path.display()))
        })?;
        SeedSigner::from_seed(&text)
            .map_err(|e| AuthorityError::IssuerKeyUnresolvable(format!("issuer key `{}`: {e}", path.display())))
    }

    /// The seed file's text.
    pub fn seed(&self) -> String {
        self.key.private().to_prefixed_string()
    }

    /// The public key in static-pin form, `<algorithm>/<hex>`.
    pub fn public_key_text(&self) -> String {
        self.key.public().to_string()
    }
}

/// The library signs Ed25519 as 64 raw bytes and ES256 as DER.
impl SigningPort for SeedSigner {
    fn encoding(&self) -> SignatureEncoding {
        match self.key.public() {
            biscuit_auth::PublicKey::Ed25519(_) => SignatureEncoding::Ed25519,
            biscuit_auth::PublicKey::P256(_) => SignatureEncoding::Es256Der,
        }
    }

    fn public_key(&self) -> Vec<u8> {
        self.key.public().to_bytes()
    }

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, AuthorityError> {
        self.key
            .sign(message)
            .map(|s| s.to_bytes().to_vec())
            .map_err(|e| AuthorityError::IssuerKeyUnresolvable(format!("the issuer key signs nothing: {e}")))
    }
}

/// The claims a mint adds beyond the plan: the holder key binding and the scoped epoch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MintClaims {
    /// The holder key thumbprint the credential binds (`authority.verify.possession-binding`).
    pub confirmation: Option<String>,
    /// The scoped revocation epoch the credential is minted under (`authority.revoke.epoch`).
    pub epoch: u64,
}

/// The authority block a plan mints: a fresh credential identifier, the subject
/// normalized with its attestations, and the revocation identity `rev://<jti>`.
pub fn authority_block(plan: &MintPlan, claims: &MintClaims) -> AuthorityBlock {
    let mut jti = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut jti);
    let jti = hex::encode(jti);
    let subject = plan.subject.clone().normalize();
    AuthorityBlock {
        iss: plan.issuer.clone().unwrap_or_else(|| plan.audience.clone()),
        aud: plan.audience.clone(),
        rev: Revocation { id: format!("rev://{jti}"), epoch: claims.epoch },
        jti,
        iat: plan.issued_at.unix_secs(),
        exp: plan.expires_at.unix_secs(),
        alg: plan.algorithm.to_string(),
        cnf: claims.confirmation.clone().map(|jkt| Confirmation { jkt }),
        att: subject.attestations(),
        sub: subject.to_subject(),
        grants: plan.grants.clone(),
    }
}

/// The public half of a signing port's key, which verifies what the port signs once
/// [`sign_through`] has brought it to the stored encoding.
///
/// Its text form is `ed25519:<hex>` or `es256:<hex>` of the SEC1 point, compressed or
/// not (`authority.issue.public-key-text`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignerKey {
    pub algorithm: SignatureAlgorithm,
    /// The port's public key encoding: 32 raw bytes, or a SEC1 P-256 point.
    pub public_key: Vec<u8>,
}

impl SignerKey {
    /// The key `port` signs under.
    pub fn of(port: &dyn SigningPort) -> SignerKey {
        SignerKey { algorithm: port.algorithm(), public_key: port.public_key() }
    }

    /// Whether `signature`, in the stored encoding for the key's scheme (64 raw bytes, or
    /// DER), verifies over `message`.
    pub fn verifies(&self, message: &[u8], signature: &[u8]) -> bool {
        match self.algorithm {
            SignatureAlgorithm::Ed25519 => {
                let (Ok(key), Ok(signature)) = (
                    <[u8; 32]>::try_from(self.public_key.as_slice()),
                    ed25519_dalek::Signature::from_slice(signature),
                ) else {
                    return false;
                };
                ed25519_dalek::VerifyingKey::from_bytes(&key).is_ok_and(|k| k.verify_strict(message, &signature).is_ok())
            }
            SignatureAlgorithm::Es256 => {
                use p256::ecdsa::signature::Verifier;
                let (Ok(key), Ok(signature)) = (
                    p256::ecdsa::VerifyingKey::from_sec1_bytes(&self.public_key),
                    p256::ecdsa::Signature::from_der(signature),
                ) else {
                    return false;
                };
                key.verify(message, &signature).is_ok()
            }
        }
    }
}

impl std::fmt::Display for SignerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tag = match self.algorithm {
            SignatureAlgorithm::Ed25519 => "ed25519",
            SignatureAlgorithm::Es256 => "es256",
        };
        write!(f, "{tag}:{}", hex::encode(&self.public_key))
    }
}

impl std::str::FromStr for SignerKey {
    type Err = String;

    /// Parse `ed25519:<hex>` or `es256:<hex>`, refusing bytes that name no key of the scheme.
    fn from_str(text: &str) -> Result<SignerKey, String> {
        let text = text.trim();
        let (tag, hex_key) = text.split_once(':').ok_or_else(|| format!("`{text}` is not `<scheme>:<hex>`"))?;
        let public_key = hex::decode(hex_key).map_err(|e| format!("`{text}`: {e}"))?;
        let algorithm = match tag {
            "ed25519" => {
                let key = <[u8; 32]>::try_from(public_key.as_slice())
                    .map_err(|_| format!("`{text}`: an Ed25519 key is 32 bytes, not {}", public_key.len()))?;
                ed25519_dalek::VerifyingKey::from_bytes(&key).map_err(|e| format!("`{text}`: {e}"))?;
                SignatureAlgorithm::Ed25519
            }
            "es256" => {
                p256::ecdsa::VerifyingKey::from_sec1_bytes(&public_key).map_err(|e| format!("`{text}`: no P-256 point: {e}"))?;
                SignatureAlgorithm::Es256
            }
            _ => return Err(format!("`{text}`: unknown key scheme `{tag}`")),
        };
        Ok(SignerKey { algorithm, public_key })
    }
}

/// Sign `message` through `port` and return the signature in the stored encoding: 64
/// bytes for Ed25519, low-S DER for ES256, converting an `es256-raw` answer at this edge
/// (`authority.issue.der-at-the-edge`). An answer that does not decode under the port's
/// tag, or does not verify under its public key, raises `SignatureEncodingInvalid`.
pub fn sign_through(port: &dyn SigningPort, message: &[u8]) -> Result<Vec<u8>, AuthorityError> {
    let encoding = port.encoding();
    let answer = port.sign(message)?;
    let invalid = |why: String| AuthorityError::SignatureEncodingInvalid(format!("the port tagged `{encoding}` {why}"));
    let stored = match encoding {
        SignatureEncoding::Ed25519 => answer,
        SignatureEncoding::Es256Der | SignatureEncoding::Es256Raw => {
            let decoded = if encoding == SignatureEncoding::Es256Der {
                p256::ecdsa::Signature::from_der(&answer)
            } else {
                p256::ecdsa::Signature::from_slice(&answer)
            };
            let signature = decoded.map_err(|e| invalid(format!("answered {} bytes that do not decode: {e}", answer.len())))?;
            signature.normalize_s().unwrap_or(signature).to_der().as_bytes().to_vec()
        }
    };
    if !SignerKey::of(port).verifies(message, &stored) {
        return Err(invalid("answered a signature its public key does not verify".into()));
    }
    Ok(stored)
}

/// Mint a checked plan as a credential whose authority block `signer` signs. The plan's
/// scheme must be the port's (`authority.issue.algorithm-mismatch`).
pub fn mint(plan: &MintPlan, claims: &MintClaims, signer: &dyn SigningPort) -> Result<String, AuthorityError> {
    let pinned = signer.algorithm();
    if plan.algorithm != pinned {
        return Err(AuthorityError::SignatureAlgorithmMismatch(format!(
            "the plan names {}; the signing key is {pinned}",
            plan.algorithm
        )));
    }
    let block = authority_block(plan, claims);
    let mut builder = BiscuitBuilder::new();
    for f in authority_facts(&block)? {
        builder = builder.fact(f).map_err(|e| AuthorityError::ProfileElementUnrecognized(e.to_string()))?;
    }
    sign_root(builder, signer)
}

/// Encode `builder` as a credential whose authority block `signer` signs. It checks
/// nothing about the block's facts; [`mint`] is the path that encodes a checked plan.
///
/// The library signs a root block only with a key pair it holds, so the credential is
/// built under a throwaway key, then the authority block's signature is replaced by the
/// port's over the same payload. The root key appears nowhere in the encoding, and the
/// block's next key is the library's own ephemeral one, so the result verifies under
/// the port's public key alone (`authority.issue.signing-port`).
pub fn sign_root(builder: BiscuitBuilder, signer: &dyn SigningPort) -> Result<String, AuthorityError> {
    let unsigned = |e: &dyn std::fmt::Display| AuthorityError::IssuerKeyUnresolvable(format!("the credential encodes to nothing: {e}"));
    let bytes = builder
        .build(&KeyPair::new_with_algorithm(Algorithm::Ed25519))
        .and_then(|token| token.to_vec())
        .map_err(|e| unsigned(&e))?;
    let mut proto = schema::Biscuit::decode(bytes.as_slice()).map_err(|e| unsigned(&e))?;
    let payload = authority_signature_payload(&proto.authority)?;
    proto.authority.signature = sign_through(signer, &payload)?;
    Ok(TOKEN_BASE64.encode(proto.encode_to_vec()))
}

/// The bytes the authority block's signature covers, per the block's signature version:
/// the library's layout, which a checkpoint recomputes to verify.
fn authority_signature_payload(block: &schema::SignedBlock) -> Result<Vec<u8>, AuthorityError> {
    let next_algorithm = block.next_key.algorithm.to_le_bytes();
    let next_key = &block.next_key.key;
    match block.version.unwrap_or_default() {
        0 => Ok([block.block.as_slice(), &next_algorithm, next_key].concat()),
        1 => Ok([
            b"\0BLOCK\0\0VERSION\0".as_slice(),
            &1u32.to_le_bytes(),
            b"\0PAYLOAD\0",
            &block.block,
            b"\0ALGORITHM\0",
            &next_algorithm,
            b"\0NEXTKEY\0",
            next_key,
        ]
        .concat()),
        v => Err(AuthorityError::IssuerKeyUnresolvable(format!("the library signs an authority block under unknown version {v}"))),
    }
}
