//! The refusals of the `run` contract's durable-run operations, one variant per error
//! identifier.
//!
//! A variant's `Display` begins with its identifier, so a surface printing the error
//! names the refusal a caller greps for.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RunError {
    /// A second resolution carrying a different payload. (`run.suspend.conflicting-resolution`)
    #[error("AwakeableAlreadyResolved: {0}")]
    AwakeableAlreadyResolved(String),
    /// A resolution past the suspension's deadline. (`run.suspend.expired-token`)
    #[error("AwakeableTimedOut: {0}")]
    AwakeableTimedOut(String),
    /// A resume token with no registry row. (`run.suspend.unknown-token`)
    #[error("AwakeableUnknown: {0}")]
    AwakeableUnknown(String),
    /// A journal row's blob reference resolves to no file. (`run.journal.missing-blob`)
    #[error("BlobMissing: {0}")]
    BlobMissing(String),
    /// A stop matching no pending, running or waiting run. (`run.cancel.not-in-flight`)
    #[error("CancelTargetNotInFlight: {0}")]
    CancelTargetNotInFlight(String),
    /// A capability the running profile links no implementation for. (`run.journal.unwired-capability`)
    #[error("CapabilityUnwired: {0}")]
    CapabilityUnwired(String),
    /// A stored position's field differs from the declared incremental field. (`run.advance.field-rename`)
    #[error("CursorFieldMismatch: {0}")]
    CursorFieldMismatch(String),
    /// A clock value with no ordering, or a position type changing mid-pass. (`run.advance.unorderable-position`)
    #[error("CursorPositionUnorderable: {0}")]
    CursorPositionUnorderable(String),
    /// A connector identity, component world or plan hash moved under a pending owner. (`run.own.pinned-plan-changed`)
    #[error("ExecutionPinMismatch: {0}")]
    ExecutionPinMismatch(String),
    /// A history window bound outside the two accepted spellings. (`run.record.bound-spelling`)
    #[error("HistoryBoundSpelling: {0}")]
    HistoryBoundSpelling(String),
    /// An input outside a shape the contract bounds without naming a refusal.
    #[error("invalid: {0}")]
    Invalid(String),
    /// Write-path redaction declared over a source whose pulls are journaled. (`run.journal.redacting-source`)
    #[error("JournalRedactionConflict: {0}")]
    JournalRedactionConflict(String),
    /// A snapshot metadata write past its serialized bound. (`run.project.metadata-too-large`)
    #[error("RunMetadataTooLarge: {0}")]
    RunMetadataTooLarge(String),
    /// A `retry_on` naming a tag outside the two retryable ones. (`run.retry.unretryable-tag`)
    #[error("RunRetryTagUnretryable: {0}")]
    RunRetryTagUnretryable(String),
    /// A run-stream request with no valid credential before the upgrade. (`run.project.unauthenticated-upgrade`)
    #[error("RunStreamUnauthorized: {0}")]
    RunStreamUnauthorized(String),
    /// A site id with no bound source, or two. (`run.record.site-id-unresolved`)
    #[error("SiteIdUnresolved: {0}")]
    SiteIdUnresolved(String),
    /// A step closed on a terminal failure, carrying its label and tag. (`run.retry.step-failed`)
    #[error("StepFailed: step `{label}` closed on {failure}")]
    StepFailed { label: String, failure: super::failure::Failure },
}
