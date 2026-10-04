//! The refusals of the `connector` contract the engine raises, one variant per error
//! identifier. A variant's `Display` begins with its identifier.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConnectorError {
    /// A delimited body invalid under its declared encoding. (`connector.source.declared-encoding`)
    #[error("ConnectorEncodingInvalid: {0}")]
    ConnectorEncodingInvalid(String),
    /// A delimited clock column neither RFC 3339 nor a fixed-width digit stamp. (`connector.source.clock-column-spelling`)
    #[error("ConnectorClockColumnRejected: {0}")]
    ConnectorClockColumnRejected(String),
    /// Both expansion pointer forms on one block. (`connector.source.pointer-ambiguity`)
    #[error("ConnectorPointerAmbiguous: {0}")]
    ConnectorPointerAmbiguous(String),
    /// A failed expansion follow-up, failing the whole read. (`connector.source.follow-up-failure`)
    #[error("ConnectorExpansionFailed: {0}")]
    ConnectorExpansionFailed(String),
    /// A pointer template with no placeholder, or one binding into the URL authority. (`connector.source.template-shape`)
    #[error("ConnectorTemplateRejected: {0}")]
    ConnectorTemplateRejected(String),
    /// An expansion placeholder naming a column the rows lack as a scalar. (`connector.source.pointer-column-missing`)
    #[error("ConnectorPointerColumnMissing: {0}")]
    ConnectorPointerColumnMissing(String),
    /// An expansion target column the index rows already carry. (`connector.source.target-column-occupied`)
    #[error("ConnectorTargetColumnOccupied: {0}")]
    ConnectorTargetColumnOccupied(String),
    /// An empty, oversized, repeated or ambiguous Drive root selection. (`connector.source.drive-root-set`)
    #[error("ConnectorDriveRootsInvalid: {0}")]
    ConnectorDriveRootsInvalid(String),
    /// A selected Drive root that is missing or outside its declared Shared Drive. (`connector.source.drive-root-validation`)
    #[error("ConnectorDriveRootRejected: {0}")]
    ConnectorDriveRootRejected(String),
    /// A Drive listing reaching its request cap before every folder page completes. (`connector.source.drive-list-bound`)
    #[error("ConnectorDriveListingExceeded: {0}")]
    ConnectorDriveListingExceeded(String),
    /// A Drive capture mode other than `bytes-and-pages` or `metadata-only`. (`connector.source.drive-mode`)
    #[error("ConnectorDriveModeUnknown: {0}")]
    ConnectorDriveModeUnknown(String),
    /// A Drive file whose version changes during metadata-only capture. (`connector.source.drive-version-consistency`)
    #[error("ConnectorDriveVersionMoved: {0}")]
    ConnectorDriveVersionMoved(String),
    /// A forwarded guest configuration value that is not a table, over 64 KiB, or carrying a reference. (`connector.import.config-shape`)
    #[error("ConnectorConfigRejected: {0}")]
    ConnectorConfigRejected(String),
    /// A guest configuration table declared against a guest exporting no configuration interface. (`connector.import.config-unclaimed`)
    #[error("ConnectorConfigUnclaimed: {0}")]
    ConnectorConfigUnclaimed(String),
    /// Resolved artifact bytes whose re-hash differs from the pin. (`connector.package.digest-mismatch`)
    #[error("ConnectorDigestMismatch: {0}")]
    ConnectorDigestMismatch(String),
    /// A connector artifact referenced over plain HTTP. (`connector.package.insecure-artifact`)
    #[error("ConnectorInsecureArtifact: {0}")]
    ConnectorInsecureArtifact(String),
    /// An unpinned local artifact under a set pin requirement. (`connector.package.local-unpinned`)
    #[error("ConnectorLocalUnpinned: {0}")]
    ConnectorLocalUnpinned(String),
    /// A remote artifact reference with no 64-hex content pin. (`connector.package.remote-unpinned`)
    #[error("ConnectorRemoteUnpinned: {0}")]
    ConnectorRemoteUnpinned(String),
    /// A malformed OCI registry, repository or selector. (`connector.package.oci-reference`)
    #[error("ConnectorOciReferenceInvalid: {0}")]
    ConnectorOciReferenceInvalid(String),
    /// An OCI manifest without one component layer. (`connector.package.oci-component-layer`)
    #[error("ConnectorOciArtifactUnsupported: {0}")]
    ConnectorOciArtifactUnsupported(String),
    /// OCI layer bytes differing from their descriptor digest. (`connector.package.oci-layer-integrity`)
    #[error("ConnectorOciLayerMismatch: {0}")]
    ConnectorOciLayerMismatch(String),
    /// Damaged bytes in the project artifact cache. (`connector.package.remote-cache-corrupt`)
    #[error("ConnectorArtifactCacheCorrupt: {0}")]
    ConnectorArtifactCacheCorrupt(String),
    /// A failed remote artifact request. (`connector.package.remote-fetch-failure`)
    #[error("ConnectorArtifactFetchFailed: {0}")]
    ConnectorArtifactFetchFailed(String),
    /// An allowlist that is empty or bare-wildcard, or an entry carrying a scheme, port or path. (`connector.declare-capability.allowlist-shape`)
    #[error("ConnectorAllowlistRejected: {0}")]
    ConnectorAllowlistRejected(String),
    /// An environment binding not supplied, or supplied off its declared shape. (`connector.declare-capability.binding-unbound`)
    #[error("ConnectorBindingUnbound: {0}")]
    ConnectorBindingUnbound(String),
    /// An environment binding on a key the connector does not read. (`connector.declare-capability.binding-unsupported`)
    #[error("ConnectorBindingUnsupported: {0}")]
    ConnectorBindingUnsupported(String),
    /// A fetched row already carrying a column the source binds from the table name. (`connector.source.bound-column-occupied`)
    #[error("ConnectorBoundColumnOccupied: {0}")]
    ConnectorBoundColumnOccupied(String),
    /// A conditional read beside a pagination shape or a declared incremental field. (`connector.source.conditional-rejected`)
    #[error("ConnectorConditionalRejected: {0}")]
    ConnectorConditionalRejected(String),
    /// A worksheet cell holding a value past the header's width. (`connector.source.cell-out-of-range`)
    #[error("ConnectorCellOutOfRange: {0}")]
    ConnectorCellOutOfRange(String),
    /// A workbook declaration, part or relationship pointing outside the archive. (`connector.source.external-reference`)
    #[error("ConnectorExternalReference: {0}")]
    ConnectorExternalReference(String),
    /// A compound-binary office container, answered with the conversion command. (`connector.source.conversion-required`)
    #[error("ConnectorConversionRequired: {0}")]
    ConnectorConversionRequired(String),
    /// A nested map, block scalar or reserved-prefix key in a note's frontmatter. (`connector.source.frontmatter-shape`)
    #[error("ConnectorFrontmatterRejected: {0}")]
    ConnectorFrontmatterRejected(String),
    /// A record path, pagination shape or decode key the chosen format does not read. (`connector.source.format-key-mismatch`)
    #[error("ConnectorFormatKeyRejected: {0}")]
    ConnectorFormatKeyRejected(String),
    /// A limiter declaration forwarding a credential-bearing response header. (`connector.meter.forward-credential`)
    #[error("ConnectorForwardRejected: {0}")]
    ConnectorForwardRejected(String),
    /// An object source declaring both or neither of a key and a prefix. (`connector.source.object-address`)
    #[error("ConnectorObjectAddressRejected: {0}")]
    ConnectorObjectAddressRejected(String),
    /// An incremental position against a workbook source. (`connector.source.workbook-incremental`)
    #[error("ConnectorIncrementalUnsupported: {0}")]
    ConnectorIncrementalUnsupported(String),
    /// An `incremental` field declared beside a component source, whose position the guest owns. (`connector.package.component-position`)
    #[error("ConnectorPositionOwned: {0}")]
    ConnectorPositionOwned(String),
    /// A non-HTTPS non-loopback limiter endpoint, one carrying a query, fragment or userinfo, or a limiter token that is not a `secret://` reference. (`connector.meter.binding-transport`)
    #[error("ConnectorLimiterBindingRejected: {0}")]
    ConnectorLimiterBindingRejected(String),
    /// A limiter response the engine cannot read. (`connector.meter.unreadable-answer`)
    #[error("ConnectorLimiterUnreadable: {0}")]
    ConnectorLimiterUnreadable(String),
    /// A paging token or link the vendor already served. (`connector.source.page-loop`)
    #[error("ConnectorPageLoop: {0}")]
    ConnectorPageLoop(String),
    /// Two pagination shapes on one source. (`connector.source.pagination-ambiguity`)
    #[error("ConnectorPaginationAmbiguous: {0}")]
    ConnectorPaginationAmbiguous(String),
    /// A URL placeholder with no table pattern to bind it. (`connector.source.placeholder-unbound`)
    #[error("ConnectorPlaceholderUnbound: {0}")]
    ConnectorPlaceholderUnbound(String),
    /// A permitted host name resolving to a private, link-local, loopback or metadata address. (`connector.attach.private-address`)
    #[error("ConnectorPrivateAddress: {0}")]
    ConnectorPrivateAddress(String),
    /// A granted scope outside the manifest's declared expectation. (`connector.declare-capability.scope-exceeded`)
    #[error("ConnectorScopeExceeded: {0}")]
    ConnectorScopeExceeded(String),
    /// A scope probe declaring a scopes header that is empty or not an HTTP field name. (`connector.declare-capability.probe-shape`)
    #[error("ConnectorScopeProbeRejected: {0}")]
    ConnectorScopeProbeRejected(String),
    /// A scope probe response carrying no granted-scopes header, or one naming no scope or holding a byte outside visible ASCII. (`connector.declare-capability.scope-unverified`)
    #[error("ConnectorScopeUnverified: {0}")]
    ConnectorScopeUnverified(String),
    /// A declared limiter quota with no operator binding. (`connector.meter.quota-unbound`)
    #[error("ConnectorQuotaUnbound: {0}")]
    ConnectorQuotaUnbound(String),
    /// A table not matching the source's table pattern. (`connector.source.table-unmatched`)
    /// A table off the source's table pattern, or binding a dot segment. (`connector.source.table-unmatched`)
    #[error("ConnectorTableUnmatched: {0}")]
    ConnectorTableUnmatched(String),
    /// A provider call to a non-TLS transport or a host other than the provider's. (`connector.source.provider-origin`)
    #[error("ConnectorProviderOriginRejected: {0}")]
    ConnectorProviderOriginRejected(String),
    /// Host access a connector reaches for that its manifest does not list. (`connector.declare-capability.undeclared-access`)
    #[error("ConnectorUndeclaredAccess: {0}")]
    ConnectorUndeclaredAccess(String),
    /// An outbound hop the pre-send hook refused before name resolution. (`connector.meter.hook-refusal`)
    #[error("ConnectorEgressRefused: {0}")]
    ConnectorEgressRefused(String),
    /// A vendor request without a granted reservation under a declared limiter. (`connector.meter.unmetered-request`)
    #[error("ConnectorUnmetered: {0}")]
    ConnectorUnmetered(String),
    /// The bootstrap name among the declared lease scopes. (`connector.lease.bootstrap-declared-leased`)
    #[error("SecretBootstrapLeased: {0}")]
    SecretBootstrapLeased(String),
    /// No adapter behind the lease provider answering the bootstrap reference. (`connector.lease.bootstrap-unserved`)
    #[error("SecretBootstrapUnresolved: {0}")]
    SecretBootstrapUnresolved(String),
    /// A sensitive header bound for a cleartext endpoint outside loopback. (`connector.attach.cleartext-endpoint`)
    #[error("SecretCleartextEndpoint: {0}")]
    SecretCleartextEndpoint(String),
    /// A configured endpoint carrying userinfo. (`connector.attach.credential-in-a-url`)
    #[error("SecretCredentialInUrl: {0}")]
    SecretCredentialInUrl(String),
    /// A template carrying `${env://NAME}` or a bare `${NAME}`. (`connector.reference.foreign-placeholder`)
    #[error("SecretForeignPlaceholder: {0}")]
    SecretForeignPlaceholder(String),
    /// The mint endpoint answering a redirect. (`connector.lease.redirect`)
    #[error("SecretLeaseRedirect: {0}")]
    SecretLeaseRedirect(String),
    /// The mint endpoint answering another `4xx`. (`connector.lease.request-rejected`)
    #[error("SecretLeaseRequestRejected: {0}")]
    SecretLeaseRequestRejected(String),
    /// The mint endpoint answering `404` for a declared scope. (`connector.lease.unknown-scope`)
    #[error("SecretLeaseScopeUnknown: {0}")]
    SecretLeaseScopeUnknown(String),
    /// The lease backend selected with no scope declared. (`connector.lease.empty-scope-set`)
    #[error("SecretLeaseScopesEmpty: {0}")]
    SecretLeaseScopesEmpty(String),
    /// An attach value embedding no reference. (`connector.attach.literal-attach-value`)
    #[error("SecretLiteralAttachValue: {0}")]
    SecretLiteralAttachValue(String),
    /// A lease with no expiry, or one already past on arrival. (`connector.lease.malformed-lease`)
    #[error("SecretMalformedLease: {0}")]
    SecretMalformedLease(String),
    /// An unclosed, empty or nested placeholder. (`connector.reference.malformed-placeholder`)
    #[error("SecretMalformedTemplate: {0}")]
    SecretMalformedTemplate(String),
    /// A credential-shaped literal where a reference belongs. (`connector.reference.material-in-a-declaration`)
    #[error("SecretMaterialInDeclaration: {0}")]
    SecretMaterialInDeclaration(String),
    /// The mint endpoint answering `401` or `403`. (`connector.lease.mint-rejected`)
    #[error("SecretMintRejected: {0}")]
    SecretMintRejected(String),
    /// A logical name answered by two assembled adapters. (`connector.resolve.shadowed-name`)
    #[error("SecretNameShadowed: {0}")]
    SecretNameShadowed(String),
    /// A hop or next link weakening the transport or leaving the configured host and port. (`connector.attach.weakened-hop`)
    #[error("SecretRedirectOffOrigin: {0}")]
    SecretRedirectOffOrigin(String),
    /// A request to a host the declaration does not cover. (`connector.attach.unpermitted-request`)
    #[error("SecretUnpermittedRequest: {0}")]
    SecretUnpermittedRequest(String),
    /// A logical name no assembled adapter answers. (`connector.resolve.unresolved-name`)
    #[error("SecretUnresolvedReference: {0}")]
    SecretUnresolvedReference(String),
    /// A wildcard or table-bound endpoint host beside a bound credential. (`connector.attach.bound-host`)
    #[error("SecretWildcardHost: {0}")]
    SecretWildcardHost(String),
}
