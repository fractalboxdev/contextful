//! The refusals of the `connector` contract the engine raises, one variant per error
//! identifier. A variant's `Display` begins with its identifier.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConnectorError {
    /// An allowlist that is empty or bare-wildcard, or an entry carrying a scheme, port or path. (`connector.declare-capability.allowlist-shape`)
    #[error("ConnectorAllowlistRejected: {0}")]
    ConnectorAllowlistRejected(String),
    /// An environment binding not supplied, or supplied off its declared shape. (`connector.declare-capability.binding-unbound`)
    #[error("ConnectorBindingUnbound: {0}")]
    ConnectorBindingUnbound(String),
    /// An environment binding on a key the connector does not read. (`connector.declare-capability.binding-unsupported`)
    #[error("ConnectorBindingUnsupported: {0}")]
    ConnectorBindingUnsupported(String),
    /// A record path, pagination shape or decode key the chosen format does not read. (`connector.source.format-key-mismatch`)
    #[error("ConnectorFormatKeyRejected: {0}")]
    ConnectorFormatKeyRejected(String),
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
    /// A scope probe response carrying no granted-scopes header. (`connector.declare-capability.scope-unverified`)
    #[error("ConnectorScopeUnverified: {0}")]
    ConnectorScopeUnverified(String),
    /// A table not matching the source's table pattern. (`connector.source.table-unmatched`)
    #[error("ConnectorTableUnmatched: {0}")]
    ConnectorTableUnmatched(String),
    /// Host access a connector reaches for that its manifest does not list. (`connector.declare-capability.undeclared-access`)
    #[error("ConnectorUndeclaredAccess: {0}")]
    ConnectorUndeclaredAccess(String),
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
    /// A wildcard host entry beside a bound credential. (`connector.attach.bound-host`)
    #[error("SecretWildcardHost: {0}")]
    SecretWildcardHost(String),
}
