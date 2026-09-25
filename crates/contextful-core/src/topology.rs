//! The refusals of the `topology` contract a running binary raises.

/// One variant per error identifier; `Display` begins with it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TopologyError {
    /// A component connector is dispatched on a profile linking no component host. (`topology.package.host-missing`)
    #[error("ComponentHostMissing: {0}")]
    ComponentHostMissing(String),
}
