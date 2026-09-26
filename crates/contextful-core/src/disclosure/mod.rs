//! The `disclosure` contract's pure domain.
//!
//! Source-faithful visibility: the visibility block a table declares, the fidelity level
//! it claims, the source family bounding that claim, and the staleness budget on its
//! mirrored permission state. Aggregate disclosure: which groups of a derived result
//! publish, and the one signal a withheld group leaves.

pub mod declare;
pub mod error;
pub mod suppress;

pub use error::{DisclosureError, VisibilityError};
