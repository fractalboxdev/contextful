//! `contextful sync` — the bucket surface of the command line.
//!
//! Each subcommand opens the store and its `[sync]` bucket and calls `contextful-sync`.
//! This build links the filesystem bucket adapter: `endpoint = "file://<directory>"`.

use crate::project::locate;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use contextful_context::fold::fold;
use contextful_context::{node, Store};
use contextful_core::store::declare::TableDecl;
use contextful_core::store::fold::FoldOutcome;
use contextful_core::store::lay_out::{Pointer, POINTER_FILE};
use contextful_core::store::sync::SyncConfig;
use contextful_core::time::Instant;
use contextful_sync::{FsBucket, PullScope, Syncer};
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
    /// Upload the store and commit the bucket manifest.
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

#[derive(serde::Deserialize, Default)]
struct ConfigFile {
    #[serde(default)]
    sync: Option<SyncConfig>,
}

fn open(args: &SyncArgs) -> Result<(Syncer, Vec<TableDecl>)> {
    let l = locate(args.project.as_deref(), args.declaration.clone())?;
    let store = Store::open(&l.project.dir, &l.project.name)?;
    let config_path = store.root().join("config.toml");
    let text = std::fs::read_to_string(&config_path).with_context(|| format!("reading `{}`", config_path.display()))?;
    let file: ConfigFile = toml::from_str(&text).with_context(|| format!("`{}`", config_path.display()))?;
    let config = file.sync.with_context(|| format!("`{}` declares no `[sync]`", config_path.display()))?;
    let Some(dir) = config.endpoint.strip_prefix("file://") else {
        bail!("endpoint `{}`: this build links the filesystem bucket adapter alone, `file://<directory>`", config.endpoint);
    };
    let bucket = FsBucket::open(std::path::Path::new(dir), &config.bucket).map_err(|e| anyhow::anyhow!("bucket: {e}"))?;
    let prefix = config.resolve_prefix(|k| std::env::var(k).ok())?;
    let (node_id, _) = node::resolve(&store, |k| std::env::var(k).ok())?;
    let decls = match std::fs::read_to_string(&l.declaration) {
        Ok(t) => TableDecl::parse_pipeline(&t)?,
        Err(_) => Vec::new(),
    };
    let syncer = Syncer { store, bucket: Arc::new(bucket), config, prefix, project: l.project.name, node: node_id.to_string() };
    Ok((syncer, decls))
}

pub fn run(cmd: SyncCmd) -> Result<()> {
    match cmd {
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
            let r = s.push(now(&args.now)?)?;
            for refusal in &r.refused {
                eprintln!("warning: {refusal}");
            }
            println!("pushed {} objects; the manifest lists {} entries after {} round(s)", r.uploaded.len(), r.entries, r.rounds);
            Ok(())
        }
        SyncCmd::Pull { args, tables } => {
            let (s, decls) = open(&args)?;
            let replicate_off = decls.iter().filter(|d| d.replicate == Some(false)).map(|d| d.name.clone()).collect();
            let r = s.pull(&PullScope { tables, replicate_off })?;
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
    let outcome = fold(&s.store, &decl, at)?;
    let FoldOutcome::Folded { snapshot_id, .. } = &outcome else { return Ok(outcome) };
    let published = s.push(at).and_then(|_| s.publish(table, snapshot_id, lease));
    if let Err(e) = published {
        match before {
            Some(b) => contextful_context::store::replace_file(&pointer_path, &b)?,
            None => {
                let _ = std::fs::remove_file(&pointer_path);
            }
        }
        return Err(e.into());
    }
    // The local pointer carries the fence it was published under, as a pulled one does.
    let published = Pointer { snapshot_id: serde_json::from_value(serde_json::Value::String(snapshot_id.clone()))?, fence: Some(lease.lease.fence) };
    contextful_context::store::replace_file(&pointer_path, &serde_json::to_vec_pretty(&published)?)?;
    Ok(outcome)
}
