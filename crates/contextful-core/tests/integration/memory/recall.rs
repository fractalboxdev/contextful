//! `read.recall`: the evidence gate.

use contextful_core::memory::recall::{gate, EvidenceRead, EVIDENCE_REFERENCES};
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
    assert_eq!(gate(Some(&refs(2, "research/notes")), &memory, readable), Ok(()));
    let unresolved = |r: Result<(), MemoryError>| assert!(matches!(r, Err(MemoryError::EvidenceUnresolved(_))), "{r:?}");
    for outcome in [EvidenceRead::Unreadable, EvidenceRead::Masked, EvidenceRead::UnknownTable] {
        unresolved(gate(Some(&refs(1, "research/notes")), &memory, |_| outcome));
    }
    unresolved(gate(Some(&refs(1, "memory/facts")), &memory, readable));
    unresolved(gate(Some("[{\"table\": \"research/notes\"}]"), &memory, readable));
    unresolved(gate(Some("not json"), &memory, readable));
    unresolved(gate(None, &memory, readable));
    // One bad row among good ones suppresses the whole claim.
    unresolved(gate(Some(&refs(3, "research/notes")), &memory, |r| if r.seq == 2 { EvidenceRead::Unreadable } else { EvidenceRead::Readable }));
}

/// A claim naming more than 256 entries of evidence is suppressed unresolved, raising `MemoryEvidenceOverflow`.
// spec: read.recall.evidence-references@d078cc96
#[test]
fn evidence_past_256_references_overflows() {
    assert_eq!(EVIDENCE_REFERENCES, 256);
    assert_eq!(gate(Some(&refs(256, "research/notes")), &[], readable), Ok(()));
    assert!(matches!(gate(Some(&refs(257, "research/notes")), &[], readable), Err(MemoryError::EvidenceOverflow(_))));
}
