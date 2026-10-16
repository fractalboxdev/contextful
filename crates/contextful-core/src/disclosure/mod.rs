//! The `disclosure` contract's pure domain.
//!
//! Source-faithful visibility: the visibility block a table declares, the fidelity level
//! it claims, the source family bounding that claim, and the staleness budget on its
//! mirrored permission state. Aggregate disclosure: the per-unit budget a release reserves,
//! the exact noise it draws, which groups of a derived result publish, and the one signal a
//! withheld group leaves, and the cross-owner hashed join under a per-pair pepper.

pub mod declare;
pub mod error;
pub mod erase;
pub mod join;
pub mod noise;
pub mod pack;
pub mod reach;
pub mod release;
pub mod stale;
pub mod suppress;
pub mod sweep;

pub use error::{DisclosureError, VisibilityError};
