//! `authority.refuse`: the payload a refusal carries on the wire.

use contextful_core::enforce::EnforceError;
use contextful_core::read::{ReadError, Refusal};
use serde_json::{json, Value};

/// Bytes one refusal payload carries (`authority.refuse.payload`).
pub const REFUSAL_PAYLOAD: usize = 512;

/// The wire code and HTTP status of a refusal.
fn code(r: &Refusal) -> (String, u16) {
    match r {
        Refusal::Enforce(EnforceError::ScopeDenied(_)) => ("scope_denied".into(), 403),
        Refusal::Enforce(EnforceError::UnknownRelation(_)) => ("unknown_relation".into(), 404),
        Refusal::Read(ReadError::HttpCredentialMissing(_)) => ("http_credential_missing".into(), 401),
        other => {
            let id = other.identifier();
            let mut snake = String::new();
            for (i, ch) in id.chars().enumerate() {
                if ch.is_ascii_uppercase() && i > 0 {
                    snake.push('_');
                }
                snake.push(ch.to_ascii_lowercase());
            }
            (snake, 400)
        }
    }
}

/// A refusal as its wire payload: code, HTTP status, identifier and message, at most
/// 512 B serialized. A longer message is cut at a character boundary.
pub fn payload(r: &Refusal) -> Value {
    let (code, http) = code(r);
    let mut message = r.to_string();
    loop {
        let v = json!({ "error": { "code": code, "http": http, "identifier": r.identifier(), "message": message } });
        if v.to_string().len() <= REFUSAL_PAYLOAD || message.is_empty() {
            return v;
        }
        let over = v.to_string().len() - REFUSAL_PAYLOAD;
        let keep = message.len().saturating_sub(over.max(1));
        let mut cut = keep;
        while !message.is_char_boundary(cut) {
            cut -= 1;
        }
        message.truncate(cut);
    }
}
