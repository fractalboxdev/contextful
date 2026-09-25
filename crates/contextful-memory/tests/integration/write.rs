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
    Landing { node: &node, at: at("2030-01-10T00:00:00Z"), writer: &w, run_id: "memory-derived".into(), boundary: &super::synthesize::admit }
        .commit(&f.face, "memory/facts", &[derived], &[])
        .unwrap();
    let promoted = write_claim(&f.face, &writer, "memory/facts", candidate("Dana"), &node, at("2030-01-11T00:00:00Z"), &super::synthesize::admit).unwrap();
    assert_eq!(promoted.claim.map(|c| c.tier), Some(Tier::Curated));
}
