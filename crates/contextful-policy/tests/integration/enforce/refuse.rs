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

/// A stale source answers HTTP 503 on the wire, and a closure past its bound the
/// non-retryable 422.
#[test]
fn a_stale_source_answers_503_and_an_overflowing_closure_422_on_the_wire() {
    use contextful_core::disclosure::VisibilityError;
    let stale = payload(&Refusal::from(VisibilityError::AccessStale {
        table: "wiki/pages".into(),
        origin: "wiki".into(),
        reason: "lags past its budget",
        lag: "901s".into(),
        budget: 900,
        last_observed: "none".into(),
    }));
    assert_eq!(stale["error"]["http"], json!(503));
    assert_eq!(stale["error"]["code"], json!("visibility_access_stale"));
    assert_eq!(stale["error"]["identifier"], json!("VisibilityAccessStale"));
    let deep = payload(&Refusal::from(VisibilityError::ClosureDepth { origin: "wiki".into(), subject: "user://dana@acme.example".into(), bound: "8 hops".into() }));
    assert_eq!(deep["error"]["http"], json!(422));
    assert_eq!(deep["error"]["code"], json!("visibility_closure_depth"));
    // A visibility refusal the contract fixes no status for answers 400.
    let other = payload(&Refusal::from(VisibilityError::UnboundTable { table: "wiki/notes".into() }));
    assert_eq!(other["error"]["http"], json!(400));
}
