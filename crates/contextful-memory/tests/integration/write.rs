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
        evidence: vec![EvidenceRef { table: "research/notes".into(), run: "run-0001".into(), seq: 0 }],
    }
}

#[test]
fn a_direct_write_lands_a_curated_claim_and_retires_a_derived_prior() {
    let f = Fixture::new();
    let writer = f.writer();
    let node = NodeId::parse("memory-a").unwrap();
    let first = write_claim(&f.face, &writer, "memory/facts", candidate("Dana"), &node, at("2030-01-11T00:00:00Z")).unwrap();
    assert_eq!(first.claim.unwrap().tier, Tier::Curated);
    let second = write_claim(&f.face, &writer, "memory/facts", candidate("Lee"), &node, at("2030-01-12T00:00:00Z")).unwrap();
    assert_eq!(second.retired.len(), 1);
    assert_eq!(second.retired[0].object, "Dana");
    match write_claim(&f.face, &writer, "memory/episodes", candidate("Lee"), &node, at("2030-01-12T00:00:00Z")) {
        Err(MemoryFault::Memory(MemoryError::DirectWriteShapeRefused(why))) => assert!(why.contains("memory_episodes"), "{why}"),
        other => panic!("{other:?}"),
    }
}
