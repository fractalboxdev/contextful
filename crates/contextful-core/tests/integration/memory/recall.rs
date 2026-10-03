//! `read.recall`: the evidence gate.

use contextful_core::memory::recall::{gate, EvidenceRead, Grounding, EVIDENCE_REFERENCES};
use contextful_core::memory::MemoryError;
use serde_json::json;

fn refs(n: usize, table: &str) -> String {
    json!((0..n).map(|i| json!({ "table": table, "run": "run-0001", "seq": i })).collect::<Vec<_>>()).to_string()
}

fn readable(_: &contextful_core::memory::synthesize::EvidenceRef) -> EvidenceRead {
    EvidenceRead::Readable
}

/// An unreadable or masked source row, an unknown table, a reference into another memory row, or malformed lineage suppresses the claim and raises `MemoryEvidenceUnresolved`.
// spec: read.recall.evidence-unresolved@24174b99
#[test]
fn unresolvable_evidence_suppresses_the_claim() {
    let memory = vec!["memory/facts".to_string()];
    assert_eq!(gate(Some(&refs(2, "research/notes")), &memory, readable), Ok(Grounding::Cited));
    let mut outcomes = Vec::new();
    for outcome in [EvidenceRead::Unreadable, EvidenceRead::Masked, EvidenceRead::UnknownTable] {
        outcomes.push(gate(Some(&refs(1, "research/notes")), &memory, |_| outcome));
    }
    outcomes.push(gate(Some(&refs(1, "memory/facts")), &memory, readable));
    outcomes.push(gate(Some("[{\"table\": \"research/notes\"}]"), &memory, readable));
    outcomes.push(gate(Some("not json"), &memory, readable));
    outcomes.push(gate(None, &memory, readable));
    // One bad row among good ones suppresses the whole claim.
    outcomes.push(gate(Some(&refs(3, "research/notes")), &memory, |r| if r.seq == 2 { EvidenceRead::Unreadable } else { EvidenceRead::Readable }));
    let admitted = outcomes.iter().filter(|r| !matches!(r, Err(MemoryError::EvidenceUnresolved(_)))).count();
    crate::emit("memory-evidence-gate", admitted as f64, outcomes.len() as u64, 0);
    assert_eq!(admitted, 0, "{outcomes:?}");
}

/// A claim naming more than 256 entries of evidence is suppressed unresolved, raising `MemoryEvidenceOverflow`.
// spec: read.recall.evidence-references@d078cc96
#[test]
fn evidence_past_256_references_overflows() {
    assert_eq!(EVIDENCE_REFERENCES, 256);
    assert_eq!(gate(Some(&refs(256, "research/notes")), &[], readable), Ok(Grounding::Cited));
    assert!(matches!(gate(Some(&refs(257, "research/notes")), &[], readable), Err(MemoryError::EvidenceOverflow(_))));
}
