use contextful_eval::entity::{self, Answer, Claim, Expected};
use std::collections::BTreeSet;

fn truth() -> Expected {
    Expected {
        entity: Some("north".into()),
        claims: vec!["edge-a".into(), "edge-b".into()],
        path: vec!["edge-a".into(), "edge-b".into()],
        citations: vec!["notes#run:0".into(), "notes#run:1".into()],
        abstain: false,
        facts: answer().claims,
    }
}

fn answer() -> Answer {
    Answer {
        entity: Some("north".into()),
        claims: vec![
            Claim {
                id: "edge-a".into(),
                subject: "north".into(),
                predicate: "supplier".into(),
                object: "bridge".into(),
                citations: vec!["notes#run:0".into()],
            },
            Claim {
                id: "edge-b".into(),
                subject: "bridge".into(),
                predicate: "owner".into(),
                object: "south".into(),
                citations: vec!["notes#run:1".into()],
            },
        ],
        reason: None,
    }
}

#[test]
fn independent_grades_reject_wrong_entities_claims_paths_and_citations() {
    let truth = truth();
    let eligible = BTreeSet::from(["notes#run:0".into(), "notes#run:1".into()]);
    let good = answer();
    assert_eq!(
        entity::grade(&truth, &good, &eligible).path_correctness,
        1.0
    );
    let mut bad = good.clone();
    bad.entity = Some("north-labs".into());
    assert_eq!(entity::grade(&truth, &bad, &eligible).entity_accuracy, 0.0);
    bad = good.clone();
    bad.claims.reverse();
    assert_eq!(entity::grade(&truth, &bad, &eligible).path_correctness, 0.0);
    bad = good.clone();
    bad.claims[0].id = "later".into();
    assert_eq!(
        entity::grade(&truth, &bad, &eligible).eligible_claim_recall,
        Some(0.5)
    );
    bad = good.clone();
    bad.claims[0].citations = vec!["private#run:0".into()];
    assert_eq!(
        entity::grade(&truth, &bad, &eligible).citation_resolution,
        Some(0.5)
    );
    assert_eq!(entity::grade(&truth, &bad, &eligible).path_correctness, 0.0);
}

#[test]
fn contradiction_and_missing_steps_abstain_without_partial_answers() {
    let claims = answer().claims;
    assert_eq!(
        entity::follow(
            Some("north".into()),
            &["supplier".into(), "owner".into()],
            &claims
        )
        .claims
        .len(),
        2
    );
    let mut conflict = claims.clone();
    conflict.push(Claim {
        id: "contradiction".into(),
        object: "other".into(),
        ..claims[0].clone()
    });
    let ambiguous = entity::follow(Some("north".into()), &["supplier".into()], &conflict);
    assert_eq!(ambiguous.reason.as_deref(), Some("contradiction"));
    assert!(ambiguous.claims.is_empty());
    let missing = entity::follow(Some("north".into()), &["missing".into()], &claims);
    assert_eq!(missing.reason.as_deref(), Some("missing_evidence"));
    assert!(missing.claims.is_empty());
}

#[test]
fn ineligible_or_unknown_claims_never_satisfy_adjudicated_truth() {
    let truth = truth();
    let eligible = BTreeSet::from(["notes#run:0".into(), "notes#run:1".into()]);
    for id in ["later", "expired", "superseded", "inaccessible", "invented"] {
        let mut bad = answer();
        bad.claims[0].id = id.into();
        bad.claims[0].citations = vec![format!("excluded#{id}:0")];
        let score = entity::grade(&truth, &bad, &eligible);
        assert_eq!(score.eligible_claim_recall, Some(0.5));
        assert_eq!(score.path_correctness, 0.0);
    }
}

#[test]
fn entity_metric_paths_resolve_through_existing_baseline_parser() {
    for metric in entity::METRICS {
        for arm in ["structured", "ordinary"] {
            let path = format!("entity_evidence.{arm}.{metric}");
            assert!(
                contextful_eval::baseline::MetricPath::parse(&path).is_ok(),
                "{path}"
            );
        }
    }
}

