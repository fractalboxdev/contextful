//! The signatures a loopback delegate checks and returns (`surface.delegate.signed`):
//! HMAC-SHA256 under the deployment's worker key, as lowercase hex.

use crate::http::{Request, Response};
use sha2::{Digest, Sha256};

pub const TIMESTAMP_HEADER: &str = "x-contextful-timestamp";
pub const SIGNATURE_HEADER: &str = "x-contextful-signature";

/// HMAC-SHA256 over `message` under `key`, as lowercase hex.
pub fn hmac_hex(key: &[u8], message: &[u8]) -> String {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let inner: Vec<u8> = k.iter().map(|b| b ^ 0x36).chain(message.iter().copied()).collect();
    let outer: Vec<u8> = k.iter().map(|b| b ^ 0x5c).chain(Sha256::digest(&inner).iter().copied()).collect();
    Sha256::digest(&outer).iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether `request` carries the signature under `key` over `call\n<timestamp>\n` and its body.
pub fn request_signed(key: &[u8], request: &Request) -> bool {
    let Some(timestamp) = request.header(TIMESTAMP_HEADER) else {
        return false;
    };
    let expected = hmac_hex(key, &[format!("call\n{timestamp}\n").as_bytes(), &request.body].concat());
    request.header(SIGNATURE_HEADER) == Some(expected.as_str())
}

/// `body` as a `200` answer to the request carrying `idempotency_key`, signed under `key`
/// over `answer\n<idempotency_key>\n<timestamp>\n` and the body.
pub fn signed_answer(key: &[u8], idempotency_key: &str, body: &str) -> Response {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
        .to_string();
    let signature = hmac_hex(key, format!("answer\n{idempotency_key}\n{timestamp}\n{body}").as_bytes());
    let mut response = Response::json(200, body);
    response.headers.push((TIMESTAMP_HEADER.into(), timestamp));
    response.headers.push((SIGNATURE_HEADER.into(), signature));
    response
}
