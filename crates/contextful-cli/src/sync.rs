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
use contextful_core::coordinate::Catalog;
use contextful_core::run::record::{RunRow, Unpushed};
use contextful_core::store::lay_out::{Pointer, MANIFEST_FILE, POINTER_FILE};
use contextful_core::store::object::ObjectStore;
use contextful_core::store::sync::{confine, Endpoint, SyncConfig};
use contextful_policy::replica::RootBucket;
use contextful_core::time::Instant;
#[cfg(feature = "data-plane")]
use contextful_core::store::catalog::MACHINE_CATALOG_FILE;
#[cfg(feature = "data-plane")]
use contextful_sync::{run_state, RunState};
use contextful_policy::enforce::policy::TablePolicy;
use contextful_sync::{FsBucket, PullScope, Redistribution, SiteResidency, Syncer};
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
        /// A table pattern — `*`, a prefix ending in `*`, or a name; repeatable; absent pulls every table.
        #[arg(long = "table", value_parser = parse_pattern)]
        tables: Vec<contextful_core::grant::TablePattern>,
        /// Restore this generation's manifest and pointers in place of the bucket's current state.
        #[arg(long)]
        generation: Option<u64>,
    },
    /// List the generation manifests the bucket holds, with each commit's time, pushing node and
    /// the project's entry and tombstone counts.
    Generations {
        #[command(flatten)]
        args: SyncArgs,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// List, as JSON, each run committed on a disposable disk that no committed push has
    /// carried, refusing while one is not lost.
    Status {
        #[command(flatten)]
        args: SyncArgs,
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

/// Parse a `--table` pattern, refusing a misplaced star (`authority.grant.malformed-pattern`).
fn parse_pattern(s: &str) -> std::result::Result<contextful_core::grant::TablePattern, String> {
    contextful_core::grant::TablePattern::parse(s).map_err(|e| e.to_string())
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
    let redistribution = Redistribution { withheld: replicate_off(&decls)?.into_iter().collect() };
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
        redistribution,
    };
    Ok((syncer, decls))
}

/// The tables whose redistribution flag is cleared: `replicate = false` on the table block or
/// under its compiled `policy.redistribution`. A push withholds them
/// (`authority.bound-redistribution.withhold`) and a replica pull leaves them behind.
fn replicate_off(decls: &[TableDecl]) -> Result<Vec<String>> {
    let mut off = Vec::new();
    for d in decls {
        if d.replicate == Some(false) || TablePolicy::from_decl(d)?.replicate == Some(false) {
            off.push(d.name.clone());
        }
    }
    Ok(off)
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
pub(crate) fn bucket(config: &SyncConfig) -> Result<Arc<dyn ObjectStore>> {
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
    pull_before_run_within(l, None)
}

/// [`pull_before_run`], waiting at most `guard_wait` seconds for the push guard when given,
/// and `push_wait_secs` otherwise.
pub fn pull_before_run_within(l: &Located, guard_wait: Option<u64>) -> Result<()> {
    match pending_pull_within(l, guard_wait)? {
        Some(pull) => pull.run(),
        None => Ok(()),
    }
}

/// The pull `[sync] pull_before_run = true` declares, its bucket and credentials resolved
/// now and its transfer run later; `None` for a store declaring no such pull.
pub fn pending_pull(l: &Located) -> Result<Option<PendingPull>> {
    pending_pull_within(l, None)
}

fn pending_pull_within(l: &Located, guard_wait: Option<u64>) -> Result<Option<PendingPull>> {
    let (Some(config), _) = sync_config(l)? else { return Ok(None) };
    let lazy = config.lazy_parts();
    if config.pull_before_run != Some(true) {
        if lazy {
            register_parts(&open_with(l, config)?.0);
        }
        return Ok(None);
    }
    let config = capped(config, guard_wait);
    let (syncer, decls) = open_with(l, config)?;
    if lazy {
        register_parts(&syncer);
    }
    let replicate_off = replicate_off(&decls)?;
    Ok(Some(PendingPull { syncer, scope: PullScope { tables: Vec::new(), replicate_off, generation: None } }))
}

/// Every store this process opens at `syncer`'s root fetches each part it lacks through the
/// syncer's bucket (`store.pull.lazy-parts`).
fn register_parts(syncer: &Syncer) {
    contextful_context::store::register_part_source(syncer.store.root(), syncer.part_source());
}

/// A resolved `pull_before_run` whose transfer has not run.
pub struct PendingPull {
    syncer: Syncer,
    scope: PullScope,
}

impl PendingPull {
    /// Transfer every table of the bucket into the store. The report line is best effort:
    /// a closed stderr fails no pull.
    pub fn run(self) -> Result<()> {
        use std::io::Write;
        let r = self.syncer.pull(&self.scope).context("`pull_before_run`")?;
        for discarded in &r.discarded {
            let _ = writeln!(std::io::stderr(), "pull_before_run: warning: {discarded}");
        }
        let _ = writeln!(std::io::stderr(), "pull_before_run: pulled {} objects and {} pointers", r.downloaded.len(), r.pointers.len());
        self.push_unpushed()
    }

    /// Push before the first read while this node holds a run marked unpushed and not lost
    /// (`store.drain.before-read`).
    fn push_unpushed(&self) -> Result<()> {
        let Some(catalog) = durable_catalog(&self.syncer.store)? else { return Ok(()) };
        let marked = marked_for(catalog.as_ref(), &self.syncer.node)?;
        if marked.is_empty() {
            return Ok(());
        }
        let runs = run_list(&marked);
        let r = push(&self.syncer, now(&None)?)
            .with_context(|| format!("`pull_before_run`: run(s) {runs} stay unpushed, and no read starts before a push carries them"))?;
        for refusal in &r.refused {
            eprintln!("warning: {refusal}");
        }
        eprintln!("pull_before_run: pushed unpushed run(s) {runs}");
        Ok(())
    }
}

/// Push the store once a fire's runs close when `[sync] push_after_run = true`, on the
/// failure arm as on the success arm (`store.push.after-run`,
/// `run.record.failure-publishes`). A store declaring no `[sync]` is left as it is.
pub fn push_after_run(l: &Located, at: Instant) -> Result<()> {
    push_after_run_within(l, at, None)
}

/// [`push_after_run`], waiting at most `guard_wait` seconds for the push guard when given,
/// and `push_wait_secs` otherwise.
pub fn push_after_run_within(l: &Located, at: Instant, guard_wait: Option<u64>) -> Result<()> {
    let (Some(config), _) = sync_config(l)? else { return Ok(()) };
    if config.push_after_run != Some(true) {
        return Ok(());
    }
    let config = capped(config, guard_wait);
    let r = open_with(l, config).and_then(|(s, _)| push(&s, at)).context("`push_after_run`")?;
    for refusal in &r.refused {
        eprintln!("warning: {refusal}");
    }
    eprintln!("push_after_run: pushed");
    Ok(())
}

/// `config` with its push guard wait lowered to `guard_wait` seconds when given.
fn capped(mut config: SyncConfig, guard_wait: Option<u64>) -> SyncConfig {
    if let Some(cap) = guard_wait {
        config.push_wait_secs = Some(config.push_wait_secs().min(cap));
    }
    config
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

/// The store, project, node and redistribution bound a push plan reads: no `[sync]` and no
/// bucket.
fn open_local(args: &SyncArgs) -> Result<(Store, String, String, Redistribution)> {
    let l = locate(args.project.as_deref(), args.declaration.clone())?;
    let store = Store::open(&l.project.dir, &l.project.name)?;
    let (node_id, _) = node::resolve(&store, |k| std::env::var(k).ok())?;
    let text = std::fs::read_to_string(&l.declaration).unwrap_or_default();
    let decls = TableDecl::parse_declaration_set(&text, &pipeline_files(&l.declaration)?)?;
    let bound = Redistribution { withheld: replicate_off(&decls)?.into_iter().collect() };
    Ok((store, l.project.name, node_id.to_string(), bound))
}

/// Write this node's run state into the store root from its machine catalog, so the push
/// carries it (`store.push.run-state`).
#[cfg(feature = "data-plane")]
fn record_run_state(store: &Store, project: &str, node_id: &str) -> Result<()> {
    let machine = store.root().join(MACHINE_CATALOG_FILE);
    let clock = Arc::new(crate::clock::SystemClock);
    let catalog = crate::run::open_catalog(&machine, clock, store.file_cipher())?;
    let mut state = RunState::read(catalog.as_ref(), node_id)?;
    let project_dir = store.root().ancestors().nth(project.split('/').count() + 2).context("the store has a project directory")?;
    let project_path = contextful_context::project::Project { dir: project_dir.to_path_buf(), name: project.to_string() };
    let mut summaries = crate::source_runs::Summaries::new();
    if let Some(prior) = run_state::run_states(store)?.remove(node_id) {
        crate::source_runs::merge(&mut summaries, prior.sources);
    }
    crate::source_runs::merge(&mut summaries, crate::source_runs::local(&project_path.audit_dir(), &catalog.runs(None)?)?);
    state.sources = summaries.into_values().collect();
    state.control_version = run_state::local_control_version(store, project)?;
    run_state::record(store, &state)?;
    Ok(())
}

/// A read-plane build fires no runs and holds no machine catalog, so it records no run state.
#[cfg(not(feature = "data-plane"))]
fn record_run_state(_store: &Store, _project: &str, _node_id: &str) -> Result<()> {
    Ok(())
}

/// Record the run state, then push; a committed push settles each unpushed mark this node
/// held before it started (`store.drain.clear`).
fn push(s: &Syncer, at: Instant) -> Result<contextful_sync::sync::PushReport> {
    record_run_state(&s.store, &s.project, &s.node)?;
    let catalog = durable_catalog(&s.store)?;
    let marked = match &catalog {
        Some(c) => marked_for(c.as_ref(), &s.node)?,
        None => Vec::new(),
    };
    let report = s.push(at)?;
    if let Some(c) = &catalog {
        settle(c.as_ref(), &s.store, &s.node, &marked)?;
    }
    Ok(report)
}

/// The catalog holding run rows in the execution database a disposable-disk storage profile
/// binds; `None` under the local profile, whose disk is not disposable.
#[cfg(feature = "data-plane")]
fn durable_catalog(store: &Store) -> Result<Option<Arc<dyn Catalog + Send + Sync>>> {
    if !crate::storage::validate_environment()? {
        return Ok(None);
    }
    let machine = store.root().join(MACHINE_CATALOG_FILE);
    Ok(Some(crate::run::open_catalog(&machine, Arc::new(crate::clock::SystemClock), store.file_cipher())?))
}

/// A read-plane build fires no runs, so it holds no run row to mark.
#[cfg(not(feature = "data-plane"))]
fn durable_catalog(_store: &Store) -> Result<Option<Arc<dyn Catalog + Send + Sync>>> {
    Ok(None)
}

/// The runs marked unpushed on `node` and not lost.
fn marked_for(catalog: &dyn Catalog, node: &str) -> Result<Vec<RunRow>> {
    Ok(catalog.unpushed_runs()?.into_iter().filter(|r| r.unpushed.as_ref().is_some_and(|u| u.node == node && !u.lost)).collect())
}

/// Whether the store holds a committed run manifest of `run_id` on `node`, under any table.
fn holds_run(store: &Store, run_id: &str, node: &str) -> bool {
    let Ok(tables) = std::fs::read_dir(store.root().join("tables")) else { return false };
    tables.flatten().any(|t| t.path().join("data/runs").join(run_id).join(node).join(MANIFEST_FILE).is_file())
}

/// Each run id, quoted, for a message.
fn run_list(rows: &[RunRow]) -> String {
    rows.iter().map(|r| format!("`{}`", r.run_id)).collect::<Vec<_>>().join(", ")
}

/// After a committed push, clear each mark whose run the store holds, which the push carried,
/// and mark lost each whose run it no longer holds (`store.drain.clear`).
fn settle(catalog: &dyn Catalog, store: &Store, node: &str, marked: &[RunRow]) -> Result<()> {
    for row in marked {
        let held = holds_run(store, &row.run_id, node);
        catalog.update_run(&row.run_id, &mut |r| {
            let ours = r.unpushed.as_ref().is_some_and(|u| u.node == node);
            if ours && held {
                r.unpushed = None;
            } else if let Some(u) = r.unpushed.as_mut().filter(|_| ours) {
                u.lost = true;
            }
            Ok(())
        })?;
        if !held {
            eprintln!("warning: run `{}` committed on node `{node}`, and neither the bucket nor this store holds it; it is marked lost", row.run_id);
        }
    }
    Ok(())
}

/// Mark each of `run_ids` whose run manifest the store holds as unpushed on this node before
/// `push_after_run` pushes, under a disposable-disk storage profile (`store.drain.mark`).
#[cfg(feature = "data-plane")]
pub fn mark_unpushed(l: &Located, run_ids: &[String]) -> Result<()> {
    let (Some(config), _) = sync_config(l)? else { return Ok(()) };
    if config.push_after_run != Some(true) {
        return Ok(());
    }
    let store = Store::open(&l.project.dir, &l.project.name)?;
    let Some(catalog) = durable_catalog(&store)? else { return Ok(()) };
    let (node, _) = node::resolve(&store, |k| std::env::var(k).ok())?;
    let node = node.to_string();
    for id in run_ids.iter().filter(|id| holds_run(&store, id, &node)) {
        catalog.update_run(id, &mut |r| {
            r.unpushed = Some(Unpushed { node: node.clone(), lost: false });
            Ok(())
        })?;
    }
    Ok(())
}

/// The longest push a stopping `pipeline serve` attempts (`store.drain.deadline`).
#[cfg(feature = "data-plane")]
pub const DRAIN_SECS: u64 = 15;

/// On a stop signal, push once the children are ended when one was running or this node
/// holds an unpushed run, for at most [`DRAIN_SECS`] (`store.drain.deadline`); a push
/// failing or outrunning it fails serve and names the runs it leaves marked
/// (`store.drain.unfinished`).
#[cfg(feature = "data-plane")]
pub fn drain(l: &Located, ended_child: bool) -> Result<()> {
    let (Some(config), _) = sync_config(l)? else { return Ok(()) };
    if config.push_after_run != Some(true) {
        return Ok(());
    }
    let store = Store::open(&l.project.dir, &l.project.name)?;
    let (node, _) = node::resolve(&store, |k| std::env::var(k).ok())?;
    let node = node.to_string();
    let catalog = durable_catalog(&store)?;
    let marked = match &catalog {
        Some(c) => marked_for(c.as_ref(), &node)?,
        None => Vec::new(),
    };
    if !ended_child && marked.is_empty() {
        return Ok(());
    }
    let located = Located { project: l.project.clone(), declaration: l.declaration.clone() };
    let at = now(&None)?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(push_after_run_within(&located, at, Some(DRAIN_SECS)));
    });
    let answered = rx.recv_timeout(std::time::Duration::from_secs(DRAIN_SECS));
    let left = || -> String {
        let rows = catalog.as_ref().and_then(|c| marked_for(c.as_ref(), &node).ok()).unwrap_or_else(|| marked.clone());
        if rows.is_empty() { "no run is marked unpushed".to_string() } else { format!("run(s) {} stay unpushed", run_list(&rows)) }
    };
    match answered {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e.context(format!("the stop push failed; {}", left()))),
        Err(_) => anyhow::bail!("the stop push did not finish within {DRAIN_SECS} s; {}", left()),
    }
}

