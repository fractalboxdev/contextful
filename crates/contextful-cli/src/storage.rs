//! Complete storage profiles validate before execution admission.

use crate::run::{ProjectArgs, CATALOG_URL_VAR};
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use contextful_context::project::Project;
use contextful_core::store::{config::StoreConfig, sync::Endpoint};
use serde_json::json;

pub const STORAGE_PROFILE_VAR: &str = "CONTEXTFUL_STORAGE_PROFILE";
pub const RECOVERY_ID_VAR: &str = "CONTEXTFUL_RECOVERY_ID";
pub const CLOUDFLARE_POSTGRES: &str = "cloudflare-postgres";

#[derive(Subcommand)]
pub enum StorageCmd {
    /// Validate complete execution storage and print its capability report as JSON.
    Check {
        #[command(flatten)]
        project: ProjectArgs,
    },
    /// Bind a fresh remote execution database to this project's recovery identity.
    Initialize {
        #[command(flatten)]
        project: ProjectArgs,
    },
}

#[cfg_attr(not(feature = "pg-catalog"), allow(dead_code))]
pub(crate) struct Remote {
    pub(crate) url: String,
    pub(crate) recovery: String,
}

fn variable(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.trim().is_empty())
}

/// Validate environment bindings before a command opens any project or dispatches work.
pub fn validate_environment() -> Result<bool> {
    let profile = variable(STORAGE_PROFILE_VAR).unwrap_or_else(|| "local".into());
    match profile.as_str() {
        "local" => Ok(false),
        CLOUDFLARE_POSTGRES => {
            for (key, capability) in [(CATALOG_URL_VAR, "catalog, execution owners and cadence"), (RECOVERY_ID_VAR, "whole-state recovery identity")] {
                if variable(key).is_none() { bail!("StorageCapabilityMissing: `{profile}` requires `{key}` for {capability}") }
            }
            Ok(true)
        }
        _ => bail!("StorageProfileUnsupported: `{profile}`; supported profiles are `local` and `{CLOUDFLARE_POSTGRES}`"),
    }
}

fn remote() -> Result<Option<Remote>> {
    if !validate_environment()? {
        return Ok(None);
    }
    if !cfg!(feature = "pg-catalog") {
        return Err(crate::ProfileError::ProfileCapabilityAbsent(format!("storage profile `{CLOUDFLARE_POSTGRES}` requires `pg-catalog` for journal, blobs, awakeables, owners, cadence and exports; build profile is `{}`", crate::PROFILE)).into());
    }
    if !cfg!(feature = "s3-sync") {
        return Err(crate::ProfileError::ProfileCapabilityAbsent(format!("storage profile `{CLOUDFLARE_POSTGRES}` requires `s3-sync` for durable corpus objects; build profile is `{}`", crate::PROFILE)).into());
    }
    Ok(Some(Remote {
        url: variable(CATALOG_URL_VAR).context("catalog binding")?,
        recovery: variable(RECOVERY_ID_VAR).context("recovery binding")?,
    }))
}

fn validate_project(project: &Project) -> Result<()> {
    for manifest in contextful_context::project::manifests(&project.declaration())? {
        let value: toml::Value = toml::from_str(&manifest.text)?;
        if value
            .get("publication")
            .and_then(toml::Value::as_array)
            .is_some_and(|groups| !groups.is_empty())
        {
            bail!("StorageCapabilityMissing: `{CLOUDFLARE_POSTGRES}` has no coordinated-group-outbox adapter")
        }
    }
    let path = project
        .store_root()
        .join(contextful_core::store::lay_out::CONFIG_FILE);
    let text = std::fs::read_to_string(&path)
        .context("StorageCapabilityMissing: durable corpus `[sync]` configuration is required")?;
    let config: StoreConfig = toml::from_str(&text).context("durable storage configuration")?;
    if config.encryption.is_some() {
        bail!(
            "StorageProfileUnsupported: the PostgreSQL execution profile has no sealed-row adapter"
        )
    }
    let sync = config
        .sync
        .context("StorageCapabilityMissing: durable corpus `[sync]` binding is required")?;
    if !matches!(sync.resolve_endpoint()?, Endpoint::S3 { .. }) || sync.bucket.is_empty() {
        bail!("StorageCapabilityMissing: a remote S3-compatible corpus bucket is required")
    }
    if sync.pull_before_run != Some(true) || sync.push_after_run != Some(true) {
        bail!("StorageCapabilityMissing: disposable disks require `[sync] pull_before_run = true` and `push_after_run = true`")
    }
    crate::sync::bucket(&sync)?;
    Ok(())
}

pub(crate) fn preflight(project: &Project) -> Result<Option<Remote>> {
    let Some(remote) = remote()? else {
        return Ok(None);
    };
    validate_project(project)?;
    recovery(project, &remote, false)?;
    Ok(Some(remote))
}

fn recovery(project: &Project, remote: &Remote, initialize: bool) -> Result<i64> {
    #[cfg(feature = "pg-catalog")]
    {
        if initialize {
            contextful_pg::PgRunStores::initialize_recovery(
                &remote.url,
                &project.name,
                &remote.recovery,
            )?;
        } else {
            contextful_pg::PgRunStores::verify_recovery(
                &remote.url,
                &project.name,
                &remote.recovery,
            )?;
        }
        Ok(contextful_pg::EXECUTION_SCHEMA_VERSION)
    }
    #[cfg(not(feature = "pg-catalog"))]
    {
        let _ = (project, remote, initialize);
        bail!("ProfileCapabilityAbsent: durable recovery requires `pg-catalog`")
    }
}

pub fn run(command: StorageCmd) -> Result<()> {
    let (args, initialize) = match command {
        StorageCmd::Check { project } => (project, false),
        StorageCmd::Initialize { project } => (project, true),
    };
    let Some(remote) = remote()? else {
        if initialize {
            bail!("StorageProfileUnsupported: initialization requires `{CLOUDFLARE_POSTGRES}`")
        }
        println!(
            "{}",
            json!({ "profile":"local", "replacement_safe":false, "capabilities":["journal", "blobs", "awakeables", "owners", "cadence", "exports"] })
        );
        return Ok(());
    };
    let project = args.locate(None)?.project;
    validate_project(&project)?;
    let schema = recovery(&project, &remote, initialize)?;
    println!(
        "{}",
        json!({ "profile":CLOUDFLARE_POSTGRES, "replacement_safe":true, "schema":schema, "project":project.name,
        "capabilities":["journal", "blobs", "awakeables", "owners", "cadence", "exports", "fenced-dispatch", "recovery-identity"] })
    );
    Ok(())
}
