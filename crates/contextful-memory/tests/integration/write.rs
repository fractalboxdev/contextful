//! The direct write.

use super::support::*;
use contextful_core::memory::revise::Tier;
use contextful_core::memory::synthesize::{CandidateClaim, EvidenceRef};
use contextful_core::memory::MemoryError;
use contextful_core::store::lay_out::NodeId;
use contextful_memory::write::write_claim;
use contextful_memory::MemoryFault;

fn candidate(object: &str) -> CandidateClaim {
    CandidateClaim {
        subject: "acme".into(),
        predicate: "cfo".into(),
        object: object.into(),
        scope: None,
        confidence: 1.0,
        evidence: vec![EvidenceRef::row("research/notes", "run-0001", 0)],
    }
}

#[test]
fn a_direct_write_lands_a_curated_claim_and_retires_a_derived_prior() {
    let f = Fixture::new();
    let writer = f.writer();
    let node = NodeId::parse("memory-a").unwrap();
    let first = write_claim(&f.face, &writer, "memory/facts", candidate("Dana"), &node, at("2030-01-11T00:00:00Z"), &super::synthesize::admit).unwrap();
    assert_eq!(first.claim.unwrap().tier, Tier::Curated);
    let second = write_claim(&f.face, &writer, "memory/facts", candidate("Lee"), &node, at("2030-01-12T00:00:00Z"), &super::synthesize::admit).unwrap();
    assert_eq!(second.retired.len(), 1);
    assert_eq!(second.retired[0].object, "Dana");
    match write_claim(&f.face, &writer, "memory/episodes", candidate("Lee"), &node, at("2030-01-12T00:00:00Z"), &super::synthesize::admit) {
        Err(MemoryFault::Memory(MemoryError::DirectWriteShapeRefused(why))) => assert!(why.contains("memory_episodes"), "{why}"),
        other => panic!("{other:?}"),
    }
}

/// Regression: a memory table declared again as a pipeline table refuses the manifest
/// rather than dropping the pipeline declaration's policy.
#[test]
fn a_table_declared_twice_refuses_the_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let store = contextful_context::Store::open(dir.path(), "research").unwrap();
    let manifest = format!(
        "{MANIFEST}\n[[pipeline.tables]]\nname = \"memory/facts\"\n[pipeline.tables.policy.columns]\nobject = {{ strategy = \"drop\" }}\n"
    );
    let err = contextful_context::read::Face::open(store, &manifest, contextful_policy::enforce::mask::Pepper::resolve(|_| None)).err().unwrap();
    assert!(err.to_string().contains("declared more than once"), "{err}");
}

/// Regression: a direct write passes the validation a synthesized claim does.
#[test]
fn a_direct_write_validates_its_claim() {
    let f = Fixture::new();
    let writer = f.writer();
    let node = NodeId::parse("memory-a").unwrap();
    for bad in [CandidateClaim { confidence: 1.5, ..candidate("Dana") }, CandidateClaim { subject: " ".into(), ..candidate("Dana") }] {
        let r = write_claim(&f.face, &writer, "memory/facts", bad, &node, at("2030-01-11T00:00:00Z"), &super::synthesize::admit);
        assert!(matches!(r, Err(MemoryFault::Invalid(_))), "{r:?}");
    }
}

/// Regression: a curated write of a fact synthesis already holds promotes the claim.
#[test]
fn a_curated_restatement_promotes_the_claim() {
    use contextful_core::memory::revise::{claim_id, Claim};
    use contextful_memory::claims::{Landing, Writer};
    let f = Fixture::new();
    let writer = f.writer();
    let node = NodeId::parse("memory-a").unwrap();
    let w = Writer::of(&writer);
    let derived = Claim {
        claim_id: claim_id("acme", "cfo", "Dana", None),
        subject: "acme".into(),
        predicate: "cfo".into(),
        object: "Dana".into(),
        scope: None,
        tier: Tier::Derived,
        confidence: 0.6,
        valid_from: at("2030-01-10T00:00:00Z"),
        valid_to: None,
        evidence: Vec::new(),
        superseded_by: None,
        grant_id: w.grant_id.clone(),
        agent: w.agent.clone(),
    };
    Landing { node: &node, at: at("2030-01-10T00:00:00Z"), writer: &w, run_id: "memory-derived".into(), boundary: &super::synthesize::admit, taint: None }
        .commit(&f.face, "memory/facts", &[derived], &[])
        .unwrap();
    let promoted = write_claim(&f.face, &writer, "memory/facts", candidate("Dana"), &node, at("2030-01-11T00:00:00Z"), &super::synthesize::admit).unwrap();
    assert_eq!(promoted.claim.map(|c| c.tier), Some(Tier::Curated));
}

fn keyed(at_: &str, key: Option<&str>) -> contextful_memory::write::Observation {
    contextful_memory::write::Observation { observed_at: Some(at(at_)), dedup_key: key.map(str::to_string) }
}

