//! The operator plane's pure rules: the schedule grammar and an entry's next fire
//! (`surface.arm`), the snapshot pointer (`surface.reconcile`), and pool admission
//! (`surface.dispatch`).

pub mod arm;
pub mod control;
pub mod dispatch;

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
    /// (`surface.fire.cycle-control-source`)
    #[error("CycleControlSourceUnresolved: {0}")]
    CycleControlSourceUnresolved(String),
    /// (`surface.apply.version-race`)
    #[error("ManifestVersionConflict: {0}")]
    ManifestVersionConflict(String),
    /// (`surface.apply.validation`)
    #[error("ApplyValidationRefused: {0}")]
    ApplyValidationRefused(String),
}
