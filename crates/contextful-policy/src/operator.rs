//! The console's operator attestation (`surface.apply.operator-attestation`): an
//! HMAC-SHA256 under the console secret over the request's method, target and body digest,
//! the operator, the signing second and a nonce. The served Admin routes and the control
//! profile verify it here; replay is the caller's nonce claim.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

/// The request header naming the operator.
pub const OPERATOR_HEADER: &str = "X-Contextful-Operator";
/// The request header carrying the signing second.
pub const TIME_HEADER: &str = "X-Contextful-Operator-Time";
/// The request header carrying the nonce.
pub const NONCE_HEADER: &str = "X-Contextful-Operator-Nonce";
/// The request header carrying the signature.
pub const SIGNATURE_HEADER: &str = "X-Contextful-Operator-Signature";

/// Seconds a signature stays fresh on either side of the verifier's clock.
pub const FRESH_SECONDS: u64 = 60;

/// The signed parts of one request, as its headers carry them.
#[derive(Debug, Clone, Copy)]
pub struct Signed<'a> {
    pub method: &'a str,
    pub target: &'a str,
    pub body: &'a [u8],
    pub operator: &'a str,
    pub time: &'a str,
    pub nonce: &'a str,
    pub signature: &'a str,
}

/// The operator an attestation verified, with the nonce the caller claims against replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operator {
    pub subject: String,
    pub nonce: String,
    pub signed_at: i64,
}

fn message(method: &str, target: &str, body: &[u8], subject: &str, at: i64, nonce: &str) -> String {
    let digest = format!("{:x}", Sha256::digest(body));
    format!("{method}\n{target}\n{digest}\n{subject}\n{at}\n{nonce}")
}

/// Verify one attestation at `now`; `None` for a malformed, stale or forged one.
pub fn verify(secret: &str, signed: &Signed<'_>, now: i64) -> Option<Operator> {
    let at: i64 = signed.time.parse().ok()?;
    let (subject, nonce, signature) = (signed.operator, signed.nonce, signed.signature);
    if subject.is_empty() || subject.len() > 256 || nonce.len() != 32 || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) || signature.len() != 64 {
        return None;
    }
    if now.abs_diff(at) > FRESH_SECONDS {
        return None;
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(message(signed.method, signed.target, signed.body, subject, at, nonce).as_bytes());
    let mut signature_bytes = [0u8; 32];
    for (index, chunk) in signature.as_bytes().chunks_exact(2).enumerate() {
        signature_bytes[index] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    mac.verify_slice(&signature_bytes).ok()?;
    Some(Operator { subject: subject.to_owned(), nonce: nonce.to_owned(), signed_at: at })
}
