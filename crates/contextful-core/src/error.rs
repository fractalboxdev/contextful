//! The refusals of the `authority` contract, one variant per error identifier.
//!
//! A variant's `Display` begins with its identifier, so a surface printing the error
//! names the refusal a caller greps for.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthorityError {
    /// A child expiry past its parent's. (`authority.attenuate.expiry-extended`)
    #[error("AttenuationExpiryExtended: {0}")]
    AttenuationExpiryExtended(String),
    /// A child dropping or changing its parent's tenant scope. (`authority.attenuate.tenant-dropped`)
    #[error("AttenuationTenantDropped: {0}")]
    AttenuationTenantDropped(String),
    /// A child broader than its parent on a grant dimension. (`authority.attenuate.widens`)
    #[error("AttenuationWidens: {0}")]
    AttenuationWidens(String),
    /// A credential audience differing from, or absent against, the declared one. (`authority.verify.audience-mismatch`)
    #[error("AudienceMismatch: {0}")]
    AudienceMismatch(String),
    /// A credential past its expiry at admission or an effect boundary. (`authority.verify.expired`)
    #[error("AuthorityExpired: {0}")]
    AuthorityExpired(String),
    /// An authorization join consumed a link whose method does not authorize. (`authority.identify.unverified-link`)
    #[error("AuthorityLinkUnverified: {0}")]
    AuthorityLinkUnverified(String),
    /// A denylisted credential, or one below the current scoped epoch. (`authority.revoke.revoked`)
    #[error("AuthorityRevoked: {0}")]
    AuthorityRevoked(String),
    /// A subject value fails mint hygiene, or arrives padded on the exchange. (`authority.identify.malformed-value`)
    #[error("AuthoritySubjectMalformed: {0}")]
    AuthoritySubjectMalformed(String),
    /// A credential carries no subject member. (`authority.identify.subject-missing`)
    #[error("AuthoritySubjectMissing: {0}")]
    AuthoritySubjectMissing(String),
    /// A derivation names a different on-behalf-of principal. (`authority.identify.subject-rebound`)
    #[error("AuthoritySubjectRebound: {0}")]
    AuthoritySubjectRebound(String),
    /// A credential in a withdrawn format. (`authority.revoke.format-withdrawn`)
    #[error("CredentialFormatWithdrawn: {0}")]
    CredentialFormatWithdrawn(String),
    /// An external assertion failing any exchange check. (`authority.exchange.assertion-invalid`)
    #[error("ExchangeAssertionInvalid: {0}")]
    ExchangeAssertionInvalid(String),
    /// An exchange configured with no verifying material. (`authority.exchange.material-missing`)
    #[error("ExchangeMaterialMissing: {0}")]
    ExchangeMaterialMissing(String),
    /// An action outside the four-verb vocabulary. (`authority.grant.unknown-action`)
    #[error("GrantActionUnknown: {0}")]
    GrantActionUnknown(String),
    /// A table pattern with a star before its final position. (`authority.grant.malformed-pattern`)
    #[error("GrantPatternMalformed: {0}")]
    GrantPatternMalformed(String),
    /// A pipeline identifier no grant covers. (`authority.grant.pipeline-not-covered`)
    #[error("GrantPipelineNotCovered: {0}")]
    GrantPipelineNotCovered(String),
    /// A run trace whose pipeline no grant covers; names no pipeline. (`authority.grant.run-trace-denied`)
    #[error("GrantRunTraceDenied: {0}")]
    GrantRunTraceDenied(String),
    /// A query template outside the credential's allowlist. (`authority.grant.template-not-allowed`)
    #[error("GrantTemplateNotAllowed: {0}")]
    GrantTemplateNotAllowed(String),
    /// A tenant scope on a table with no bare outermost partition column. (`authority.grant.tenant-unbindable`)
    #[error("GrantTenantUnbindable: {0}")]
    GrantTenantUnbindable(String),
    /// A requested lifetime above the persisted ceiling. (`authority.issue.above-ceiling`)
    #[error("IssuanceLifetimeAboveCeiling: {0}")]
    IssuanceLifetimeAboveCeiling(String),
    /// A write or execute grant naming no on-behalf-of principal. (`authority.issue.principal-required`)
    #[error("IssuancePrincipalRequired: {0}")]
    IssuancePrincipalRequired(String),
    /// A mint request without an admin grant. (`authority.issue.unauthorized-mint`)
    #[error("IssuanceUnauthorized: {0}")]
    IssuanceUnauthorized(String),
    /// A subject declaring a wildcard inference zone at the mint. (`authority.issue.zone-wildcard`)
    #[error("IssuanceZoneWildcard: {0}")]
    IssuanceZoneWildcard(String),
    /// A face configured with no issuer key. (`authority.issue.missing-key`)
    #[error("IssuerKeyMissing: {0}")]
    IssuerKeyMissing(String),
    /// An issuer key reference resolving to no material. (`authority.issue.unresolvable-key`)
    #[error("IssuerKeyUnresolvable: {0}")]
    IssuerKeyUnresolvable(String),
    /// A last-known-good key set past its staleness bound. (`authority.verify.key-set-stale`)
    #[error("KeySetStale: {0}")]
    KeySetStale(String),
    /// An opted-into published key set that could not be obtained, or a malformed pin. (`authority.verify.key-set-unavailable`)
    #[error("KeySetUnavailable: {0}")]
    KeySetUnavailable(String),
    /// A request proof failing against the confirmation thumbprint. (`authority.verify.possession-invalid`)
    #[error("PossessionProofInvalid: {0}")]
    PossessionProofInvalid(String),
    /// A proof nonce repeated inside the replay window. (`authority.verify.replayed-nonce`)
    #[error("PossessionProofReplayed: {0}")]
    PossessionProofReplayed(String),
    /// A credential element the profile does not name. (`authority.profile.unrecognized-element`)
    #[error("ProfileElementUnrecognized: {0}")]
    ProfileElementUnrecognized(String),
    /// Evaluator input past the fact or iteration ceiling. (`authority.profile.evaluator-bound`)
    #[error("ProfileEvaluationBudget: {0}")]
    ProfileEvaluationBudget(String),
    /// A token block introduces an engine-reserved fact. (`authority.profile.reserved-fact`)
    #[error("ProfileReservedFact: {0}")]
    ProfileReservedFact(String),
    /// A restriction no read evaluator exists for. (`authority.profile.unevaluated-restriction`)
    #[error("ProfileRestrictionUnevaluated: {0}")]
    ProfileRestrictionUnevaluated(String),
    /// A profile version outside the checkpoint's supported set. (`authority.profile.version-unsupported`)
    #[error("ProfileVersionUnsupported: {0}")]
    ProfileVersionUnsupported(String),
    /// A mint attempted on a replica. (`authority.issue.replica-mint`)
    #[error("ReplicaCannotIssue: {0}")]
    ReplicaCannotIssue(String),
    /// A rotation grace window shorter than the issuance ceiling. (`authority.revoke.short-grace`)
    #[error("RotationGraceTooShort: {0}")]
    RotationGraceTooShort(String),
    /// A named scheme differing from the pinned key's. (`authority.issue.algorithm-mismatch`)
    #[error("SignatureAlgorithmMismatch: {0}")]
    SignatureAlgorithmMismatch(String),
    /// A block signature failing against every pinned key. (`authority.verify.bad-signature`)
    #[error("SignatureInvalid: {0}")]
    SignatureInvalid(String),
    /// A credential timestamp outside the timestamp grammar. (`authority.verify.malformed-timestamp`)
    #[error("TimestampMalformed: {0}")]
    TimestampMalformed(String),
}
