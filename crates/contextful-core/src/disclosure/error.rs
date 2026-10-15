//! The refusals of the `disclosure` contract, one variant per error identifier: the
//! visibility operations' and aggregate suppression's.
//!
//! A variant's `Display` begins with its identifier, so a surface printing the error
//! names the refusal a caller greps for.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VisibilityError {
    /// A staleness budget is compound, bare, or carries an unknown suffix.
    /// (`disclosure.bound-staleness.budget-grammar`)
    /// `table` names the table, or the sweep cadence, whose budget text was read.
    #[error("VisibilityBudgetMalformed: {table}: `{text}` is not a positive integer with one suffix from s, m, h, d")]
    BudgetMalformed { table: String, text: String },
    /// A table declared a level its source family does not permit.
    /// (`disclosure.declare-fidelity.family-bound`)
    #[error("VisibilityFamilyBound: table `{table}`: family `{family}` does not permit fidelity `{fidelity}`")]
    FamilyBound { table: String, family: &'static str, fidelity: &'static str },
    /// A mapping landed a table without naming its family.
    /// (`disclosure.declare-fidelity.family-undeclared`)
    #[error("VisibilityFamilyUndeclared: table `{table}` names no family")]
    FamilyUndeclared { table: String },
    /// A full sweep run missed a governed resource of its source and would have advanced
    /// the watermark. (`disclosure.sweep.coverage-watermark`)
    #[error("VisibilityWatermarkUncovered: source `{origin}`: the run did not read {missed} governed resource(s), first `{first}`; watermark_at stays where it stood")]
    WatermarkUncovered { origin: String, missed: usize, first: String },
    /// A grant landed with no resource row, or on an unknown-class resource.
    /// (`disclosure.sweep.orphan-grant`)
    #[error("VisibilityOrphanGrant: source `{origin}`: grant on resource `{resource_id}` names {reason}; the run commits nothing")]
    OrphanGrant { origin: String, resource_id: String, reason: &'static str },
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
            VisibilityError::WatermarkUncovered { .. } => "VisibilityWatermarkUncovered",
            VisibilityError::OrphanGrant { .. } => "VisibilityOrphanGrant",
            VisibilityError::DiagnosticRow(_) => "VisibilityDiagnosticRow",
            VisibilityError::NoObservations(_) => "VisibilityNoObservations",
            VisibilityError::UnqualifiedAssurance(_) => "VisibilityUnqualifiedAssurance",
            VisibilityError::IndividualNamed(_) => "VisibilityIndividualNamed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DisclosureError {
    /// A published aggregate-shaped model has no policy or opt-out. (`disclosure.set-mode.policy-absent`)
    #[error("DisclosurePolicyAbsent: {0}")]
    PolicyAbsent(String),
    /// A published model's local statement file does not read. (`disclosure.set-mode.model-unreadable`)
    #[error("DisclosureModelUnreadable: {0}")]
    ModelUnreadable(String),
    /// A template declaration holds more than one statement. (`disclosure.template.single-statement`)
    #[error("DisclosureTemplateMultiStatement: {0}")]
    TemplateMultiStatement(String),
    /// A grouping allowlist is empty or contains a non-column name. (`disclosure.suppress.grouping-allowlist`)
    #[error("DisclosureGroupingAllowlistEmpty: {0}")]
    GroupingAllowlistEmpty(String),
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
            DisclosureError::PolicyAbsent(_) => "DisclosurePolicyAbsent",
            DisclosureError::ModelUnreadable(_) => "DisclosureModelUnreadable",
            DisclosureError::TemplateMultiStatement(_) => "DisclosureTemplateMultiStatement",
            DisclosureError::GroupingAllowlistEmpty(_) => "DisclosureGroupingAllowlistEmpty",
            DisclosureError::DominanceUnverifiable(_) => "DisclosureDominanceUnverifiable",
            DisclosureError::MinGroupSizeBelowFloor(_) => "DisclosureMinGroupSizeBelowFloor",
            DisclosureError::PolicySuppressesNothing(_) => "DisclosurePolicySuppressesNothing",
            DisclosureError::ShareOutOfRange(_) => "DisclosureShareOutOfRange",
        }
    }
}
