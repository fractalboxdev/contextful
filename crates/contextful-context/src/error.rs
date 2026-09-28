//! What a store operation refuses with: a registered store refusal, or an I/O, Parquet
//! or input fault naming its file.

use contextful_core::store::StoreError;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: std::io::Error },
    #[error("{}: parquet: {message}", path.display())]
    Parquet { path: PathBuf, message: String },
    /// A column read as row values whose type no row value represents.
    #[error("column `{column}` holds {data_type}, which a row value does not represent")]
    ColumnType { column: String, data_type: String },
    /// A batch, request or configuration that fails validation before anything lands.
    #[error("{0}")]
    Invalid(String),
    /// A catalog port reporting a storage fault.
    #[error("{0}")]
    Catalog(contextful_core::run::Failure),
}

impl ContextError {
    pub fn store(&self) -> Option<&StoreError> {
        match self {
            ContextError::Store(e) => Some(e),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, ContextError>;

/// Attach a path to an I/O result.
pub(crate) trait IoPath<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T> IoPath<T> for std::io::Result<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|source| ContextError::Io { path: path.into(), source })
    }
}
