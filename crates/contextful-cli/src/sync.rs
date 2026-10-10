//! `contextful sync` — the bucket surface of the command line.
//!
//! Each subcommand opens the store and its `[sync]` bucket and calls `contextful-sync`.
//! The endpoint's scheme selects the adapter: `file://` the filesystem bucket, and
//! `s3://`, `r2://`, `https://` and loopback `http://` the S3 adapter under `s3-sync`.

use crate::project::{locate, pipeline_files, Located};
use anyhow::{Context, Result};
use clap::Subcommand;
use contextful_context::fold::fold_under;
use contextful_context::{node, Store};
use contextful_core::store::declare::TableDecl;
use contextful_core::store::fold::FoldOutcome;
use contextful_core::store::lay_out::{Pointer, POINTER_FILE};
use contextful_core::store::object::ObjectStore;
use contextful_core::store::sync::{confine, Endpoint, SyncConfig};
use contextful_policy::replica::RootBucket;
use contextful_core::time::Instant;
#[cfg(feature = "data-plane")]
use contextful_core::store::catalog::MACHINE_CATALOG_FILE;
#[cfg(feature = "data-plane")]
use contextful_sqlite::MachineCatalog;
#[cfg(feature = "data-plane")]
use contextful_sync::{run_state, RunState};
use contextful_sync::{FsBucket, PullScope, SiteResidency, Syncer};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(clap::Args)]
pub struct SyncArgs {
    /// The project whose store root is `.contextful/context/<project>/` under the working
    /// directory; absent, the nearest `contextful.toml` upward names it.
    #[arg(long)]
    project: Option<String>,
    /// The pipeline manifest holding the table declarations; absent, the project's
    /// `contextful.toml`.
    #[arg(long)]
    declaration: Option<PathBuf>,
    /// The judging instant (RFC 3339); absent reads the system clock.
    #[arg(long)]
    now: Option<String>,
}

#[derive(Subcommand)]
pub enum SyncCmd {
    /// Demonstrate the bucket's conditional writes and print the coordination they allow.
    Probe {
        #[command(flatten)]
        args: SyncArgs,
    },
    /// Print the bucket manifest a push of the store commits, reading no bucket object.
    Manifest {
        #[command(flatten)]
        args: SyncArgs,
        /// Emit the plan computed from the store's files.
        #[arg(long, required = true)]
        emit: bool,
    },
    /// Upload the store and commit the bucket manifest as the next generation.
    Push {
        #[command(flatten)]
        args: SyncArgs,
    },
    /// Fetch the bucket into the store, writing each table pointer last.
    Pull {
        #[command(flatten)]
        args: SyncArgs,
        /// Repeatable; absent pulls every table.
        #[arg(long = "table")]
        tables: Vec<String>,
        /// Restore this generation's manifest and pointers in place of the bucket's current state.
        #[arg(long)]
        generation: Option<u64>,
    },
    /// Take or release a table's compaction lease in the bucket.
    #[command(subcommand)]
    Lease(LeaseCmd),
    /// Fold a table under its compaction lease and publish the snapshot under the lease's fence.
    Compact {
        table: String,
        #[command(flatten)]
        args: SyncArgs,
        /// Fold under the lease this node already holds instead of taking one.
        #[arg(long)]
        held: bool,
    },
}

#[derive(Subcommand)]
pub enum LeaseCmd {
    Acquire {
        table: String,
        #[command(flatten)]
        args: SyncArgs,
    },
    Release {
        table: String,
        #[command(flatten)]
        args: SyncArgs,
    },
}

fn now(flag: &Option<String>) -> Result<Instant> {
    Ok(match flag {
        Some(s) => Instant::parse(s)?,
        None => {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
            Instant::from_unix_nanos(i128::try_from(nanos)?)?
        }
    })
}