/// `contextful memory write --observed-at <instant>` lands the claim with `valid_from` at that instant; without it, `valid_from` is the write's own instant.
// spec: read.revise.observed-at@c39aeee4
#[test]
fn an_observed_write_lands_valid_from_its_observed_instant() {
    use contextful_memory::write::{write_observed, Observation};
    let f = Fixture::new();
    let writer = f.writer();
    let node = NodeId::parse("memory-a").unwrap();
    let observed = write_observed(&f.face, &writer, "memory/facts", candidate("Dana"), &keyed("2029-06-01T00:00:00Z", None), &node, at("2030-01-11T00:00:00Z"), &super::synthesize::admit).unwrap();
    assert_eq!(observed.claim.unwrap().valid_from, at("2029-06-01T00:00:00Z"));
    let unobserved = CandidateClaim { subject: "globex".into(), ..candidate("Kim") };
    let now = write_observed(&f.face, &writer, "memory/facts", unobserved, &Observation::default(), &node, at("2030-01-12T00:00:00Z"), &super::synthesize::admit).unwrap();
    assert_eq!(now.claim.unwrap().valid_from, at("2030-01-12T00:00:00Z"));
}

/// `--dedup-key <key>` derives `claim_id` from the key beside the subject, predicate, object and scope, and a write whose `claim_id` the table already holds lands nothing.
// spec: read.revise.dedup-key@8c1ffcb6
#[test]
fn a_dedup_key_seeds_the_claim_id_and_a_retry_lands_nothing() {
    use contextful_core::memory::revise::claim_id;
    use contextful_memory::write::write_observed;
    let f = Fixture::new();
    let writer = f.writer();
    let node = NodeId::parse("memory-a").unwrap();
    let write = |object: &str, when: &str, key: &str, now: &str| {
        write_observed(&f.face, &writer, "memory/facts", candidate(object), &keyed(when, Some(key)), &node, at(now), &super::synthesize::admit).unwrap()
    };
    let first = write("Dana", "2030-01-01T00:00:00Z", "evt-1", "2030-01-11T00:00:00Z").claim.unwrap();
    assert_ne!(first.claim_id, claim_id("acme", "cfo", "Dana", None));
    // The same observation retried lands nothing, even after a successor retired it.
    let lee = write("Lee", "2030-02-01T00:00:00Z", "evt-2", "2030-01-12T00:00:00Z");
    assert_eq!(lee.retired.len(), 1);
    let retried = write("Dana", "2030-01-01T00:00:00Z", "evt-1", "2030-01-13T00:00:00Z");
    assert_eq!((retried.claim, retried.retired.len()), (None, 0));
    // Dana observed again under a new key keeps both intervals.
    let again = write("Dana", "2030-03-01T00:00:00Z", "evt-3", "2030-01-14T00:00:00Z").claim.unwrap();
    assert_ne!(again.claim_id, first.claim_id);
    let s = f.face.session(&writer, &contextful_policy::enforce::session::Request::default(), Default::default()).unwrap();
    let rows = contextful_memory::claims::read_claims(&f.face, &s, "memory/facts").unwrap();
    let mut spans: Vec<(String, Option<String>)> =
        rows.iter().map(|c| (c.object.clone(), c.valid_to.map(|t| t.to_rfc3339()))).collect();
    spans.sort();
    assert_eq!(
        spans,
        [("Dana".to_string(), None), ("Dana".to_string(), Some("2030-02-01T00:00:00Z".to_string())), ("Lee".to_string(), Some("2030-03-01T00:00:00Z".to_string()))]
    );
}

/// A direct write whose `valid_from` precedes the `valid_from` of an unsuperseded claim of its subject, predicate and scope with another object raises `MemoryObservationOutOfOrder`, and nothing lands.
// spec: read.revise.observed-order@abfadc9e
#[test]
fn an_observation_before_a_live_contradicting_prior_refuses() {
    use contextful_memory::write::write_observed;
    let f = Fixture::new();
    let writer = f.writer();
    let node = NodeId::parse("memory-a").unwrap();
    let write = |object: &str, when: &str, key: &str, now: &str| {
        write_observed(&f.face, &writer, "memory/facts", candidate(object), &keyed(when, Some(key)), &node, at(now), &super::synthesize::admit)
    };
    write("Lee", "2030-03-01T00:00:00Z", "evt-2", "2030-01-11T00:00:00Z").unwrap();
    match write("Dana", "2030-01-01T00:00:00Z", "evt-1", "2030-01-12T00:00:00Z") {
        Err(MemoryFault::Memory(MemoryError::ObservationOutOfOrder(why))) => assert!(why.contains("2030-03-01"), "{why}"),
        other => panic!("{other:?}"),
    }
    let s = f.face.session(&writer, &contextful_policy::enforce::session::Request::default(), Default::default()).unwrap();
    let rows = contextful_memory::claims::read_claims(&f.face, &s, "memory/facts").unwrap();
    assert!(rows.iter().all(|c| c.valid_to.is_none_or(|end| end >= c.valid_from)), "{rows:?}");
    assert!(rows.iter().any(|c| c.object == "Lee" && c.valid_to.is_none() && c.superseded_by.is_none()), "{rows:?}");
    assert!(rows.iter().all(|c| c.object != "Dana"), "{rows:?}");
    // Kim retires Lee and becomes the live prior a later observation must follow.
    write("Kim", "2030-04-01T00:00:00Z", "evt-3", "2030-01-13T00:00:00Z").unwrap();
    assert!(write("Dana", "2030-03-15T00:00:00Z", "evt-4", "2030-01-14T00:00:00Z").is_err());
    assert!(write("Dana", "2030-05-01T00:00:00Z", "evt-5", "2030-01-15T00:00:00Z").is_ok());
}