/// Every run marked unpushed, any node's, and the refusal while one is not lost
/// (`store.drain.status`).
fn status(args: &SyncArgs) -> Result<()> {
    let l = locate(args.project.as_deref(), args.declaration.clone())?;
    let store = Store::open(&l.project.dir, &l.project.name)?;
    let rows = match durable_catalog(&store)? {
        Some(c) => c.unpushed_runs()?,
        None => Vec::new(),
    };
    let listed: Vec<serde_json::Value> = rows
        .iter()
        .filter_map(|r| {
            let u = r.unpushed.as_ref()?;
            Some(serde_json::json!({
                "run_id": r.run_id,
                "pipeline": r.pipeline_id,
                "table": r.table,
                "node": u.node,
                "ended_at": r.ended_at.map(|t| t.to_string()),
                "lost": u.lost,
            }))
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "runs": listed }))?);
    let open: Vec<RunRow> = rows.into_iter().filter(|r| r.unpushed.as_ref().is_some_and(|u| !u.lost)).collect();
    if !open.is_empty() {
        anyhow::bail!("SyncRunsUnpushed: run(s) {} committed on a disposable disk, and no committed push carried them; replacing that disk discards them", run_list(&open));
    }
    Ok(())
}

pub fn run(cmd: SyncCmd) -> Result<()> {
    match cmd {
        SyncCmd::Manifest { args, emit: _ } => {
            let (store, project, node_id, bound) = open_local(&args)?;
            record_run_state(&store, &project, &node_id)?;
            // The emitted plan holds the bound a push of the store holds (`store.emit.plan`).
            let mut plan = contextful_sync::plan_manifest(&store, &project, &node_id)?;
            plan.withhold(&bound, &project);
            println!("{}", serde_json::to_string_pretty(&plan.manifest())?);
            Ok(())
        }
        SyncCmd::Status { args } => status(&args),
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
            let r = s.pull(&PullScope { tables, replicate_off: replicate_off(&decls)?, generation })?;
            for refusal in r.unverified.iter().chain(&r.discarded) {
                eprintln!("warning: {refusal}");
            }
            println!("pulled {} objects and {} pointers in {} attempt(s)", r.downloaded.len(), r.pointers.len(), r.attempts);
            Ok(())
        }
        SyncCmd::Generations { args, json } => {
            let (s, _) = open(&args)?;
            let listed = s.generation_list()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "generations": listed }))?);
                return Ok(());
            }
            let dash = |v: Option<String>| v.unwrap_or_else(|| "-".into());
            println!("generation committed_at committed_by entries tombstones");
            for g in listed {
                println!("{} {} {} {} {}", g.generation, dash(g.committed_at.map(|t| t.to_string())), dash(g.committed_by), g.entries, g.tombstones);
            }
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
