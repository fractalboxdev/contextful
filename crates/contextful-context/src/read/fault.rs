//! What a read returns in place of a response: a typed refusal, or a fault of the store,
//! the policy declarations or the engine.

use crate::error::ContextError;
use contextful_core::enforce::EnforceError;
use contextful_core::read::{ReadError, Refusal};
use contextful_core::AuthorityError;
use contextful_policy::enforce::PolicyError;

#[derive(Debug, thiserror::Error)]
pub enum ReadFault {
    /// A refusal a caller branches on.
    #[error(transparent)]
    Refused(Refusal),
    #[error(transparent)]
    Authority(AuthorityError),
    /// A policy declaration that does not parse or runs past a bound.
    #[error(transparent)]
    Policy(PolicyError),
    #[error(transparent)]
    Store(#[from] ContextError),
    #[error("the SQL engine: {0}")]
    Engine(String),
}

impl ReadFault {
    /// The refusal, where this fault is one.
    pub fn refusal(&self) -> Option<&Refusal> {
        match self {
            ReadFault::Refused(r) => Some(r),
            _ => None,
        }
    }
}

impl From<Refusal> for ReadFault {
    fn from(r: Refusal) -> Self {
        ReadFault::Refused(r)
    }
}

impl From<ReadError> for ReadFault {
    fn from(e: ReadError) -> Self {
        ReadFault::Refused(e.into())
    }
}

impl From<EnforceError> for ReadFault {
    fn from(e: EnforceError) -> Self {
        ReadFault::Refused(e.into())
    }
}

impl From<AuthorityError> for ReadFault {
    fn from(e: AuthorityError) -> Self {
        ReadFault::Authority(e)
    }
}

impl From<PolicyError> for ReadFault {
    fn from(e: PolicyError) -> Self {
        match e {
            PolicyError::Enforce(e) => e.into(),
            PolicyError::Authority(e) => e.into(),
            other => ReadFault::Policy(other),
        }
    }
}

impl From<contextful_core::store::StoreError> for ReadFault {
    fn from(e: contextful_core::store::StoreError) -> Self {
        ReadFault::Store(e.into())
    }
}
