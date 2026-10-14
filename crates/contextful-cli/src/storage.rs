//! Complete storage profiles validate before execution admission.

use crate::run::{ProjectArgs, CATALOG_URL_VAR};
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use contextful_context::project::Project;
use contextful_core::store::{config::StoreConfig, sync::Endpoint};
use serde_json::json;
#[cfg(feature = "d1-catalog")]
use sha2::{Digest, Sha256};

pub const STORAGE_PROFILE_VAR: &str = "CONTEXTFUL_STORAGE_PROFILE";
pub const RECOVERY_ID_VAR: &str = "CONTEXTFUL_RECOVERY_ID";
pub const POSTGRES: &str = "postgres";
pub const CLOUDFLARE_D1: &str = "cloudflare-d1";
pub const D1_ENDPOINT_VAR: &str = "CONTEXTFUL_D1_ENDPOINT";

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
    /// Record the quiesced R2 corpus frontier in D1 before exporting the database.
    Capture {
        #[command(flatten)]
        project: ProjectArgs,
    },
    /// Show a D1 callback guard left by an active or crashed execution.
    Guard {
        #[command(flatten)]
        project: ProjectArgs,
    },
    /// Record external-effect evidence and release a stopped D1 callback guard.
    ReconcileGuard {
        #[command(flatten)]
        project: ProjectArgs,
        #[arg(long)]
        token: String,
        #[arg(long)]
        evidence: String,
    },
}

pub(crate) enum Remote {
    Postgres { url: String, recovery: String },
    D1 { endpoint: String, recovery: String },
}

impl Remote {
    pub(crate) fn profile(&self) -> &'static str {
        match self { Self::Postgres { .. } => POSTGRES, Self::D1 { .. } => CLOUDFLARE_D1 }
    }
}

fn variable(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.trim().is_empty())
}

/// Validate environment bindings before a command opens any project or dispatches work.
pub fn validate_environment() -> Result<bool> {
    let profile = variable(STORAGE_PROFILE_VAR).unwrap_or_else(|| "local".into());
    match profile.as_str() {
        "local" => Ok(false),
        POSTGRES => {
            for (key, capability) in [(CATALOG_URL_VAR, "catalog, execution owners and cadence"), (RECOVERY_ID_VAR, "whole-state recovery identity")] {
                if variable(key).is_none() { bail!("StorageCapabilityMissing: `{profile}` requires `{key}` for {capability}") }
            }
            Ok(true)
        }
        CLOUDFLARE_D1 => {
            for (key, capability) in [(D1_ENDPOINT_VAR, "private D1 and R2 binding"), (RECOVERY_ID_VAR, "whole-state recovery identity")] {
                if variable(key).is_none() { bail!("StorageCapabilityMissing: `{profile}` requires `{key}` for {capability}") }
            }
            Ok(true)
        }
        _ => bail!("StorageProfileUnsupported: `{profile}`; supported profiles are `local`, `{POSTGRES}` and `{CLOUDFLARE_D1}`"),
    }
}

fn remote() -> Result<Option<Remote>> {
    if !validate_environment()? {
        return Ok(None);
    }
    let profile = variable(STORAGE_PROFILE_VAR).unwrap_or_else(|| "local".into());
    if profile == CLOUDFLARE_D1 {
        if !cfg!(feature = "d1-catalog") {
            return Err(crate::ProfileError::ProfileCapabilityAbsent(format!("storage profile `{CLOUDFLARE_D1}` requires `d1-catalog`; build profile is `{}`", crate::PROFILE)).into());
        }
        let endpoint = variable(D1_ENDPOINT_VAR).context("private D1 binding")?;
        #[cfg(feature = "d1-catalog")]
        contextful_d1::D1::connect(&endpoint).map_err(anyhow::Error::msg)?;
        if !cfg!(feature = "s3-sync") {
            return Err(crate::ProfileError::ProfileCapabilityAbsent(format!("storage profile `{CLOUDFLARE_D1}` requires `s3-sync` for durable R2 corpus objects; build profile is `{}`", crate::PROFILE)).into());
        }
        return Ok(Some(Remote::D1 { endpoint, recovery: variable(RECOVERY_ID_VAR).context("recovery binding")? }));
    }
    if !cfg!(feature = "pg-catalog") {
        return Err(crate::ProfileError::ProfileCapabilityAbsent(format!("storage profile `{POSTGRES}` requires `pg-catalog` for journal, blobs, awakeables, owners, cadence and exports; build profile is `{}`", crate::PROFILE)).into());
    }
    if !cfg!(feature = "s3-sync") {
        return Err(crate::ProfileError::ProfileCapabilityAbsent(format!("storage profile `{POSTGRES}` requires `s3-sync` for durable corpus objects; build profile is `{}`", crate::PROFILE)).into());
    }
    Ok(Some(Remote::Postgres {
        url: variable(CATALOG_URL_VAR).context("catalog binding")?,
        recovery: variable(RECOVERY_ID_VAR).context("recovery binding")?,
    }))
}

