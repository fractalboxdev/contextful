//! The refusals of the `disclosure` contract, one variant per error identifier: the
//! visibility operations' and aggregate suppression's.
//!
//! A variant's `Display` begins with its identifier, so a surface printing the error
//! names the refusal a caller greps for.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VisibilityError {
    /// A staleness budget is compound, bare, or carries an unknown suffix.
    /// (`disclosure.bound-staleness.budget-grammar`)
    #[error("VisibilityBudgetMalformed: table `{table}`: max_acl_staleness `{text}` is not a positive integer with one suffix from s, m, h, d")]
    BudgetMalformed { table: String, text: String },
    /// A table declared a level its source family does not permit.
    /// (`disclosure.declare-fidelity.family-bound`)
    #[error("VisibilityFamilyBound: table `{table}`: family `{family}` does not permit fidelity `{fidelity}`")]
    FamilyBound { table: String, family: &'static str, fidelity: &'static str },
    /// A mapping landed a table without naming its family.
    /// (`disclosure.declare-fidelity.family-undeclared`)
    #[error("VisibilityFamilyUndeclared: table `{table}` names no family")]
    FamilyUndeclared { table: String },
    /// An explanation returned content from the resource it decides about.
    /// (`disclosure.explain.no-row`)
    #[error("VisibilityDiagnosticRow: {0}")]
    DiagnosticRow(String),
    /// A replay window holds no observations; no claim is available.
    /// (`disclosure.explain.empty-window`)
    #[error("VisibilityNoObservations: {0}")]
    NoObservations(String),
    /// A negative assurance answer lacks its coverage block.
    /// (`disclosure.explain.unqualified-assurance`)
    #[error("VisibilityUnqualifiedAssurance: {0}")]
    UnqualifiedAssurance(String),
    /// An explanation rendered the members of a group on the path.
    /// (`disclosure.explain.groups-not-members`)
    #[error("VisibilityIndividualNamed: {0}")]
    IndividualNamed(String),
}

impl VisibilityError {
    /// The error identifier.
    pub fn identifier(&self) -> &'static str {
        match self {
            VisibilityError::BudgetMalformed { .. } => "VisibilityBudgetMalformed",
            VisibilityError::FamilyBound { .. } => "VisibilityFamilyBound",
            VisibilityError::FamilyUndeclared { .. } => "VisibilityFamilyUndeclared",
            VisibilityError::DiagnosticRow(_) => "VisibilityDiagnosticRow",
            VisibilityError::NoObservations(_) => "VisibilityNoObservations",
            VisibilityError::UnqualifiedAssurance(_) => "VisibilityUnqualifiedAssurance",
            VisibilityError::IndividualNamed(_) => "VisibilityIndividualNamed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DisclosureError {
    /// A group under a share constraint arrived without per-contributor masses. (`disclosure.suppress.dominance-unverifiable`)
    #[error("DisclosureDominanceUnverifiable: {0}")]
    DominanceUnverifiable(String),
    /// A declared `min_group_size` is below the floor. (`disclosure.suppress.min-group-size`)
    #[error("DisclosureMinGroupSizeBelowFloor: {0}")]
    MinGroupSizeBelowFloor(String),
    /// A policy sets neither threshold. (`disclosure.suppress.empty-policy`)
    #[error("DisclosurePolicySuppressesNothing: {0}")]
    PolicySuppressesNothing(String),
    /// A declared `max_contributor_share` lies outside `(0, 1]`. (`disclosure.suppress.contributor-share`)
    #[error("DisclosureShareOutOfRange: {0}")]
    ShareOutOfRange(String),
}

impl DisclosureError {
    /// The error identifier.
    pub fn identifier(&self) -> &'static str {
        match self {
            DisclosureError::DominanceUnverifiable(_) => "DisclosureDominanceUnverifiable",
            DisclosureError::MinGroupSizeBelowFloor(_) => "DisclosureMinGroupSizeBelowFloor",
            DisclosureError::PolicySuppressesNothing(_) => "DisclosurePolicySuppressesNothing",
            DisclosureError::ShareOutOfRange(_) => "DisclosureShareOutOfRange",
        }
    }
}