#[test]
fn malformed_empty_answers_and_wrong_path_roots_fail() {
    let eligible = BTreeSet::from(["notes#run:0".into(), "notes#run:1".into()]);
    let malformed = Answer {
        entity: Some("north".into()),
        claims: vec![],
        reason: None,
    };
    assert_eq!(
        entity::grade(&truth(), &malformed, &eligible).appropriate_abstention,
        0.0
    );
    let mut wrong_root = answer();
    wrong_root.entity = Some("other".into());
    assert_eq!(
        entity::grade(&truth(), &wrong_root, &eligible).path_correctness,
        0.0
    );
}

#[test]
fn existing_baseline_comparisons_reject_an_entity_metric_regression() {
    use contextful_eval::baseline::{self, Baselines, Outcome};
    let baseline = Baselines::parse(r#"{"_run":{"k":16,"tier":"deterministic","model":null,"samples":1},"entity_evidence.structured.path_correctness":{"value":1.0,"band":0.0}}"#).unwrap();
    let report = serde_json::json!({"run":{"k":16,"tier":"deterministic","model":null,"samples":1},"entity_evidence":{"structured":{"path_correctness":{"mean":0.9,"n":72}}}});
    let comparisons = baseline::comparisons(&report, &baseline).unwrap();
    assert_eq!(comparisons.len(), 1);
    assert_eq!(comparisons[0].outcome, Outcome::Regressed);
}

#[test]
fn canonical_claim_ids_cannot_hide_forged_relationship_tuples() {
    let eligible = BTreeSet::from(["notes#run:0".into(), "notes#run:1".into()]);
    let mut forged = answer();
    forged.claims[0].predicate = "owner".into();
    let score = entity::grade(&truth(), &forged, &eligible);
    assert_eq!(score.eligible_claim_recall, Some(0.5));
    assert_eq!(score.path_correctness, 0.0);
}

#[test]
fn citation_order_never_changes_a_claim_or_path_grade() {
    let fact = Claim {
        id: "edge-a".into(),
        subject: "north".into(),
        predicate: "supplier".into(),
        object: "bridge".into(),
        citations: vec!["notes#run:0".into(), "notes#run:1".into()],
    };
    let truth = Expected {
        entity: Some("north".into()),
        claims: vec!["edge-a".into()],
        path: vec!["edge-a".into()],
        citations: fact.citations.clone(),
        abstain: false,
        facts: vec![fact.clone()],
    };
    let eligible = BTreeSet::from(["notes#run:0".into(), "notes#run:1".into()]);
    let answer = |citations: Vec<&str>| Answer {
        entity: Some("north".into()),
        claims: vec![Claim {
            citations: citations.into_iter().map(String::from).collect(),
            ..fact.clone()
        }],
        reason: None,
    };
    let ordered = entity::grade(&truth, &answer(vec!["notes#run:0", "notes#run:1"]), &eligible);
    let reordered = entity::grade(&truth, &answer(vec!["notes#run:1", "notes#run:0"]), &eligible);
    for scores in [&ordered, &reordered] {
        assert_eq!(scores.eligible_claim_recall, Some(1.0));
        assert_eq!(scores.path_correctness, 1.0);
        assert_eq!(scores.citation_resolution, Some(1.0));
    }
    let subset = entity::grade(&truth, &answer(vec!["notes#run:1"]), &eligible);
    assert_eq!(subset.eligible_claim_recall, Some(0.0));
    assert_eq!(subset.path_correctness, 0.0);
    assert_eq!(subset.citation_resolution, Some(1.0));
    let mut forged = answer(vec!["notes#run:1", "notes#run:0"]);
    forged.claims[0].object = "other".into();
    let forged = entity::grade(&truth, &forged, &eligible);
    assert_eq!(forged.eligible_claim_recall, Some(0.0));
    assert_eq!(forged.path_correctness, 0.0);
}