fn open(args: &SyncArgs) -> Result<(Syncer, Vec<TableDecl>)> {
    let l = locate(args.project.as_deref(), args.declaration.clone())?;
    let (config, config_path) = sync_config(&l)?;
    let config = config.with_context(|| format!("`{}` declares no `[sync]`", config_path.display()))?;
    open_with(&l, config)
}

/// The store's `[sync]` block, absent when the store root holds no `config.toml` or the
/// file declares none, and the path read.
pub(crate) fn sync_config(l: &Located) -> Result<(Option<SyncConfig>, PathBuf)> {
    let config_path = l.project.store_root().join(contextful_core::store::lay_out::CONFIG_FILE);
    let text = match std::fs::read_to_string(&config_path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((None, config_path)),
        Err(e) => return Err(e).with_context(|| format!("reading `{}`", config_path.display())),
    };
    let file: contextful_core::store::config::StoreConfig = toml::from_str(&text).with_context(|| format!("`{}`", config_path.display()))?;
    Ok((file.sync, config_path))
}

fn open_with(l: &Located, config: SyncConfig) -> Result<(Syncer, Vec<TableDecl>)> {
    let store = Store::open(&l.project.dir, &l.project.name)?;
    let bucket = bucket(&config)?;
    let prefix = config.resolve_prefix(|k| std::env::var(k).ok())?;
    let (node_id, _) = node::resolve(&store, |k| std::env::var(k).ok())?;
    // The declaration set (`store.declare.declaration-set`): `replicate` and `primary_key`
    // read here match those the read face reads.
    let text = std::fs::read_to_string(&l.declaration).unwrap_or_default();
    let control_dir = control_dir(&text, &l.project.dir, &l.project.name)?;
    let decls = TableDecl::parse_declaration_set(&text, &pipeline_files(&l.declaration)?)?;
    let site_id = crate::project::site_id_for(&text, &l.declaration, None, None).unwrap_or_else(|_| node_id.to_string());
    let residency = Some(SiteResidency { site_id, regions: crate::reside::declared(&text)?.map(|r| r.entries()) });
    // Tombstones sign with the project's issuer key and verify against its key-set ledger
    // (`store.merge.tombstone-signed`, `store.merge.tombstone-unverified`).
    let seed = l.project.dir.join(contextful_policy::issue::DEFAULT_SEED_PATH);
    let signer: Option<std::sync::Arc<dyn contextful_core::ports::SigningPort + Send + Sync>> = match std::fs::read_to_string(&seed) {
        Ok(text) => Some(std::sync::Arc::new(
            contextful_policy::issue::SeedSigner::from_seed(&text).with_context(|| format!("issuer key `{}`", seed.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("reading `{}`", seed.display())),
    };
    let key_set = Some(l.project.dir.join(contextful_core::revoke::KeySetLedger::PATH));
    let syncer = Syncer {
        store,
        bucket,
        config,
        prefix,
        project: l.project.name.clone(),
        node: node_id.to_string(),
        residency,
        control_dir,
        signer,
        key_set,
    };
    Ok((syncer, decls))
}

fn control_dir(text: &str, project_dir: &std::path::Path, project: &str) -> Result<Option<PathBuf>> {
    let value: toml::Value = toml::from_str(text)?;
    let Some(block) = value.get("control") else {
        return Ok(Some(project_dir.join(".contextful/control").join(project)));
    };
    let block = block.as_table().context("`[control]` is a table")?;
    if block.contains_key("url") && block.contains_key("snapshot_dir") {
        anyhow::bail!("`[control]` names one source: `url` or `snapshot_dir`");
    }
    if block.contains_key("url") {
        return Ok(None);
    }
    let path = block.get("snapshot_dir")
        .map(|v| v.as_str().context("`[control] snapshot_dir` is a path string"))
        .transpose()?;
    Ok(Some(path.map_or_else(|| project_dir.join(".contextful/control").join(project), |p| project_dir.join(p))))
}

/// Open the bucket `[sync] endpoint` names through the adapter its scheme selects
/// (`store.endpoint.schemes`).
fn bucket(config: &SyncConfig) -> Result<Arc<dyn ObjectStore>> {
    Ok(match config.resolve_endpoint()? {
        Endpoint::File(dir) => Arc::new(FsBucket::open(std::path::Path::new(&dir), &config.bucket).map_err(|e| anyhow::anyhow!("bucket: {e}"))?),
        Endpoint::S3 { url, region } => s3_bucket(config, &url, &region)?,
    })
}

/// An S3 or R2 bucket, signing with the material its credential keys hydrate
/// (`store.endpoint.credentials`, `connector.reference.whole-value-reference`).
#[cfg(feature = "s3-sync")]
fn s3_bucket(config: &SyncConfig, url: &str, region: &str) -> Result<Arc<dyn ObjectStore>> {
    use contextful_core::connector::reference::Hydrated;
    use contextful_core::store::sync::CredentialRef;
    use contextful_core::store::StoreError;
    let refs = config.credential_refs()?;
    let vars: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let mut resolver = None;
    let mut hydrate = |key: &str, r: &CredentialRef| -> Result<Hydrated> {
        match r {
            CredentialRef::Env(var) => Ok(Hydrated::new(vars.get(var).filter(|v| !v.is_empty()).cloned().ok_or_else(|| {
                StoreError::SyncCredentialUnbound(format!("`[sync] {key}` binds `env://{var}`, which is unset"))
            })?)),
            CredentialRef::Secret(name) => {
                if resolver.is_none() {
                    resolver = Some(contextful_outbound::assemble(&vars, Arc::new(crate::clock::SystemClock))?);
                }
                // A credential key is a whole value, never a template: the chain's environment adapter serves it.
                Ok(resolver.as_ref().expect("assembled above").resolve(name)?)
            }
        }
    };
    let credentials = contextful_sync::S3Credentials {
        access_key_id: hydrate("access_key_id", &refs.access_key_id)?,
        secret_access_key: hydrate("secret_access_key", &refs.secret_access_key)?,
        session_token: refs.session_token.as_ref().map(|r| hydrate("session_token", r)).transpose()?,
    };
    Ok(Arc::new(contextful_sync::S3Bucket::open(url, region, &config.bucket, credentials).map_err(|e| anyhow::anyhow!("bucket: {e}"))?))
}

/// A build without the S3 adapter refuses an S3 or R2 endpoint (`store.endpoint.unsupported-scheme`).
#[cfg(not(feature = "s3-sync"))]
fn s3_bucket(config: &SyncConfig, _url: &str, _region: &str) -> Result<Arc<dyn ObjectStore>> {
    Err(contextful_core::store::StoreError::SyncEndpointUnsupported(format!(
        "endpoint `{}`: this build links no S3 adapter; build with the `s3-sync` feature",
        config.endpoint
    ))
    .into())
}

/// Pull every table of the bucket into the store when `[sync] pull_before_run = true`, so
/// a command starting on a cold disk reads what the other nodes pushed
/// (`store.pull.before-run`). A store declaring no `[sync]` is left as it is.
pub fn pull_before_run(l: &Located) -> Result<()> {
    let (Some(config), _) = sync_config(l)? else { return Ok(()) };
    if config.pull_before_run != Some(true) {
        return Ok(());
    }
    let (s, decls) = open_with(l, config)?;
    let replicate_off = decls.iter().filter(|d| d.replicate == Some(false)).map(|d| d.name.clone()).collect();
    let r = s.pull(&PullScope { tables: Vec::new(), replicate_off, generation: None }).context("`pull_before_run`")?;
    eprintln!("pull_before_run: pulled {} objects and {} pointers", r.downloaded.len(), r.pointers.len());
    Ok(())
}

/// Push the store once a fire's runs close when `[sync] push_after_run = true`, on the
/// failure arm as on the success arm (`store.push.after-run`,
/// `run.record.failure-publishes`). A store declaring no `[sync]` is left as it is.
pub fn push_after_run(l: &Located, at: Instant) -> Result<()> {
    let (Some(config), _) = sync_config(l)? else { return Ok(()) };
    if config.push_after_run != Some(true) {
        return Ok(());
    }
    let r = open_with(l, config).and_then(|(s, _)| push(&s, at)).context("`push_after_run`")?;
    for refusal in &r.refused {
        eprintln!("warning: {refusal}");
    }
    eprintln!("push_after_run: pushed");
    Ok(())
}

/// The project's root keyspace, `<prefix>/<project>/`, in its `[sync]` bucket, or `None`
/// when the store declares no `[sync]` (`disclosure.attest.root-replication`).
pub fn root_bucket(l: &Located) -> Result<Option<RootBucket>> {
    let (Some(config), _) = sync_config(l)? else { return Ok(None) };
    let bucket = bucket(&config)?;
    let prefix = config.resolve_prefix(|k| std::env::var(k).ok())?;
    let base = confine(&prefix, &l.project.name)?;
    Ok(Some(RootBucket::new(bucket, format!("{base}/"))))
}

/// The store, project and node a push plan reads: no `[sync]` and no bucket.
fn open_local(args: &SyncArgs) -> Result<(Store, String, String)> {
    let l = locate(args.project.as_deref(), args.declaration.clone())?;
    let store = Store::open(&l.project.dir, &l.project.name)?;
    let (node_id, _) = node::resolve(&store, |k| std::env::var(k).ok())?;
    Ok((store, l.project.name, node_id.to_string()))
}

/// Write this node's run state into the store root from its machine catalog, so the push
/// carries it (`store.push.run-state`).
#[cfg(feature = "data-plane")]
fn record_run_state(store: &Store, project: &str, node_id: &str) -> Result<()> {
    let machine = store.root().join(MACHINE_CATALOG_FILE);
    let clock = Arc::new(crate::clock::SystemClock);
    let catalog = match store.file_cipher() {
        Some(cipher) => MachineCatalog::open_sealed(&machine, clock, cipher)?,
        None => MachineCatalog::open(&machine, clock)?,
    };
    let mut state = RunState::read(&catalog, node_id)?;
    state.control_version = run_state::local_control_version(store, project)?;
    run_state::record(store, &state)?;
    Ok(())
}

/// A read-plane build fires no runs and holds no machine catalog, so it records no run state.
#[cfg(not(feature = "data-plane"))]
fn record_run_state(_store: &Store, _project: &str, _node_id: &str) -> Result<()> {
    Ok(())
}

/// Record the run state, then push.
fn push(s: &Syncer, at: Instant) -> Result<contextful_sync::sync::PushReport> {
    record_run_state(&s.store, &s.project, &s.node)?;
    Ok(s.push(at)?)
}

pub fn run(cmd: SyncCmd) -> Result<()> {
    match cmd {
        SyncCmd::Manifest { args, emit: _ } => {
            let (store, project, node_id) = open_local(&args)?;
            record_run_state(&store, &project, &node_id)?;
            let plan = contextful_sync::plan_manifest(&store, &project, &node_id)?;
            println!("{}", serde_json::to_string_pretty(&plan.manifest())?);
            Ok(())
        }
        SyncCmd::Probe { args } => {
            let (s, _) = open(&args)?;
            let (c, why) = s.probe_with_reason()?;
            let mode = match c {
                contextful_core::store::sync::Coordination::Cas => "cas",
                contextful_core::store::sync::Coordination::SingleWriter => "single-writer",
            };
            println!("{mode}: {why}");
            Ok(())
        }
        SyncCmd::Push { args } => {
            let (s, _) = open(&args)?;
            let r = push(&s, now(&args.now)?)?;
            for refusal in &r.refused {
                eprintln!("warning: {refusal}");
            }
            let mut stranded: std::collections::BTreeMap<&str, Vec<&str>> = std::collections::BTreeMap::new();
            for (key, owner) in &r.stranded {
                stranded.entry(owner.as_str()).or_default().push(key.as_str());
            }
            for (owner, keys) in stranded {
                eprintln!(
                    "warning: {} local key(s) owned by node `{owner}` are absent from the bucket manifest and stay unpushed ({}); push them with CONTEXTFUL_NODE_ID={owner}",
                    keys.len(),
                    keys.join(", ")
                );
            }
            println!(
                "pushed {} objects and {} pointers; generation {} lists {} entries after {} round(s)",
                r.uploaded.len(),
                r.pointers.len(),
                r.generation,
                r.entries,
                r.rounds
            );
            Ok(())
        }
        SyncCmd::Pull { args, tables, generation } => {
            let (s, decls) = open(&args)?;
            let replicate_off = decls.iter().filter(|d| d.replicate == Some(false)).map(|d| d.name.clone()).collect();
            let r = s.pull(&PullScope { tables, replicate_off, generation })?;
            for refusal in &r.unverified {
                eprintln!("warning: {refusal}");
            }
            println!("pulled {} objects and {} pointers in {} attempt(s)", r.downloaded.len(), r.pointers.len(), r.attempts);
            Ok(())
        }
        SyncCmd::Lease(LeaseCmd::Acquire { table, args }) => {
            let (s, _) = open(&args)?;
            let held = s.acquire(&table, now(&args.now)?)?;
            println!("{table}: lease held at fence {} until {}", held.lease.fence, held.lease.expires_at.map(|e| e.to_string()).unwrap_or_default());
            Ok(())
        }
        SyncCmd::Lease(LeaseCmd::Release { table, args }) => {
            let (s, _) = open(&args)?;
            s.release(&table)?;
            println!("{table}: lease released");
            Ok(())
        }
        SyncCmd::Compact { table, args, held } => {
            let (s, decls) = open(&args)?;
            let at = now(&args.now)?;
            let lease = if held {
                s.held(&table)?.with_context(|| format!("this node holds no lease on `{table}`"))?
            } else {
                s.acquire(&table, at)?
            };
            let outcome = compact(&s, &decls, &table, &lease, at);
            if !held {
                s.release(&table)?;
            }
            println!("{table}: {}", outcome?);
            Ok(())
        }
    }
}

/// Pull, fold and publish under `lease`. A fenced publish restores the local pointer, so
/// the snapshot it carried stays unreadable here as in the bucket.
fn compact(s: &Syncer, decls: &[TableDecl], table: &str, lease: &contextful_sync::Held, at: Instant) -> Result<FoldOutcome> {
    s.pull(&PullScope::default())?;
    s.check_fence(table, lease)?;
    let decl = decls.iter().find(|d| d.name == table).cloned().unwrap_or_else(|| TableDecl::named(table));
    let pointer_path = s.store.table_dir(table)?.join(POINTER_FILE);
    let before = std::fs::read(&pointer_path).ok();
    let outcome = fold_under(&s.store, &decl, at, Some(lease.lease.fence))?;
    let FoldOutcome::Folded { snapshot_id, .. } = &outcome else { return Ok(outcome) };
    let published = push(s, at).and_then(|_| s.publish(table, snapshot_id, lease).map_err(anyhow::Error::from));
    if let Err(e) = published {
        match before {
            Some(b) => contextful_context::store::replace_file(&pointer_path, &b)?,
            None => {
                let _ = std::fs::remove_file(&pointer_path);
            }
        }
        return Err(e);
    }
    // The local pointer carries the fence it was published under, as a pulled one does.
    let published = Pointer { snapshot_id: serde_json::from_value(serde_json::Value::String(snapshot_id.clone()))?, fence: Some(lease.lease.fence) };
    s.store.metadata_files().replace(&pointer_path, &serde_json::to_vec_pretty(&published)?)?;
    Ok(outcome)
}