/// Equal-instant, unscoped direct contradictions keep both claims live and record their ids.
// spec: read.revise.unscoped-collision@e2109b5f
#[test]
fn unscoped_writers_at_one_instant_record_a_conflict_without_retirement() {
    use contextful_context::read::ReadOptions;
    use contextful_memory::write::write_observed;
    use contextful_policy::enforce::session::Request;

    let f = Fixture::new();
    let writer = f.writer();
    let node = NodeId::parse("memory-a").unwrap();
    let first = write_observed(
        &f.face, &writer, "memory/facts", candidate("Dana"),
        &keyed("2030-01-01T00:00:00Z", Some("writer-a")), &node,
        at("2030-01-11T00:00:00Z"), &super::synthesize::admit,
    ).unwrap().claim.unwrap();
    let second = write_observed(
        &f.face, &writer, "memory/facts", candidate("Lee"),
        &keyed("2030-01-01T00:00:00Z", Some("writer-b")), &node,
        at("2030-01-12T00:00:00Z"), &super::synthesize::admit,
    ).unwrap();
    assert!(second.retired.is_empty());
    let second = second.claim.unwrap();
    let session = f.face.session(&writer, &Request::default(), Default::default()).unwrap();
    let claims = contextful_memory::claims::read_claims(&f.face, &session, "memory/facts").unwrap();
    assert!(claims.iter().any(|c| c.claim_id == first.claim_id && c.valid_to.is_none() && c.superseded_by.is_none()));
    assert!(claims.iter().any(|c| c.claim_id == second.claim_id && c.valid_to.is_none() && c.superseded_by.is_none()));
    let letters = f.face.query(
        &session, r#"SELECT reason, detail FROM "memory/facts_dead_letter""#, ReadOptions::default(),
    ).unwrap();
    assert_eq!(letters.rows.len(), 1);
    assert_eq!(letters.rows[0][0], serde_json::json!("unscoped-collision"));
    let detail = letters.rows[0][1].as_str().unwrap();
    assert!(detail.contains(&first.claim_id) && detail.contains(&second.claim_id), "{detail}");

    let resolved = write_observed(
        &f.face, &writer, "memory/facts", candidate("Kim"),
        &keyed("2030-02-01T00:00:00Z", Some("writer-c")), &node,
        at("2030-01-13T00:00:00Z"), &super::synthesize::admit,
    ).unwrap();
    assert_eq!(resolved.retired.len(), 2, "a later explicit write resolves both live claims");
    assert!(resolved.retired.iter().any(|c| c.claim_id == first.claim_id));
    assert!(resolved.retired.iter().any(|c| c.claim_id == second.claim_id));
}

#[test]
fn reaffirming_one_conflicting_object_retires_the_other() {
    use contextful_memory::write::write_observed;

    let f = Fixture::new();
    let writer = f.writer();
    let node = NodeId::parse("memory-a").unwrap();
    let write = |object, observed, key, committed| {
        write_observed(
            &f.face, &writer, "memory/facts", candidate(object),
            &keyed(observed, Some(key)), &node, at(committed), &super::synthesize::admit,
        ).unwrap()
    };
    let first = write("Dana", "2030-01-01T00:00:00Z", "writer-a", "2030-01-11T00:00:00Z").claim.unwrap();
    let second = write("Lee", "2030-01-01T00:00:00Z", "writer-b", "2030-01-12T00:00:00Z").claim.unwrap();
    let resolution = write("Dana", "2030-02-01T00:00:00Z", "writer-c", "2030-01-13T00:00:00Z");
    assert_eq!(resolution.retired.len(), 2, "the later explicit write resolves both earlier claims");
    assert!(resolution.retired.iter().any(|c| c.claim_id == first.claim_id));
    assert!(resolution.retired.iter().any(|c| c.claim_id == second.claim_id));
    assert_eq!(resolution.claim.unwrap().object, "Dana");
}
