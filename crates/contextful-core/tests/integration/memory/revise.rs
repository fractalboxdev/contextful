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

/// A claim of equal or higher tier retires a live prior of its subject, predicate and scope when both are open-ended or share a valid-from instant: the prior's `valid_to` becomes the claim's `valid_from`, and `superseded_by` names it.
// spec: read.revise.supersede@2eeab5bb
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
    let scoped = Claim { scope: Some("emea".into()), ..dana };
    assert!(revise(lee, &[scoped]).retired.is_empty());
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
