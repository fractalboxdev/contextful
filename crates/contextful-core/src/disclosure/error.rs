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
    #[error("VisibilityFamilyBound: table `{table}`: family `{}` does not permit fidelity `{}`; it permits {}", family.as_str(), fidelity.as_str(), family.permitted())]
    FamilyBound { table: String, family: crate::disclosure::declare::Family, fidelity: crate::disclosure::declare::Fidelity },
    /// A mapping landed a table without naming its family.
    /// (`disclosure.declare-fidelity.family-undeclared`)
    #[error("VisibilityFamilyUndeclared: table `{table}` names no family; declare one of {}", crate::disclosure::declare::Family::names())]
    FamilyUndeclared { table: String },
    /// A sweep run that missed a governed resource advanced the watermark. (`disclosure.sweep.coverage-watermark`)
    #[error("VisibilityWatermarkUncovered: source `{origin}`: the run did not read {missed} governed resource(s), first `{first}`; watermark_at stays where it stood")]
    WatermarkUncovered { origin: String, missed: usize, first: String },
    /// A grant landed with no resource row, or on an unknown-class resource.
    /// (`disclosure.sweep.orphan-grant`)
    #[error("VisibilityOrphanGrant: source `{origin}`: grant on resource `{resource_id}` names {reason}; the run commits nothing")]
    OrphanGrant { origin: String, resource_id: String, reason: &'static str },
    /// A servable binding lacks a declared sweep or a staleness budget.
    /// (`disclosure.mirror.incomplete-binding`)
    #[error("VisibilityBindingIncomplete: table `{table}` binds source `{origin}` at `{fidelity}` with {missing}")]
    BindingIncomplete { table: String, origin: String, fidelity: &'static str, missing: &'static str },
    /// A table on an organization-wide face carries no visibility block.
    /// (`disclosure.mirror.unbound-table`)
    #[error("VisibilityUnboundTable: table `{table}` carries no visibility block, and this project's faces are organization-wide")]
    UnboundTable { table: String },
    /// The watermark advanced from an event stream not declared gap-detectable. (`disclosure.sweep.ungapped-stream`)
    #[error("VisibilityUngappedStream: source `{origin}`: {stream}; a webhook run moves watermark_at only from a stream declaring `gap_detectable = true`")]
    UngappedStream { origin: String, stream: String },
    /// A pack landed a table with neither a mapping nor an exclusion. (`disclosure.pack.mapping-absent`)
    #[error("VisibilityMappingAbsent: pack `{pack}` lands table `{table}` with neither an access mapping nor `excluded`")]
    MappingAbsent { pack: String, table: String },
    /// A pack carries an access-shaped key the mapping schema lacks.
    /// (`disclosure.pack.asserts-access`)
    #[error("VisibilityPackAssertsAccess: pack `{pack}`: key `{key}` asserts access outside the mapping schema")]
    PackAssertsAccess { pack: String, key: String },
    /// A container allowlist is evaluated as any-of. (`disclosure.pack.any-of-allowlist`)
    #[error("VisibilityAllowlistAnyOf: pack `{pack}`: a container allowlist evaluates all-of; `any-of` widens every entry into an opening")]
    AllowlistAnyOf { pack: String },
    /// A table declared coarse where its source emits no container signal.
    /// (`disclosure.pack.unsupported-coarse`)
    #[error("VisibilityUnsupportedCoarse: pack `{pack}`: table `{table}` declares `coarse` and names no `container_key`")]
    UnsupportedCoarse { pack: String, table: String },
    /// The group closure reached its depth or node bound; no set is returned. (`disclosure.reach.closure-overflow`)
    #[error("VisibilityClosureDepth: source `{origin}`: the group closure of `{subject}` reached its bound of {bound}; no reachable set is returned")]
    ClosureDepth { origin: String, subject: String, bound: String },
    /// A result produced past a budget or under the narrowing posture was retained. (`disclosure.reach.degraded-uncached`)
    #[error("VisibilityDegradedCached: {what} was produced past a budget or under the narrowing posture and is not retained")]
    DegradedCached { what: String },
    /// Source lag exceeds a touched table's budget, or the source was never fully swept. (`disclosure.bound-staleness.access-stale`)
    #[error("VisibilityAccessStale: table `{table}`: source `{origin}` {reason}; lag {lag}, budget {budget}s, last observation {last_observed}")]
    AccessStale { table: String, origin: String, reason: &'static str, lag: String, budget: u64, last_observed: String },
    /// A budget is tighter than the sweep cadence its source sustains.
    /// (`disclosure.bound-staleness.budget-below-cadence`)
    #[error("VisibilityBudgetUnreachable: table `{table}`: budget {budget}s is tighter than the {cadence}s sweep cadence source `{origin}` sustains")]
    BudgetUnreachable { table: String, origin: String, budget: u64, cadence: u64 },
    /// A federated table was registered on a read face.
    /// (`disclosure.declare-fidelity.federated-registered`)
    #[error("VisibilityFederatedRegistered: table `{table}` is federated; it answers live under the reader's credential and registers on no read face")]
    FederatedRegistered { table: String },
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
            VisibilityError::BindingIncomplete { .. } => "VisibilityBindingIncomplete",
            VisibilityError::UnboundTable { .. } => "VisibilityUnboundTable",
            VisibilityError::UngappedStream { .. } => "VisibilityUngappedStream",
            VisibilityError::MappingAbsent { .. } => "VisibilityMappingAbsent",
            VisibilityError::PackAssertsAccess { .. } => "VisibilityPackAssertsAccess",
            VisibilityError::AllowlistAnyOf { .. } => "VisibilityAllowlistAnyOf",
            VisibilityError::UnsupportedCoarse { .. } => "VisibilityUnsupportedCoarse",
            VisibilityError::ClosureDepth { .. } => "VisibilityClosureDepth",
            VisibilityError::DegradedCached { .. } => "VisibilityDegradedCached",
            VisibilityError::AccessStale { .. } => "VisibilityAccessStale",
            VisibilityError::BudgetUnreachable { .. } => "VisibilityBudgetUnreachable",
            VisibilityError::FederatedRegistered { .. } => "VisibilityFederatedRegistered",
            VisibilityError::DiagnosticRow(_) => "VisibilityDiagnosticRow",
            VisibilityError::NoObservations(_) => "VisibilityNoObservations",
            VisibilityError::UnqualifiedAssurance(_) => "VisibilityUnqualifiedAssurance",
            VisibilityError::IndividualNamed(_) => "VisibilityIndividualNamed",
        }
    }

    /// The HTTP status a read surface answers this refusal with, where the contract fixes
    /// one: `503` for a stale source, which a later sweep clears, and the non-retryable
    /// `422` for a closure past its bound.
    pub fn status(&self) -> Option<u16> {
        match self {
            VisibilityError::AccessStale { .. } => Some(503),
            VisibilityError::ClosureDepth { .. } => Some(422),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DisclosureError {
    /// A published aggregate-shaped model carries no policy and no opt-out. (`disclosure.set-mode.policy-absent`)
    #[error("DisclosurePolicyAbsent: {0}")]
    PolicyAbsent(String),
    /// A published model's statement text does not read. (`disclosure.set-mode.model-unreadable`)
    #[error("DisclosureModelUnreadable: {0}")]
    ModelUnreadable(String),
    /// A template declaration holds more than one statement. (`disclosure.template.single-statement`)
    #[error("DisclosureTemplateMultiStatement: {0}")]
    TemplateMultiStatement(String),
    /// A grouping allowlist is empty or names a non-column. (`disclosure.suppress.grouping-allowlist`)
    #[error("DisclosureGroupingAllowlistEmpty: {0}")]
    GroupingAllowlistEmpty(String),
    /// Per-contributor masses are unavailable where a share constraint applies. (`disclosure.suppress.dominance-unverifiable`)
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
    /// A contributing unit's budget reservation failed; nothing is published. (`disclosure.release.budget-reservation`)
    #[error("DisclosureUnitBudgetExhausted: {0}")]
    UnitBudgetExhausted(String),
    /// A released model's policy declares no positive per-run spend. (`disclosure.release.spend-declaration`)
    #[error("DisclosureSpendUndeclared: {0}")]
    SpendUndeclared(String),
    /// A release requests an activation handle or lookalike segment. (`disclosure.release.lookalike-refusal`)
    #[error("DisclosureLookalikeUnsupported: {0}")]
    LookalikeUnsupported(String),
    /// A model declaring a disclosure policy is built outside a release. (`disclosure.release.governed-build`)
    #[error("DisclosureReleaseRequired: {0}")]
    ReleaseRequired(String),
    /// A noised sum has no finite contributor bounds or positive quantum. (`disclosure.release.metric-bounds`)
    #[error("DisclosureMetricBoundsAbsent: {0}")]
    MetricBoundsAbsent(String),
    /// A carried non-grain column is not constant within its group. (`disclosure.release.duplicate-grain`)
    #[error("DisclosureDuplicateGrain: {0}")]
    DuplicateGrain(String),
    /// A group key is not a subset of the permitted grouping columns. (`disclosure.release.group-key`)
    #[error("DisclosureGroupKeyNotPermitted: {0}")]
    GroupKeyNotPermitted(String),
    /// A governed statement omits the contributor key. (`disclosure.release.contributor-key`)
    #[error("DisclosureContributorKeyAbsent: {0}")]
    ContributorKeyAbsent(String),
    /// A contract declares, or a materialization produces, a forbidden column. (`disclosure.release.forbidden-column`)
    #[error("DisclosureForbiddenColumnPublished: {0}")]
    ForbiddenColumnPublished(String),
    /// A shared store lacks a clean-room requirement. (`disclosure.set-mode.clean-room-preconditions`)
    #[error("DisclosureCleanRoomPreconditionUnmet: {0}")]
    CleanRoomPreconditionUnmet(String),
    /// A cross-owner hashed join is keyed on a static pepper. (`disclosure.set-mode.hashed-join`)
    #[error("DisclosureStaticPepper: {0}")]
    StaticPepper(String),
    /// A cohort key's grain resolves to one person. (`disclosure.bound-cohort.singleton-cohort`)
    #[error("DisclosureSingletonCohort: {0}")]
    SingletonCohort(String),
    /// A per-person table declares exact figures. (`disclosure.bound-cohort.exact-figures`)
    #[error("DisclosureExactPerPerson: {0}")]
    ExactPerPerson(String),
    /// A read grouped a cohort table by a strict subset of its group key. (`disclosure.bound-cohort.cohort-widening`)
    #[error("DisclosureCohortWidening: {0}")]
    CohortWidening(String),
    /// A read touched a cohort model's source table without `read-individual`. (`disclosure.bound-cohort.per-individual-row`)
    #[error("DisclosurePerIndividualUngranted: {0}")]
    PerIndividualUngranted(String),
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
            DisclosureError::UnitBudgetExhausted(_) => "DisclosureUnitBudgetExhausted",
            DisclosureError::SpendUndeclared(_) => "DisclosureSpendUndeclared",
            DisclosureError::LookalikeUnsupported(_) => "DisclosureLookalikeUnsupported",
            DisclosureError::ReleaseRequired(_) => "DisclosureReleaseRequired",
            DisclosureError::MetricBoundsAbsent(_) => "DisclosureMetricBoundsAbsent",
            DisclosureError::DuplicateGrain(_) => "DisclosureDuplicateGrain",
            DisclosureError::GroupKeyNotPermitted(_) => "DisclosureGroupKeyNotPermitted",
            DisclosureError::ContributorKeyAbsent(_) => "DisclosureContributorKeyAbsent",
            DisclosureError::ForbiddenColumnPublished(_) => "DisclosureForbiddenColumnPublished",
            DisclosureError::CleanRoomPreconditionUnmet(_) => "DisclosureCleanRoomPreconditionUnmet",
            DisclosureError::StaticPepper(_) => "DisclosureStaticPepper",
            DisclosureError::SingletonCohort(_) => "DisclosureSingletonCohort",
            DisclosureError::ExactPerPerson(_) => "DisclosureExactPerPerson",
            DisclosureError::CohortWidening(_) => "DisclosureCohortWidening",
            DisclosureError::PerIndividualUngranted(_) => "DisclosurePerIndividualUngranted",
        }
    }
}
