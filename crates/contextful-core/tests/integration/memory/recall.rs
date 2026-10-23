//! `read.recall`: the evidence gate.

use contextful_core::memory::recall::{admit, gate, EvidenceRead, Grounding, EVIDENCE_GATE, EVIDENCE_REFERENCES};
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

/// For claims citing more than 256 entries of evidence, recall returns the first 256 in cited table, run and sequence
/// order. `evidence_omitted` reports the remaining count.
// spec: read.recall.evidence-references@74eb3869
#[test]
fn recall_caps_evidence_at_256_and_counts_omitted() {
    assert_eq!(EVIDENCE_REFERENCES, 256);
    let full = admit(Some(&refs(256, "research/notes")), &[], readable).unwrap();
    assert_eq!((full.served.len(), full.omitted), (256, 0));
    // Cited out of order across two tables: the served prefix follows table, run, sequence.
    let mut cited: Vec<serde_json::Value> = (0..300).rev().map(|i| json!({ "table": "research/notes", "run": "run-0001", "seq": i })).collect();
    cited.push(json!({ "table": "a/first", "run": "run-0009", "seq": 7 }));
    let capped = admit(Some(&json!(cited).to_string()), &[], readable).unwrap();
    assert_eq!((capped.served.len(), capped.omitted), (256, 45));
    assert_eq!((capped.served[0].table.as_str(), capped.served[0].seq), ("a/first", 7));
    assert_eq!((capped.served[1].table.as_str(), capped.served[1].seq), ("research/notes", 0));
    assert_eq!(capped.served[255].seq, 254);
    let three_hundred = admit(Some(&refs(300, "research/notes")), &[], readable).unwrap();
    assert_eq!((three_hundred.served.len(), three_hundred.omitted, three_hundred.grounding), (256, 44, Grounding::Cited));
}

/// Every entry, served or omitted, passes the evidence gate: entry 290 of 300 reading masked
/// suppresses the claim.
#[test]
fn recall_gates_omitted_entries() {
    let masked_at = |n: i64| move |r: &contextful_core::memory::synthesize::EvidenceRef| if r.seq == n { EvidenceRead::Masked } else { EvidenceRead::Readable };
    assert!(matches!(admit(Some(&refs(300, "research/notes")), &[], masked_at(290)), Err(MemoryError::EvidenceUnresolved(_))));
    assert!(matches!(gate(Some(&refs(300, "research/notes")), &[], |r| if r.seq == 299 { EvidenceRead::Unreadable } else { EvidenceRead::Readable }), Err(MemoryError::EvidenceUnresolved(_))));
    assert!(matches!(admit(Some(&refs(300, "research/notes")), &[], |r| if r.seq == 280 { EvidenceRead::Stale } else { EvidenceRead::Readable }), Ok(a) if a.grounding == Grounding::Stale));
}

/// Recall suppresses a claim with more than 4096 entries of evidence as unresolved and raises
/// `MemoryEvidenceOverflow`.
// spec: read.recall.evidence-gate-bound@dd1350b0
#[test]
fn recall_suppresses_claim_over_4096_entries() {
    assert_eq!(EVIDENCE_GATE, 4096);
    let at_bound = admit(Some(&refs(4096, "research/notes")), &[], readable).unwrap();
    assert_eq!((at_bound.served.len(), at_bound.omitted), (256, 3840));
    assert!(matches!(gate(Some(&refs(4097, "research/notes")), &[], readable), Err(MemoryError::EvidenceOverflow(_))));
}
