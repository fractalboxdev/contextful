//! The refusals of the `read` contract, one variant per error identifier, and the one
//! refusal a read surface reports, whichever contract raised it.
//!
//! A variant's `Display` begins with its identifier, so a surface printing the error
//! names the refusal a caller greps for.

use crate::enforce::EnforceError;
use crate::disclosure::DisclosureError;
use crate::memory::MemoryError;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReadError {
    /// The statement exceeded its selected execution deadline. (`read.respond.duration-ceiling`)
    #[error("ReadDurationExceeded: {0}")]
    ReadDurationExceeded(String),
    /// The response envelope or first row exceeds the selected byte ceiling. (`read.respond.byte-ceiling`)
    #[error("ReadResponseTooLarge: {0}")]
    ReadResponseTooLarge(String),
    /// A previewed path resolved to no table. (`read.register.file-preview-target`)
    #[error("FilePreviewNotATable: {0}")]
    FilePreviewNotATable(String),
    /// A ranked read's filter exceeded its value, condition or byte budget. (`read.retrieve.filter-budget-refusal`)
    #[error("FilterBudgetExceeded: {0}")]
    FilterBudgetExceeded(String),
    /// A tenant-scoped token named the request-ledger child relation. (`read.register.scoped-ledger`)
    #[error("LedgerNotTenantScoped: {0}")]
    LedgerNotTenantScoped(String),
    /// A pinned build identifier is unknown or collected. (`read.resolve-pin.unknown-build`)
    #[error("PinnedBuildUnavailable: {0}")]
    PinnedBuildUnavailable(String),
    /// A client required a face this binary did not link. (`read.embed.required-face`)
    #[error("RequiredFaceAbsent: {0}")]
    RequiredFaceAbsent(String),
    /// A read tool was called on a binary without the embedded SQL engine. (`read.embed.absent-read-backend`)
    #[error("ReadBackendAbsent: {0}")]
    ReadBackendAbsent(String),
    /// A query parameter was missing, unused, untyped or mistyped. (`read.guard.query-binding`)
    #[error("QueryParameterRejected: {0}")]
    QueryParameterRejected(String),
    /// A `--project` named no store on disk. (`read.query.project-store`)
    #[error("QueryProjectAbsent: {0}")]
    QueryProjectAbsent(String),
    /// Operator text held other than exactly one statement. (`read.query.one-statement`)
    #[error("QueryNotOneStatement: {0}")]
    QueryNotOneStatement(String),
    /// Admitted text did not parse to exactly one read-only SELECT. (`read.guard.single-read-only-statement`)
    #[error("StatementNotReadOnly: {0}")]
    StatementNotReadOnly(String),
    /// A table function or a schema-qualified catalog reach appeared in caller text. (`read.guard.table-function`)
    #[error("TableFunctionRefused: {0}")]
    TableFunctionRefused(String),
    /// A template argument was missing, unknown or mistyped. (`read.guard.template-binding`)
    #[error("TemplateArgumentRejected: {0}")]
    TemplateArgumentRejected(String),
    /// A template named a non-store relation or collided with a built-in tool prefix. (`read.guard.template-relation-shape`)
    #[error("TemplateNamesForeignRelation: {0}")]
    TemplateNamesForeignRelation(String),
    /// The network transport started with no audience or no positive in-flight ceiling. (`read.register.serve-declaration`)
    #[error("ServeDeclarationMissing: {0}")]
    ServeDeclarationMissing(String),
    /// A network transport request carried no credential. (`read.register.credential-missing`)
    #[error("HttpCredentialMissing: {0}")]
    HttpCredentialMissing(String),
    /// A read reached for a query-engine extension the binary does not link. (`assurance.build.runtime-extension-load`)
    #[error("ExtensionAutoloadRefused: {0}")]
    ExtensionAutoloadRefused(String),
}

impl ReadError {
    /// The error identifier.
    pub fn identifier(&self) -> &'static str {
        match self {
            ReadError::ReadDurationExceeded(_) => "ReadDurationExceeded",
            ReadError::ReadResponseTooLarge(_) => "ReadResponseTooLarge",
            ReadError::FilePreviewNotATable(_) => "FilePreviewNotATable",
            ReadError::FilterBudgetExceeded(_) => "FilterBudgetExceeded",
            ReadError::LedgerNotTenantScoped(_) => "LedgerNotTenantScoped",
            ReadError::PinnedBuildUnavailable(_) => "PinnedBuildUnavailable",
            ReadError::QueryParameterRejected(_) => "QueryParameterRejected",
            ReadError::RequiredFaceAbsent(_) => "RequiredFaceAbsent",
            ReadError::ReadBackendAbsent(_) => "ReadBackendAbsent",
            ReadError::QueryProjectAbsent(_) => "QueryProjectAbsent",
            ReadError::QueryNotOneStatement(_) => "QueryNotOneStatement",
            ReadError::StatementNotReadOnly(_) => "StatementNotReadOnly",
            ReadError::TableFunctionRefused(_) => "TableFunctionRefused",
            ReadError::TemplateArgumentRejected(_) => "TemplateArgumentRejected",
            ReadError::TemplateNamesForeignRelation(_) => "TemplateNamesForeignRelation",
            ReadError::ServeDeclarationMissing(_) => "ServeDeclarationMissing",
            ReadError::HttpCredentialMissing(_) => "HttpCredentialMissing",
            ReadError::ExtensionAutoloadRefused(_) => "ExtensionAutoloadRefused",
        }
    }
}

/// A refusal a read surface reports: a read-face, enforcement or memory refusal.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error(transparent)]
    Disclosure(#[from] DisclosureError),
    #[error(transparent)]
    Read(#[from] ReadError),
    #[error(transparent)]
    Enforce(#[from] EnforceError),
    #[error(transparent)]
    Memory(#[from] MemoryError),
}

impl Refusal {
    /// The error identifier.
    pub fn identifier(&self) -> &'static str {
        match self {
            Refusal::Disclosure(e) => e.identifier(),
            Refusal::Read(e) => e.identifier(),
            Refusal::Enforce(e) => e.identifier(),
            Refusal::Memory(e) => e.identifier(),
        }
    }
}
