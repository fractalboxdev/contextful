//! The refusals of the `assurance` contract a linked binary raises at run time, one
//! variant per error identifier.
//!
//! A variant's `Display` begins with its identifier, so a surface printing the error
//! names the refusal a caller greps for.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AssuranceError {
    /// A read path loading a query-engine extension into a statically linked binary. (`assurance.build.runtime-extension-load`)
    #[error("ExtensionAutoloadRefused: {0}")]
    ExtensionAutoloadRefused(String),
}

impl AssuranceError {
    /// The error identifier.
    pub fn identifier(&self) -> &'static str {
        match self {
            AssuranceError::ExtensionAutoloadRefused(_) => "ExtensionAutoloadRefused",
        }
    }
}
