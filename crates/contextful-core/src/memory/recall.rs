//! `read.recall`: the evidence gate a claim passes before it is served.

use super::synthesize::EvidenceRef;
use super::MemoryError;

/// Evidence references one claim names before recall suppresses it
/// (`read.recall.evidence-references`).
pub const EVIDENCE_REFERENCES: usize = 256;

/// How one evidence row reads through the caller's own session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceRead {
    /// The row reads, every cell as stored.
    Readable,
    /// The row reads with a cell masked or nulled by zone.
    Masked,
    /// The table is registered and the row does not read.
    Unreadable,
    /// The table is not registered for the caller, or does not exist.
    UnknownTable,
}

/// Parse a claim's stored evidence list; text that is not a list of references is
/// malformed lineage.
pub fn parse_evidence(text: Option<&str>) -> Result<Vec<EvidenceRef>, MemoryError> {
    let text = text.ok_or_else(|| MemoryError::EvidenceUnresolved("the claim carries no evidence list".into()))?;
    serde_json::from_str(text).map_err(|e| MemoryError::EvidenceUnresolved(format!("the evidence list is malformed: {e}")))
}

/// Gate one claim: every evidence row reads, unmasked, through the caller's session. A
/// list past 256 references is suppressed as an overflow
/// (`read.recall.evidence-references`); an unreadable or masked row, an unknown table, a
/// reference into a memory table or malformed lineage suppresses it as unresolved
/// (`read.recall.evidence-unresolved`). The refusal names no evidence row.
pub fn gate(
    evidence: Option<&str>,
    memory_tables: &[String],
    read: impl Fn(&EvidenceRef) -> EvidenceRead,
) -> Result<(), MemoryError> {
    let refs = parse_evidence(evidence)?;
    if refs.len() > EVIDENCE_REFERENCES {
        return Err(MemoryError::EvidenceOverflow(format!(
            "the claim names {} evidence references; the bound is {EVIDENCE_REFERENCES}",
            refs.len()
        )));
    }
    if refs.is_empty() {
        return Err(MemoryError::EvidenceUnresolved("the claim names no evidence".into()));
    }
    for r in &refs {
        if memory_tables.iter().any(|t| t == &r.table) {
            return Err(MemoryError::EvidenceUnresolved("a reference names another memory row".into()));
        }
        match read(r) {
            EvidenceRead::Readable => {}
            EvidenceRead::Masked => return Err(MemoryError::EvidenceUnresolved("an evidence row reads masked".into())),
            EvidenceRead::Unreadable => return Err(MemoryError::EvidenceUnresolved("an evidence row does not read".into())),
            EvidenceRead::UnknownTable => {
                return Err(MemoryError::EvidenceUnresolved("an evidence table is unknown to this session".into()))
            }
        }
    }
    Ok(())
}
