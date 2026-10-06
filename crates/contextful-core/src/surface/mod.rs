//! The operator plane's pure rules: the schedule grammar, the trigger and an entry's next
//! fire (`surface.arm`), the snapshot pointer (`surface.reconcile`), pool admission
//! (`surface.dispatch`), the control document's refusals (`surface.edit`) and the residency
//! allow-set (`surface.reside`).

pub mod arm;
pub mod control;
pub mod dispatch;
pub mod edit;
pub mod reside;
pub mod worker;

/// The operator plane's refusals. `Display` begins with the identifier.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SurfaceError {
    /// (`surface.arm.unreadable-schedule`)
    #[error("ScheduleUnreadable: {0}")]
    ScheduleUnreadable(String),
    /// (`surface.reconcile.pointer-malformed`)
    #[error("ControlPointerMalformed: {0}")]
    ControlPointerMalformed(String),
    /// (`surface.reconcile.fail-static`)
    #[error("ControlSnapshotUnreadable: {0}")]
    ControlSnapshotUnreadable(String),
    /// (`surface.reconcile.pulled-control-untrusted`)
    #[error("ControlSnapshotUntrusted: {0}")]
    ControlSnapshotUntrusted(String),
    /// (`surface.reconcile.loopback-only`)
    #[error("ControlSourceNotLoopback: {0}")]
    ControlSourceNotLoopback(String),
    /// (`surface.apply.owner-unconfigured`)
    #[error("ConfigOwnerUnconfigured: {0}")]
    ConfigOwnerUnconfigured(String),
    /// (`surface.fire.cycle-control-source`)
    #[error("CycleControlSourceUnresolved: {0}")]
    CycleControlSourceUnresolved(String),
    /// (`surface.apply.version-race`)
    #[error("ManifestVersionConflict: {0}")]
    ManifestVersionConflict(String),
    /// (`surface.apply.validation`)
    #[error("ApplyValidationRefused: {0}")]
    ApplyValidationRefused(String),
    /// (`surface.apply.attestation-unavailable`)
    #[error("ControlAttestationUnavailable: {0}")]
    ControlAttestationUnavailable(String),
    /// (`surface.arm.unknown-trigger`)
    #[error("TriggerAdapterUnknown: {0}")]
    TriggerAdapterUnknown(String),
    /// (`surface.arm.trigger-face-missing`)
    #[error("TriggerFaceMissing: {0}")]
    TriggerFaceMissing(String),
    /// (`surface.edit.secret-in-document`)
    #[error("SecretMaterialInDocument: {0}")]
    SecretMaterialInDocument(String),
    /// (`surface.edit.connector-upload`)
    #[error("ConnectorUploadRefused: {0}")]
    ConnectorUploadRefused(String),
    /// (`surface.apply.weak-conditional-backend`)
    #[error("ConditionalWriteUnsupported: {0}")]
    ConditionalWriteUnsupported(String),
    /// (`surface.apply.uninitialized-store`)
    #[error("StoreNotInitialized: {0}")]
    StoreNotInitialized(String),
    /// (`surface.reside.region-mismatch`)
    #[error("EnforceRegionMismatch: {0}")]
    EnforceRegionMismatch(String),
    /// (`surface.reside.site-regions`)
    #[error("ResidencySitesDiverge: {0}")]
    ResidencySitesDiverge(String),
    /// (`surface.dispatch.not-a-head`)
    #[error("DispatchUnitNotAHead: {0}")]
    DispatchUnitNotAHead(String),
    /// (`surface.dispatch.step-result`)
    #[error("StepResultRefused: {0}")]
    StepResultRefused(String),
    /// (`surface.dispatch.in-band-tool-error`)
    #[error("StepToolError: {0}")]
    StepToolError(String),
    /// (`surface.dispatch.callback-rejected`)
    #[error("DispatchCallbackRejected: {0}")]
    DispatchCallbackRejected(String),
    /// (`surface.dispatch.submit-signed`)
    #[error("DispatchSubmitRejected: {0}")]
    DispatchSubmitRejected(String),
    /// A `[control]` or `[residency]` block that does not parse, or a bound it exceeds.
    #[error("{0}")]
    Invalid(String),
}

impl SurfaceError {
    /// The HTTP status an operator route answers the refusal with: `503` for an owner with
    /// nothing behind it (`surface.apply.owner-unconfigured`) or a control source it cannot
    /// read, `409` for a store taking no guarded import (`surface.apply.uninitialized-store`)
    /// or a lost claim, `401` for a worker submission it cannot authenticate, and `422` for a document the plane refuses.
    pub fn status(&self) -> u16 {
        match self {
            SurfaceError::ConfigOwnerUnconfigured(_)
            | SurfaceError::ControlSnapshotUnreadable(_)
            | SurfaceError::ControlPointerMalformed(_)
            | SurfaceError::ControlSourceNotLoopback(_)
            | SurfaceError::CycleControlSourceUnresolved(_)
            | SurfaceError::ConditionalWriteUnsupported(_)
            | SurfaceError::ControlAttestationUnavailable(_) => 503,
            SurfaceError::StoreNotInitialized(_) | SurfaceError::ManifestVersionConflict(_) | SurfaceError::DispatchCallbackRejected(_) => 409,
            SurfaceError::EnforceRegionMismatch(_) | SurfaceError::ResidencySitesDiverge(_) => 503,
            SurfaceError::DispatchSubmitRejected(_) => 401,
            SurfaceError::ScheduleUnreadable(_)
            | SurfaceError::ControlSnapshotUntrusted(_)
            | SurfaceError::ApplyValidationRefused(_)
            | SurfaceError::TriggerAdapterUnknown(_)
            | SurfaceError::TriggerFaceMissing(_)
            | SurfaceError::SecretMaterialInDocument(_)
            | SurfaceError::ConnectorUploadRefused(_)
            | SurfaceError::DispatchUnitNotAHead(_)
            | SurfaceError::StepResultRefused(_)
            | SurfaceError::StepToolError(_)
            | SurfaceError::Invalid(_) => 422,
        }
    }
}
