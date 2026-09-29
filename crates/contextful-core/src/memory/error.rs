//! The refusals of the memory operations of the `read` contract, one variant per error
//! identifier.
//!
//! A variant's `Display` begins with its identifier, so a surface printing the error
//! names the refusal a caller greps for.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MemoryError {
    /// A direct write targeted a memory shape other than claims. (`read.revise.direct-write`)
    #[error("MemoryDirectWriteShapeRefused: {0}")]
    DirectWriteShapeRefused(String),
    /// A candidate edge endpoint resolves to no identity. (`read.resolve-entity.edge-endpoint`)
    #[error("MemoryEdgeEndpointUnresolved: {0}")]
    EdgeEndpointUnresolved(String),
    /// A mention resolves onto two identities with no separating key. (`read.resolve-entity.ambiguous-mention`)
    #[error("MemoryEntityAmbiguous: {0}")]
    EntityAmbiguous(String),
    /// A claim names more evidence references than the bound. (`read.recall.evidence-references`)
    #[error("MemoryEvidenceOverflow: {0}")]
    EvidenceOverflow(String),
    /// A claim's evidence does not resolve through the caller's session. (`read.recall.evidence-unresolved`)
    #[error("MemoryEvidenceUnresolved: {0}")]
    EvidenceUnresolved(String),
    /// A keyed recall named a table declaring no `memory_facts` shape. (`read.recall.keyed-not-claims`)
    #[error("MemoryRecallNotClaims: {0}")]
    RecallNotClaims(String),
    /// An extract batch exhausted its attempts and was dead-lettered. (`read.synthesize.dead-letter`)
    #[error("MemoryExtractExhausted: {0}")]
    ExtractExhausted(String),
    /// A table naming a memory shape omits a canonical column. (`read.declare.canonical-column`)
    #[error("MemoryShapeColumnMissing: {0}")]
    ShapeColumnMissing(String),
    /// A candidate edge carries an undeclared relation type. (`read.declare.undeclared-relation`)
    #[error("MemoryUndeclaredRelation: {0}")]
    UndeclaredRelation(String),
    /// A judgment verdict carries no http or https settling citation. (`read.settle.settling-citation`)
    #[error("OutcomeCitationMissing: {0}")]
    OutcomeCitationMissing(String),
    /// A prediction registration breaks the one-form, one-source, comparator-iff-metric rule. (`read.settle.registration`)
    #[error("OutcomeRegistrationInvalid: {0}")]
    OutcomeRegistrationInvalid(String),
    /// An observation's verdict carries no source or a source other than the registration's. (`read.settle.source-mismatch`)
    #[error("OutcomeSourceMismatch: {0}")]
    OutcomeSourceMismatch(String),
}

impl MemoryError {
    /// The error identifier.
    pub fn identifier(&self) -> &'static str {
        match self {
            MemoryError::DirectWriteShapeRefused(_) => "MemoryDirectWriteShapeRefused",
            MemoryError::EdgeEndpointUnresolved(_) => "MemoryEdgeEndpointUnresolved",
            MemoryError::EntityAmbiguous(_) => "MemoryEntityAmbiguous",
            MemoryError::EvidenceOverflow(_) => "MemoryEvidenceOverflow",
            MemoryError::EvidenceUnresolved(_) => "MemoryEvidenceUnresolved",
            MemoryError::ExtractExhausted(_) => "MemoryExtractExhausted",
            MemoryError::RecallNotClaims(_) => "MemoryRecallNotClaims",
            MemoryError::ShapeColumnMissing(_) => "MemoryShapeColumnMissing",
            MemoryError::UndeclaredRelation(_) => "MemoryUndeclaredRelation",
            MemoryError::OutcomeCitationMissing(_) => "OutcomeCitationMissing",
            MemoryError::OutcomeRegistrationInvalid(_) => "OutcomeRegistrationInvalid",
            MemoryError::OutcomeSourceMismatch(_) => "OutcomeSourceMismatch",
        }
    }
}
