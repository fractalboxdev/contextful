//! AWS Signature Version 4 in the `Authorization` header, for a JSON request to an AWS
//! service API, behind the `aws-secrets-manager` feature. It performs no I/O.

use contextful_core::time::Instant;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

/// The key material one request signs with.
pub struct SigningKey<'a> {
    pub access_key_id: &'a str,
    pub secret_access_key: &'a str,
    /// The session token of a temporary credential, sent and signed as `x-amz-security-token`.
    pub session_token: Option<&'a str>,
}

/// The `x-amz-date` spelling of `at`: `YYYYMMDDTHHMMSSZ`.
pub fn amz_date(at: Instant) -> String {
    let text = at.to_rfc3339_nanos();
    format!("{}Z", text[..19].replace(['-', ':'], ""))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes a key of any length");
    mac.update(data.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// The headers that sign one request: `x-amz-date`, `x-amz-security-token` for a temporary
/// credential, and `authorization`. `headers` holds every other header sent, `host` among
/// them; each is signed. `path` is the URI path, already percent-encoded, and `query` the
/// canonical query string, empty for none.
#[allow(clippy::too_many_arguments)]
pub fn sign(
    method: &str,
    path: &str,
    query: &str,
    headers: &[(&str, &str)],
    payload: &[u8],
    key: &SigningKey<'_>,
    region: &str,
    service: &str,
    at: Instant,
) -> Vec<(String, String)> {
    let date_time = amz_date(at);
    let date = &date_time[..8];
    let mut added = vec![("x-amz-date".to_string(), date_time.clone())];
    if let Some(token) = key.session_token {
        added.push(("x-amz-security-token".to_string(), token.to_string()));
    }
    let mut canonical: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.split_whitespace().collect::<Vec<_>>().join(" ")))
        .chain(added.iter().cloned())
        .collect();
    canonical.sort();
    let signed = canonical.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
    let header_block: String = canonical.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let request = format!("{method}\n{path}\n{query}\n{header_block}\n{signed}\n{}", hex(&Sha256::digest(payload)));
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let to_sign = format!("AWS4-HMAC-SHA256\n{date_time}\n{scope}\n{}", hex(&Sha256::digest(request.as_bytes())));
    let mut k = hmac(format!("AWS4{}", key.secret_access_key).as_bytes(), date);
    for part in [region, service, "aws4_request"] {
        k = hmac(&k, part);
    }
    let signature = hex(&hmac(&k, &to_sign));
    added.push(("authorization".to_string(), format!("AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed}, Signature={signature}", key.access_key_id)));
    added
}
