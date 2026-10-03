//! The pipeline declaration and the batch stages around the landing: the specification,
//! its canonical hash, the normalize stage, the transform chain, the secret guard, the seed ceiling, and the
//! `[[model]]` block with the publication a build commits.

pub mod canonical;
pub mod declare;
pub mod guard;
pub mod model;
pub mod normalize;
pub mod seed;
pub mod transform;