fn validate_project(project: &Project, profile: &str) -> Result<()> {
    for manifest in contextful_context::project::manifests(&project.declaration())? {
        let value: toml::Value = toml::from_str(&manifest.text)?;
        if value
            .get("publication")
            .and_then(toml::Value::as_array)
            .is_some_and(|groups| !groups.is_empty())
        {
            bail!("PublicationStorageUnsupported: `{profile}` has no coordinated-group-outbox adapter")
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
            "StorageProfileUnsupported: `{profile}` has no sealed-row adapter"
        )
    }
    let sync = config
        .sync
        .context("StorageCapabilityMissing: durable corpus `[sync]` binding is required")?;
    if !matches!(sync.resolve_endpoint()?, Endpoint::S3 { .. }) || sync.bucket.is_empty() {
        bail!("StorageCapabilityMissing: a remote S3-compatible corpus bucket is required")
    }
    if profile == CLOUDFLARE_D1 {
        let endpoint = sync.resolve_endpoint()?;
        let bound_r2 = sync.endpoint.starts_with("r2://") || matches!(endpoint, Endpoint::S3 { ref url, .. } if url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_owned)).is_some_and(|host| host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())));
        if !bound_r2 { bail!("StorageCapabilityMissing: `{CLOUDFLARE_D1}` requires an R2 corpus endpoint") }
    }
    if sync.pull_before_run != Some(true) || sync.push_after_run != Some(true) {
        bail!("StorageCapabilityMissing: disposable disks require `[sync] pull_before_run = true` and `push_after_run = true`")
    }
    let bucket = crate::sync::bucket(&sync)?;
    if profile == CLOUDFLARE_D1 {
        let prefix = sync.resolve_prefix(variable)?;
        let probe = if prefix.is_empty() { ".contextful-preflight/".to_owned() }
            else { format!("{prefix}/.contextful-preflight/") };
        bucket.list(&probe).map_err(|error| anyhow::anyhow!("StorageCapabilityMissing: R2 corpus bucket: {error}"))?;
    }
    Ok(())
}

pub(crate) fn preflight(project: &Project) -> Result<Option<Remote>> {
    let Some(remote) = remote()? else {
        return Ok(None);
    };
    validate_project(project, remote.profile())?;
    recovery(project, &remote, false)?;
    Ok(Some(remote))
}

fn recovery(project: &Project, remote: &Remote, initialize: bool) -> Result<i64> {
    let _ = (project, initialize);
    match remote {
        Remote::Postgres { url, recovery } => {
            #[cfg(feature = "pg-catalog")]
            {
                if initialize { contextful_pg::PgRunStores::initialize_recovery(url, &project.name, recovery)? }
                else { contextful_pg::PgRunStores::verify_recovery(url, &project.name, recovery)? }
                Ok(contextful_pg::EXECUTION_SCHEMA_VERSION)
            }
            #[cfg(not(feature = "pg-catalog"))]
            { let _ = (url, recovery); bail!("ProfileCapabilityAbsent: durable recovery requires `pg-catalog`") }
        }
        Remote::D1 { endpoint, recovery } => {
            #[cfg(feature = "d1-catalog")]
            {
                let db = contextful_d1::D1::connect(endpoint).map_err(anyhow::Error::msg)?;
                if initialize { db.initialize_recovery(&project.name, recovery).map_err(anyhow::Error::msg)? }
                else { db.verify_recovery(&project.name, recovery).map_err(anyhow::Error::msg)? }
                let objects = db.recovery_objects(&project.name, recovery).map_err(anyhow::Error::msg)?;
                verify_corpus_frontier(project, &objects)?;
                Ok(contextful_d1::EXECUTION_SCHEMA_VERSION)
            }
            #[cfg(not(feature = "d1-catalog"))]
            { let _ = (endpoint, recovery); bail!("ProfileCapabilityAbsent: durable recovery requires `d1-catalog`") }
        }
    }
}

#[cfg(feature = "d1-catalog")]
fn verify_corpus_frontier(project: &Project, objects: &[contextful_d1::RecoveryObject]) -> Result<()> {
    if objects.is_empty() { return Ok(()) }
    let path = project.store_root().join(contextful_core::store::lay_out::CONFIG_FILE);
    let text = std::fs::read_to_string(path)?;
    let config: StoreConfig = toml::from_str(&text)?;
    let sync = config.sync.context("StorageCapabilityMissing: durable corpus `[sync]` binding is required")?;
    let prefix = sync.resolve_prefix(variable)?;
    let bucket = crate::sync::bucket(&sync)?;
    for object in objects {
        if !prefix.is_empty() && !object.key.starts_with(&format!("{prefix}/")) {
            bail!("ExecutionRecoveryMismatch: corpus key outside the recovery prefix")
        }
        let Some((bytes, _)) = bucket.get(&object.key)
            .map_err(|error| anyhow::anyhow!("StorageCapabilityMissing: R2 corpus object: {error}"))?
        else { bail!("ExecutionRecoveryMismatch: missing R2 corpus object `{}`", object.key) };
        let digest = format!("{:x}", Sha256::digest(&bytes));
        if digest != object.sha256 {
            bail!("ExecutionRecoveryMismatch: R2 corpus object `{}` changed", object.key)
        }
    }
    Ok(())
}

