//! `read.synthesize`: the output schema and the attempt budget.

use contextful_core::memory::synthesize::{feedback, judge, template_hash, validate, Attempt, EXTRACT_ATTEMPTS};
use serde_json::json;

fn valid() -> String {
    json!({ "claims": [{ "subject": "acme", "predicate": "cfo", "object": "Dana", "confidence": 0.9,
        "evidence": [{ "table": "research/notes", "run": "run-0001", "seq": 0 }] }] })
    .to_string()
}

/// A schema-invalid response is re-prompted with its validation feedback, at most 3 attempts per batch in total.
// spec: read.synthesize.extract-attempts@6dfacc5d
#[test]
fn an_invalid_response_is_retried_with_feedback_three_attempts_in_all() {
    assert_eq!(EXTRACT_ATTEMPTS, 3);
    let feedback_for = |a: Attempt| match a {
        Attempt::Retry(why) => why,
        other => panic!("{other:?}"),
    };
    let why = feedback_for(judge(1, "the CFO is Dana"));
    assert!(why.contains("not JSON"), "{why}");
    assert!(feedback(&why).content.contains("did not validate") && feedback(&why).content.contains(&why));
    let missing = feedback_for(judge(2, r#"{"claims": [{"subject": "acme"}]}"#));
    assert!(missing.contains("output schema"), "{missing}");
    assert!(matches!(judge(3, "still prose"), Attempt::Exhausted(_)));
    assert!(matches!(judge(3, &valid()), Attempt::Accept(_)));
    let out_of_range = valid().replace("0.9", "1.5");
    assert!(validate(&out_of_range).unwrap_err().contains("confidence"));
    assert_eq!(validate(&format!("```json\n{}\n```", valid())).unwrap().claims[0].object, "Dana");
    assert!(template_hash("prompt").starts_with("sha256:") && template_hash("prompt").len() == 71);
}

#[test]
fn one_claim_validation_serves_every_write_path() {
    use contextful_core::memory::synthesize::{validate_claim, CandidateClaim};
    let c = |subject: &str, confidence: f64, seq: i64| CandidateClaim {
        subject: subject.into(),
        predicate: "cfo".into(),
        object: "Dana".into(),
        scope: None,
        confidence,
        evidence: vec![contextful_core::memory::synthesize::EvidenceRef::row("t", "r", seq)],
    };
    assert_eq!(validate_claim(&c("acme", 0.5, 0)), Ok(()));
    assert!(validate_claim(&c(" ", 0.5, 0)).unwrap_err().contains("subject"));
    assert!(validate_claim(&c("acme", 1.01, 0)).unwrap_err().contains("confidence"));
    assert!(validate_claim(&c("acme", 0.5, -1)).unwrap_err().contains("seq"));
}
