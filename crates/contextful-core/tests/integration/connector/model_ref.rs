//! `connector.infer.cutoffs`: a model reference's snapshot and its two tagged cutoffs.

use contextful_core::connector::model_ref::{Cutoff, CutoffTag, ModelRef};
use contextful_core::time::Instant;

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

/// A model reference carries a pinned snapshot id and pre-training and post-training cutoffs, each tagged `documented` or `estimated`. An undeclared cutoff compares as the call's own instant.
// spec: connector.infer.cutoffs@d0583665
#[test]
fn a_model_reference_carries_tagged_cutoffs_and_an_undeclared_one_is_the_call_instant() {
    let c = Cutoff::parse("2024-10:documented").unwrap();
    assert_eq!(c, Cutoff { year: 2024, month: 10, tag: CutoffTag::Documented });
    assert_eq!(c.to_string(), "2024-10:documented");
    // A month cutoff covers the whole month: it ends at the start of the next one.
    assert_eq!(c.end(), at("2024-11-01T00:00:00Z"));
    assert_eq!(Cutoff::parse("2024-12:estimated").unwrap().end(), at("2025-01-01T00:00:00Z"));
    for bad in ["2024-10", "2024-13:documented", "2024-10:guessed", "24-10:documented", "2024-1:documented"] {
        assert!(Cutoff::parse(bad).is_err(), "{bad}");
    }

    let reader = ModelRef { snapshot: "open-instruct-7b@2025-01-15".into(), pretrain_cutoff: Some(c), posttrain_cutoff: None };
    let call = at("2030-06-01T00:00:00Z");
    assert_eq!(reader.pretrain_end(call), at("2024-11-01T00:00:00Z"));
    assert_eq!(reader.posttrain_end(call), call, "an undeclared cutoff compares as the call's own instant");

    let json = serde_json::to_value(&reader).unwrap();
    assert_eq!(json, serde_json::json!({ "snapshot": "open-instruct-7b@2025-01-15", "pretrain_cutoff": "2024-10:documented" }));
    assert_eq!(serde_json::from_value::<ModelRef>(json).unwrap(), reader);
    assert!(serde_json::from_value::<ModelRef>(serde_json::json!({ "snapshot": "m", "posttrain_cutoff": "2024-10" })).is_err());
}
