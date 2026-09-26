//! `authority.refuse`: the refusal payload on the wire.

use contextful_core::enforce::EnforceError;
use contextful_core::read::{ReadError, Refusal};
use contextful_policy::enforce::refuse::{payload, REFUSAL_PAYLOAD};
use serde_json::json;

/// A refusal payload carries at most 512 B.
// spec: authority.refuse.payload@fd750417
#[test]
fn a_refusal_payload_carries_at_most_512_bytes() {
    assert_eq!(REFUSAL_PAYLOAD, 512);
    let long = Refusal::from(EnforceError::UnknownRelation(format!("`{}`", "é".repeat(2000))));
    let p = payload(&long);
    assert!(p.to_string().len() <= 512, "{}", p.to_string().len());
    assert_eq!(p["error"]["code"], json!("unknown_relation"));
    assert_eq!(p["error"]["http"], json!(404));
    let short = Refusal::from(EnforceError::ScopeDenied("table `notes`: granted scope `acme`, requested scope `globex`".into()));
    let p = payload(&short);
    assert_eq!(p["error"]["code"], json!("scope_denied"));
    assert_eq!(p["error"]["http"], json!(403));
    assert_eq!(p["error"]["identifier"], json!("EnforceScopeDenied"));
    assert!(p["error"]["message"].as_str().unwrap().ends_with("requested scope `globex`"));
    let read = payload(&Refusal::from(ReadError::StatementNotReadOnly("DELETE".into())));
    assert_eq!(read["error"]["code"], json!("statement_not_read_only"));
}
