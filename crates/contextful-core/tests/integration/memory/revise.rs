//! `read.revise`: tier, supersession and the direct write.

use super::at;
use contextful_core::memory::declare::Shape;
use contextful_core::memory::revise::{
    claim_id, direct_write, revise, tier, Claim, Grounding, Tier, WritePath, CONTRADICTION_VALIDITY_SECS,
};
use contextful_core::memory::MemoryError;

fn claim(object: &str, tier: Tier, from: &str) -> Claim {
    Claim {
        claim_id: claim_id("acme", "cfo", object, None),
        subject: "acme".into(),
        predicate: "cfo".into(),
        object: object.into(),
        scope: None,
        tier,
        confidence: 0.9,
        valid_from: at(from),
        valid_to: None,
        evidence: Vec::new(),
        superseded_by: None,
        grant_id: "g".into(),
        agent: None,
    }
}

/// `tier` is stamped from the write path and grounding, never the payload: a synthesis pass stamps `derived`, a direct write `curated`, a fetched result `researched`, and mixed grounding takes the lowest.
// spec: read.revise.tier@008ddbaa
#[test]
fn tier_follows_the_write_path_and_the_lowest_grounding() {
    assert_eq!(tier(WritePath::Synthesis, &[Grounding::Landed]), Tier::Derived);
    assert_eq!(tier(WritePath::Direct, &[]), Tier::Curated);
    assert_eq!(tier(WritePath::Synthesis, &[Grounding::Fetched]), Tier::Researched);
    assert_eq!(tier(WritePath::Direct, &[Grounding::Landed, Grounding::Fetched]), Tier::Researched);
    assert!(Tier::Researched < Tier::Derived && Tier::Derived < Tier::Curated);
}

/// A higher-standing successor ends the prior's interval and names its successor.
// spec: read.revise.supersede@5e9c89cf
#[test]
fn an_equal_or_higher_claim_retires_its_prior_on_one_line() {
    let dana = claim("Dana", Tier::Derived, "2030-01-01T00:00:00Z");
    let lee = claim("Lee", Tier::Derived, "2030-02-01T00:00:00Z");
    let r = revise(lee.clone(), std::slice::from_ref(&dana));
    assert_eq!(r.landed.as_ref().unwrap().object, "Lee");
    assert_eq!(r.retired.len(), 1);
    assert_eq!(r.retired[0].valid_to, Some(lee.valid_from));
    assert_eq!(r.retired[0].superseded_by.as_deref(), Some(lee.claim_id.as_str()));
    assert!(!r.retired[0].live_at(at("2030-03-01T00:00:00Z")));

    // Restating the live object lands nothing.
    let again = revise(claim("Dana", Tier::Derived, "2030-02-01T00:00:00Z"), std::slice::from_ref(&dana));
    assert_eq!((again.landed, again.retired.len()), (None, 0));

    // A lower-standing contradiction retires nothing and lands bounded.
    let curated = claim("Dana", Tier::Curated, "2030-01-01T00:00:00Z");
    let weak = revise(claim("Lee", Tier::Derived, "2030-02-01T00:00:00Z"), std::slice::from_ref(&curated));
    assert!(weak.retired.is_empty());
    assert_eq!(weak.landed.unwrap().valid_to, Some(at("2030-02-01T00:00:00Z").plus_secs(CONTRADICTION_VALIDITY_SECS)));

    // A bounded prior anchored elsewhere is another validity line.
    let bounded = Claim { valid_to: Some(at("2030-01-15T00:00:00Z")), ..dana.clone() };
    assert!(revise(lee.clone(), &[bounded]).retired.is_empty());

    // Another scope is another line.
    let scoped = Claim { scope: Some("emea".into()), ..dana.clone() };
    assert!(revise(lee.clone(), &[scoped]).retired.is_empty());

    // An expired prior is no longer live: it is neither retired nor restated.
    let expired = Claim { valid_to: Some(at("2030-01-20T00:00:00Z")), ..dana.clone() };
    let after = revise(lee, std::slice::from_ref(&expired));
    assert!(after.retired.is_empty() && after.landed.is_some());
    let again_after = revise(claim("Dana", Tier::Derived, "2030-02-01T00:00:00Z"), &[expired]);
    assert!(again_after.landed.is_some(), "restating an expired claim lands it anew");

    // A higher-tier restatement promotes the claim: it re-lands at the higher tier.
    let promoted = revise(claim("Dana", Tier::Curated, "2030-02-01T00:00:00Z"), std::slice::from_ref(&dana));
    assert_eq!(promoted.landed.map(|c| (c.claim_id, c.tier)), Some((dana.claim_id.clone(), Tier::Curated)));
    let demoted = revise(claim("Dana", Tier::Researched, "2030-02-01T00:00:00Z"), std::slice::from_ref(&dana));
    assert_eq!(demoted.landed, None);
}

#[test]
fn a_later_reaffirmation_resolves_multiple_live_objects() {
    let dana = claim("Dana", Tier::Curated, "2030-01-01T00:00:00Z");
    let lee = claim("Lee", Tier::Curated, "2030-01-01T00:00:00Z");
    let mut reaffirmed = claim("Dana", Tier::Curated, "2030-02-01T00:00:00Z");
    reaffirmed.claim_id = "new-observation".into();
    let revision = revise(reaffirmed.clone(), &[dana.clone(), lee.clone()]);
    assert_eq!(revision.landed, Some(reaffirmed.clone()));
    assert_eq!(revision.retired.len(), 2);
    assert!(revision.retired.iter().all(|prior| prior.valid_to == Some(reaffirmed.valid_from) && prior.superseded_by.as_deref() == Some(reaffirmed.claim_id.as_str())));

    let same_id = revise(claim("Dana", Tier::Curated, "2030-02-01T00:00:00Z"), &[dana, lee.clone()]);
    assert!(same_id.landed.is_none(), "the existing claim keeps its original validity interval");
    assert_eq!(same_id.retired.len(), 1);
    assert_eq!(same_id.retired[0].claim_id, lee.claim_id);
}

/// The direct write accepts claims alone. Naming `memory_episodes`, `memory_entities`, `memory_edges` or `memory_preferences` raises `MemoryDirectWriteShapeRefused`; an entity row enters through the entity upsert.
// spec: read.revise.direct-write@b2e8a7eb
#[test]
fn the_direct_write_accepts_claims_alone() {
    assert_eq!(direct_write("team_memory", Shape::Facts), Ok(()));
    for shape in [Shape::Episodes, Shape::Entities, Shape::Edges, Shape::Preferences] {
        match direct_write("t", shape) {
            Err(MemoryError::DirectWriteShapeRefused(why)) => {
                assert!(why.contains(shape.name()), "{why}");
                assert_eq!(why.contains("entity upsert"), shape == Shape::Entities);
            }
            other => panic!("{other:?}"),
        }
    }
}
