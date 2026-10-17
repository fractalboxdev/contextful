//! The refusals of the enforcement operations of the `authority` contract, one variant
//! per error identifier.
//!
//! A variant's `Display` begins with its identifier, so a surface printing the error
//! names the refusal a caller greps for.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnforceError {
    #[error("EnforceRedactionInvalid: {0}")]
    RedactionInvalid(String),
    /// A value matching a credential shape on the write path. (`authority.resist.credential-shaped-value`)
    #[error("EnforceCredentialShapedValue: {0}")]
    CredentialShapedValue(String),
    /// A combine leaving its primary's digest output unchanged. (`authority.mask.combine-generalizes`)
    #[error("EnforceCombineWithoutGeneralization: {0}")]
    CombineWithoutGeneralization(String),
    /// A keyed hash standing alone over an exhaustible class. (`authority.mask.digest-alone`)
    #[error("EnforceDigestAloneOnExhaustibleClass: {0}")]
    DigestAloneOnExhaustibleClass(String),
    /// A synthesized row declaring a set wider than its evidence intersection. (`authority.place.evidence-floor`)
    #[error("EnforceEvidenceFloorExceeded: {0}")]
    EvidenceFloorExceeded(String),
    /// A synthesized row naming no evidence table. (`authority.place.empty-evidence`)
    #[error("EnforceEvidenceListEmpty: {0}")]
    EvidenceListEmpty(String),
    /// A session asserting a zone wider than the incognito pin. (`authority.place.incognito-widening`)
    #[error("EnforceIncognitoWidening: {0}")]
    IncognitoWidening(String),
    /// Pepper-derived key state in relation text, a plan or the session catalog. (`authority.mask.key-material`)
    #[error("EnforceKeyMaterialInRelation: {0}")]
    KeyMaterialInRelation(String),
    /// A mask naming a column the schema omits. (`authority.mask.absent-column`)
    #[error("EnforceMaskOnAbsentColumn: {0}")]
    MaskOnAbsentColumn(String),
    /// A wildcard zone default in a store with several principals. (`authority.place.permissive-default`)
    #[error("EnforcePermissiveZoneDefault: {0}")]
    PermissiveZoneDefault(String),
    /// A row predicate node outside the typed subset. (`authority.filter-rows.outside-grammar`)
    #[error("EnforcePredicateOutsideGrammar: {0}")]
    PredicateOutsideGrammar(String),
    /// A protected-class surface widened past its floor with no override. (`authority.place.floor-widened`)
    #[error("EnforceProtectedFloorWidened: {0}")]
    ProtectedFloorWidened(String),
    /// Write-time removal on a surface whose allow-set admits a public cloud. (`authority.place.removal-with-public-cloud`)
    #[error("EnforceRemovalWithPublicCloud: {0}")]
    RemovalWithPublicCloud(String),
    /// A root-level object pushed outside the allowlist while a table is withheld. (`authority.bound-redistribution.root-not-allowlisted`)
    #[error("EnforceRootObjectNotAllowlisted: {0}")]
    RootObjectNotAllowlisted(String),
    /// A read of another tenant's rows on a granted table. (`authority.refuse.scope-denied`)
    #[error("EnforceScopeDenied: {0}")]
    ScopeDenied(String),
    /// Objects an earlier push left addressable for a now-withheld table. (`authority.bound-redistribution.left-behind`)
    #[error("EnforceStaleRedistributedObjects: {0}")]
    StaleRedistributedObjects(String),
    /// A mask strategy a binary or vector column's type does not admit. (`authority.mask.typed-strategy`)
    #[error("EnforceStrategyOutsideType: {0}")]
    StrategyOutsideType(String),
    /// A truncation width at or past the primary's output width. (`authority.mask.cuts-nothing`)
    #[error("EnforceTruncationCutsNothing: {0}")]
    TruncationCutsNothing(String),
    /// A digest truncation wider than the class domain and crowd admit. (`authority.mask.truncation-ceiling`)
    #[error("EnforceTruncationTooWide: {0}")]
    TruncationTooWide(String),
    /// A column class outside the class registry. (`authority.mask.unknown-class`)
    #[error("EnforceUnknownClass: {0}")]
    UnknownClass(String),
    /// A read naming a table the caller holds no grant on. (`authority.refuse.ungranted-table`)
    #[error("EnforceUnknownRelation: {0}")]
    UnknownRelation(String),
    /// A write tool registered on an organization-wide face. (`authority.resist.write-tool`)
    #[error("EnforceWriteOnReadOnlyFace: {0}")]
    WriteOnReadOnlyFace(String),
    /// A request asserting a zone its credential does not sign. (`authority.place.asserted-zone`)
    #[error("EnforceZoneAssertionWidens: {0}")]
    ZoneAssertionWidens(String),
    /// A mint signing a zone other than a non-widenable project default. (`authority.place.default-widened`)
    #[error("EnforceZoneDefaultWidened: {0}")]
    ZoneDefaultWidened(String),
    /// An allow-set entry matching no entry form. (`authority.place.unparsed-pattern`)
    #[error("EnforceZonePatternUnparsed: {0}")]
    ZonePatternUnparsed(String),
}

