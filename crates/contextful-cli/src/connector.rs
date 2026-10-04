//! Connector artifact inspection through an explicit local pin command.

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use contextful_core::connector::package::Digest;
use contextful_core::connector::attach::Allowlist;
use contextful_core::connector::component::ComponentSource;
use contextful_core::connector::ConnectorError;
use std::path::{Component, Path, PathBuf};

#[derive(Subcommand)]
pub enum ConnectorCmd {
    /// Print the digest of a host-local component artifact without changing its manifest.
    Pin {
        manifest: PathBuf,
        #[arg(long)]
        local: bool,
    },
}

pub fn run(command: ConnectorCmd) -> Result<()> {
    match command {
        ConnectorCmd::Pin { manifest, local } => {
            if !local {
                bail!("connector pin requires --local until a fixed-platform builder is configured");
            }
            let source = std::fs::read_to_string(&manifest).with_context(|| format!("reading connector manifest `{}`", manifest.display()))?;
            let value: toml::Value = toml::from_str(&source).with_context(|| format!("parsing connector manifest `{}`", manifest.display()))?;
            let wasm = value.get("wasm").and_then(toml::Value::as_str).context("connector manifest requires a `wasm` path")?;
            let path = manifest.parent().unwrap_or_else(|| std::path::Path::new(".")).join(wasm);
            let bytes = std::fs::read(&path).with_context(|| format!("reading connector artifact `{}`", path.display()))?;
            println!("{}", Digest::of(&bytes));
            Ok(())
        }
    }
}

/// The manifest limits the operator's component grant before the artifact loads.
pub fn admit_hosts(decl: &ComponentSource, base: &Path) -> Result<()> {
    let Some(raw) = &decl.manifest else { return Ok(()); };
    let relative = Path::new(raw);
    if relative.components().any(|part| !matches!(part, Component::Normal(_))) {
        bail!("component manifest is a project-relative file: `{raw}`");
    }
    let path = base.join(relative);
    let root = std::fs::canonicalize(base).context("reading project directory")?;
    let resolved = std::fs::canonicalize(&path).with_context(|| format!("reading connector manifest `{raw}`"))?;
    if !resolved.starts_with(&root) {
        bail!("component manifest is a project-relative file: `{raw}`");
    }
    let source = std::fs::read_to_string(&resolved).with_context(|| format!("reading connector manifest `{raw}`"))?;
    let value: toml::Value = toml::from_str(&source).with_context(|| format!("parsing connector manifest `{raw}`"))?;
    let hosts = value.get("capabilities").and_then(|v| v.get("allow_hosts"));
    let declared = match hosts {
        None => Allowlist(Vec::new()),
        Some(toml::Value::Array(items)) => {
            let entries = items.iter().map(|item| item.as_str().context("manifest `allow_hosts` holds host strings")).collect::<Result<Vec<_>>>()?;
            Allowlist::parse(&entries)?
        }
        Some(_) => bail!("manifest `allow_hosts` is a list of hosts"),
    };
    if !decl.allow.included_in(&declared) {
        let witness = decl.allow.widening_witness(&declared).unwrap_or_else(|| "an unresolved host".to_string());
        return Err(ConnectorError::ConnectorUndeclaredAccess(format!("host `{witness}` is absent from manifest `{raw}`")).into());
    }
    Ok(())
}
