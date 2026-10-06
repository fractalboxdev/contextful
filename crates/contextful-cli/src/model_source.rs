//! The local statement file a declared model reads before validation or build.

use contextful_core::pipeline::model::ModelSpec;
use contextful_core::run::RunError;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum StatementFault {
    #[error(transparent)]
    Declaration(#[from] RunError),
    #[error("statement file `{path}` does not read: {source}")]
    Unreadable { path: String, source: std::io::Error },
}

/// Resolve a local `sql_file` beside the manifest holding the model. The resolved
/// statement takes the same SQL admission path as an inline declaration.
pub fn resolve(spec: &mut ModelSpec, manifest: &str) -> Result<(), StatementFault> {
    spec.validate_statement_source()?;
    if let Some(file) = spec.sql_file.take() {
        let path = Path::new(manifest).parent().unwrap_or_else(|| Path::new(".")).join(file);
        spec.sql = std::fs::read_to_string(&path).map_err(|source| StatementFault::Unreadable { path: path.display().to_string(), source })?;
    }
    Ok(())
}
