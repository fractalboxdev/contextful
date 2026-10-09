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


/// Resolve and validate model declarations before immutable control publication.
pub(crate) fn collect(files: &[contextful_core::pipeline::declare::ManifestFile]) -> anyhow::Result<Vec<ModelSpec>> {
    let pipelines = contextful_core::pipeline::declare::collect(files)?;
    let mut models = Vec::new();
    for model in contextful_core::pipeline::model::collect_models(files, &pipelines)? {
        let mut spec = model.spec;
        resolve(&mut spec, &model.file)?;
        spec.validate()?;
        contextful_context::build::admit_statements(&spec, |_| None).map_err(|(what, e)| anyhow::anyhow!("{what}: {e}"))?;
        models.push(spec);
    }
    models.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(models)
}

/// Applied models must be self contained, never resolving a path from a live draft.
pub(crate) fn applied(file: &contextful_core::pipeline::declare::ManifestFile) -> anyhow::Result<Vec<ModelSpec>> {
    let models = contextful_core::pipeline::model::collect_models(std::slice::from_ref(file), &contextful_core::pipeline::declare::collect(std::slice::from_ref(file))?)?;
    if models.iter().any(|m| m.spec.sql_file.is_some()) { anyhow::bail!("applied model SQL must be inline"); }
    collect(std::slice::from_ref(file))
}
