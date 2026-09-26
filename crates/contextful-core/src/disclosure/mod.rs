//! Source-faithful visibility's pure domain: the visibility block a table declares, the
//! fidelity level it claims, the source family bounding that claim, and the staleness
//! budget on its mirrored permission state.

pub mod declare;
pub mod error;

pub use error::VisibilityError;
