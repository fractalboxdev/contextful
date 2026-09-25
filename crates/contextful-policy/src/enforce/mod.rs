//! The query-time layer of the reference monitor: table policies, row predicates,
//! column masks, inference zones, the scope guard, refusal payloads, and the session
//! value every row-returning read takes.

pub mod mask;
pub mod policy;
pub mod predicate;
pub mod refuse;
pub mod scope;
pub mod session;
pub mod zone;

use contextful_core::enforce::EnforceError;
use contextful_core::store::declare::DeclarationMalformed;
use contextful_core::AuthorityError;

/// An enforcement refusal, an authority refusal met while compiling a relation, or a
/// declaration that does not parse or runs past a bound.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error(transparent)]
    Enforce(#[from] EnforceError),
    #[error(transparent)]
    Authority(#[from] AuthorityError),
    #[error(transparent)]
    Malformed(#[from] DeclarationMalformed),
}
