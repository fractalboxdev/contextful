//! The refusals of the `read` contract, one variant per error identifier, and the one
//! refusal a read surface reports, whichever contract raised it.
//!
//! A variant's `Display` begins with its identifier, so a surface printing the error
//! names the refusal a caller greps for.

use crate::enforce::EnforceError;
use crate::memory::MemoryError;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReadError {
    /// A previewed path resolved to no table. (`read.register.file-preview-target`)
    #[error("FilePreviewNotATable: {0}")]
    FilePreviewNotATable(String),
    /// A ranked read's filter exceeded its value, condition or byte budget. (`read.retrieve.filter-budget-refusal`)
    #[error("FilterBudgetExceeded: {0}")]
    FilterBudgetExceeded(String),
    /// A client required a face this binary did not link. (`read.embed.required-face`)
    #[error("RequiredFaceAbsent: {0}")]
    RequiredFaceAbsent(String),
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
}

impl ReadError {
    /// The error identifier.
    pub fn identifier(&self) -> &'static str {
        match self {
            ReadError::FilePreviewNotATable(_) => "FilePreviewNotATable",
            ReadError::FilterBudgetExceeded(_) => "FilterBudgetExceeded",
            ReadError::RequiredFaceAbsent(_) => "RequiredFaceAbsent",
            ReadError::StatementNotReadOnly(_) => "StatementNotReadOnly",
            ReadError::TableFunctionRefused(_) => "TableFunctionRefused",
            ReadError::TemplateArgumentRejected(_) => "TemplateArgumentRejected",
            ReadError::TemplateNamesForeignRelation(_) => "TemplateNamesForeignRelation",
        }
    }
}

/// A refusal a read surface reports: a read-face, enforcement or memory refusal.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
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
            Refusal::Read(e) => e.identifier(),
            Refusal::Enforce(e) => e.identifier(),
            Refusal::Memory(e) => e.identifier(),
        }
    }
}
