//! The project root the issuance policy, the default issuer seed and the key-set ledger sit under.

use anyhow::Result;
use std::path::PathBuf;

/// The declaration file whose directory marks a project root.
const DECLARATION_FILE: &str = "contextful.toml";

/// The directory the issuance policy, the default issuer seed and the key-set ledger sit
/// under (`authority.issue.project-root`): the working directory under `--project`, else
/// the nearest directory upward holding `contextful.toml`, else the working directory.
pub fn root(project: Option<&str>) -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    if project.is_some() {
        return Ok(cwd);
    }
    let found = cwd.ancestors().find(|d| d.join(DECLARATION_FILE).is_file()).map(PathBuf::from);
    Ok(found.unwrap_or(cwd))
}