impl EnforceError {
    /// The error identifier.
    pub fn identifier(&self) -> &'static str {
        match self {
            EnforceError::RedactionInvalid(_) => "EnforceRedactionInvalid",
            EnforceError::CredentialShapedValue(_) => "EnforceCredentialShapedValue",
            EnforceError::CombineWithoutGeneralization(_) => "EnforceCombineWithoutGeneralization",
            EnforceError::DigestAloneOnExhaustibleClass(_) => "EnforceDigestAloneOnExhaustibleClass",
            EnforceError::EvidenceFloorExceeded(_) => "EnforceEvidenceFloorExceeded",
            EnforceError::EvidenceListEmpty(_) => "EnforceEvidenceListEmpty",
            EnforceError::IncognitoWidening(_) => "EnforceIncognitoWidening",
            EnforceError::KeyMaterialInRelation(_) => "EnforceKeyMaterialInRelation",
            EnforceError::MaskOnAbsentColumn(_) => "EnforceMaskOnAbsentColumn",
            EnforceError::PermissiveZoneDefault(_) => "EnforcePermissiveZoneDefault",
            EnforceError::PredicateOutsideGrammar(_) => "EnforcePredicateOutsideGrammar",
            EnforceError::ProtectedFloorWidened(_) => "EnforceProtectedFloorWidened",
            EnforceError::RemovalWithPublicCloud(_) => "EnforceRemovalWithPublicCloud",
            EnforceError::RootObjectNotAllowlisted(_) => "EnforceRootObjectNotAllowlisted",
            EnforceError::ScopeDenied(_) => "EnforceScopeDenied",
            EnforceError::StaleRedistributedObjects(_) => "EnforceStaleRedistributedObjects",
            EnforceError::StrategyOutsideType(_) => "EnforceStrategyOutsideType",
            EnforceError::TruncationCutsNothing(_) => "EnforceTruncationCutsNothing",
            EnforceError::TruncationTooWide(_) => "EnforceTruncationTooWide",
            EnforceError::UnknownClass(_) => "EnforceUnknownClass",
            EnforceError::UnknownRelation(_) => "EnforceUnknownRelation",
            EnforceError::WriteOnReadOnlyFace(_) => "EnforceWriteOnReadOnlyFace",
            EnforceError::ZoneAssertionWidens(_) => "EnforceZoneAssertionWidens",
            EnforceError::ZoneDefaultWidened(_) => "EnforceZoneDefaultWidened",
            EnforceError::ZonePatternUnparsed(_) => "EnforceZonePatternUnparsed",
        }
    }
}