pub fn run(command: StorageCmd) -> Result<()> {
    let (args, initialize) = match command {
        StorageCmd::Check { project } => (project, false),
        StorageCmd::Initialize { project } => (project, true),
        StorageCmd::Capture { project } => return capture(project),
        StorageCmd::Guard { project } => return guard(project, None),
        StorageCmd::ReconcileGuard { project, token, evidence } => return guard(project, Some((token, evidence))),
    };
    let Some(remote) = remote()? else {
        if initialize {
            bail!("StorageProfileUnsupported: initialization requires `{POSTGRES}` or `{CLOUDFLARE_D1}`")
        }
        println!(
            "{}",
            json!({ "profile":"local", "replacement_safe":false, "strict_execution_guards":false, "capabilities":["journal", "blobs", "awakeables", "owners", "cadence", "exports"] })
        );
        return Ok(());
    };
    let project = args.locate(None)?.project;
    validate_project(&project, remote.profile())?;
    let schema = recovery(&project, &remote, initialize)?;
    println!(
        "{}",
        json!({ "profile":remote.profile(), "replacement_safe":true, "strict_execution_guards":true, "schema":schema, "project":project.name,
        "capabilities":["journal", "blobs", "awakeables", "owners", "cadence", "exports", "fenced-dispatch", "recovery-identity"] })
    );
    Ok(())
}

fn capture(args: ProjectArgs) -> Result<()> {
    let Some(remote) = remote()? else { bail!("StorageProfileUnsupported: recovery capture requires `{CLOUDFLARE_D1}`") };
    let Remote::D1 { endpoint, recovery: generation } = &remote else {
        bail!("StorageProfileUnsupported: recovery capture requires `{CLOUDFLARE_D1}`")
    };
    let project = args.locate(None)?.project;
    validate_project(&project, CLOUDFLARE_D1)?;
    recovery(&project, &remote, false)?;
    #[cfg(feature = "d1-catalog")]
    {
        let path = project.store_root().join(contextful_core::store::lay_out::CONFIG_FILE);
        let text = std::fs::read_to_string(path)?;
        let config: StoreConfig = toml::from_str(&text)?;
        let sync = config.sync.context("StorageCapabilityMissing: durable corpus `[sync]` binding is required")?;
        let prefix = sync.resolve_prefix(variable)?;
        let bucket = crate::sync::bucket(&sync)?;
        let root = if prefix.is_empty() { String::new() } else { format!("{prefix}/") };
        let listed = bucket.list(&root)?;
        let mut objects = Vec::with_capacity(listed.len());
        for key in listed {
            let (bytes, _) = bucket.get(&key)?.ok_or_else(|| anyhow::anyhow!("ExecutionRecoveryMismatch: missing R2 corpus object `{key}`"))?;
            objects.push(contextful_d1::RecoveryObject { key, sha256: format!("{:x}", Sha256::digest(&bytes)) });
        }
        let db = contextful_d1::D1::connect(endpoint).map_err(anyhow::Error::msg)?;
        let frontier = db.capture_recovery(&project.name, generation, &objects).map_err(anyhow::Error::msg)?;
        println!("{}", json!({"profile":CLOUDFLARE_D1,"project":project.name,"generation":generation,
            "frontier_sha256":frontier,"corpus_objects":objects.len(),"schema":contextful_d1::EXECUTION_SCHEMA_VERSION}));
        Ok(())
    }
    #[cfg(not(feature = "d1-catalog"))]
    { let _ = (endpoint, generation); bail!("ProfileCapabilityAbsent: recovery capture requires `d1-catalog`") }
}

fn guard(args: ProjectArgs, reconciliation: Option<(String, String)>) -> Result<()> {
    let Some(remote) = remote()? else { bail!("StorageProfileUnsupported: callback guards require `{CLOUDFLARE_D1}`") };
    let Remote::D1 { endpoint, .. } = &remote else { bail!("StorageProfileUnsupported: callback guards require `{CLOUDFLARE_D1}`") };
    let project = args.locate(None)?.project;
    validate_project(&project, CLOUDFLARE_D1)?;
    recovery(&project, &remote, false)?;
    #[cfg(feature = "d1-catalog")]
    {
        let catalog = contextful_d1::D1Catalog::connect(endpoint)?;
        if let Some((token, evidence)) = reconciliation {
            catalog.reconcile_guard(&token, &evidence)?;
        }
        println!("{}", json!({ "profile":CLOUDFLARE_D1, "token":catalog.pending_guard()? }));
        Ok(())
    }
    #[cfg(not(feature = "d1-catalog"))]
    { let _ = (endpoint, reconciliation); bail!("ProfileCapabilityAbsent: callback guard requires `d1-catalog`") }
}
