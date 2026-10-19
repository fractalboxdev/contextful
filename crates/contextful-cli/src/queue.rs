//! `contextful queue drain` and the post-commit wake-up: the project's local spool behind
//! the `Queue` port (`topology.compose.queue-extension`).
//!
//! A published manifest sends one `committed` wake-up naming its table; the drain wakes
//! each export reading a woken table and fires each object read an arrival key covers, then
//! acknowledges what it settled. A wake reads from the export's cursor or the source's held
//! position, so a lost, repeated or reordered message delays work and lands no row twice
//! (`run.export.queue-wake`, `run.land.object-arrival-wake`).

use crate::admit::AdmitArgs;
use crate::run::ProjectArgs;
use anyhow::{Context, Result};
use clap::Subcommand;
use contextful_connectors::object::{ObjectConfig, NAME as OBJECT_SOURCE};
use contextful_context::Store;
use contextful_core::export::parse_exports;
use contextful_core::pipeline::declare::collect;
use contextful_core::run::queue::{Queue, QueueMessage};
use contextful_core::run::{Failure, FailureTag};
use contextful_core::store::StoreError;
use contextful_engine::queue::{drain, LocalSpool, Wake};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The deliveries one drain receives when `--max` is absent.
const DRAIN_MAX: usize = 100;

#[derive(Subcommand)]
pub enum QueueCmd {
    /// Wake the exports and object reads the project's queued messages name, then acknowledge them.
    Drain {
        #[command(flatten)]
        project: ProjectArgs,
        /// The project manifest; absent, the project's `contextful.toml`.
        #[arg(long)]
        declaration: Option<PathBuf>,
        /// The most deliveries one drain receives.
        #[arg(long, default_value_t = DRAIN_MAX)]
        max: usize,
        #[command(flatten)]
        admit: AdmitArgs,
    },
}

/// The spool beside a store root `.contextful/context/<project>/`:
/// `.contextful/queue/<project>/`, machine-local and outside corpus sync.
pub(crate) fn spool_dir(store_root: &Path) -> Option<PathBuf> {
    let project = store_root.file_name()?;
    Some(store_root.parent()?.parent()?.join("queue").join(project))
}

/// Send the wake-up for a manifest `run` published on `table`; a store sealing its run
/// state spools no run id. A wake-up that fails to spool is lost, which delays an export to
/// its next wake and skips no row.
pub(crate) fn announce(store: &Store, table: &str, run: &str, commit_seq: Option<i64>) {
    let Some(dir) = spool_dir(store.root()) else { return };
    let run = store.file_cipher().is_none().then(|| run.to_string());
    let message = QueueMessage::Committed { table: table.to_string(), commit_seq, run };
    if let Err(e) = LocalSpool::open(&dir).and_then(|spool| spool.send(&message)) {
        eprintln!("the wake-up for `{table}` did not spool: {e}");
    }
}

pub fn run(cmd: QueueCmd) -> Result<()> {
    match cmd {
        QueueCmd::Drain { project, declaration, max, admit } => {
            let l = project.locate(declaration.clone())?;
            let dir = spool_dir(&l.project.store_root()).context("the project's store root names no queue spool")?;
            let spool = LocalSpool::open(&dir)?;
            let mut wake = Woken { project: &project, declaration, located: l.declaration.clone(), admit: &admit };
            let drained = drain(&spool, max, &mut wake).map_err(|f| match f.tag {
                FailureTag::Transient => anyhow::anyhow!("{}; retryable, its deliveries stay queued for the next drain", f.message),
                _ => anyhow::anyhow!("{}; its deliveries stay queued", f.message),
            })?;
            println!("drained {} wake-ups and {} arrivals · {} left queued", drained.wakes, drained.arrivals, drained.left);
            Ok(())
        }
    }
}

/// The project's declared exports and pipelines as the reads a drain wakes.
struct Woken<'a> {
    project: &'a ProjectArgs,
    declaration: Option<PathBuf>,
    located: PathBuf,
    admit: &'a AdmitArgs,
}

/// A wake's failure. A push guard held past its wait (`SyncPushInFlight`) is retryable, and
/// no failure acknowledges the deliveries it woke from.
fn failed(e: anyhow::Error) -> Failure {
    let held = e.chain().any(|c| {
        matches!(c.downcast_ref::<StoreError>(), Some(StoreError::SyncPushInFlight(_)))
            || matches!(c.downcast_ref::<contextful_context::ContextError>(), Some(contextful_context::ContextError::Store(StoreError::SyncPushInFlight(_))))
            || matches!(c.downcast_ref::<contextful_sync::SyncError>(), Some(contextful_sync::SyncError::Store(StoreError::SyncPushInFlight(_))))
    });
    Failure::new(if held { FailureTag::Transient } else { FailureTag::Permanent }, format!("{e:#}"))
}

impl Woken<'_> {
    fn manifest(&self) -> Result<String> {
        if !self.located.exists() {
            return Ok(String::new());
        }
        std::fs::read_to_string(&self.located).with_context(|| format!("reading the declaration `{}`", self.located.display()))
    }

    /// Fire pipeline `id` through this executable, as a scheduled fire does.
    fn fire(&self, id: &str) -> Result<()> {
        let mut child = std::process::Command::new(std::env::current_exe()?);
        child.args(["pipeline", "run", id]);
        if let Some(p) = &self.project.project {
            child.args(["--project", p]);
        }
        if let Some(now) = &self.project.now {
            child.args(["--now", now]);
        }
        if let Some(d) = &self.declaration {
            child.arg("--declaration").arg(d);
        }
        let out = child.stdout(std::process::Stdio::inherit()).stderr(std::process::Stdio::piped()).output().with_context(|| format!("firing pipeline `{id}`"))?;
        let said = String::from_utf8_lossy(&out.stderr);
        eprint!("{said}");
        if out.status.success() {
            return Ok(());
        }
        // The fire's own pull met a held push guard: the arrival stays queued, retryable.
        match said.lines().find_map(|l| l.split_once("SyncPushInFlight: ").map(|(_, m)| m.to_string())) {
            Some(held) => Err(anyhow::Error::new(StoreError::SyncPushInFlight(held)).context(format!("the fire of pipeline `{id}` exited {}", out.status))),
            None => anyhow::bail!("the fire of pipeline `{id}` exited {}", out.status),
        }
    }
}

impl Wake for Woken<'_> {
    fn exports(&mut self, tables: &BTreeSet<String>) -> Result<(), Failure> {
        let text = self.manifest().map_err(failed)?;
        let mut first = None;
        for export in parse_exports(&text).map_err(|e| failed(e.into()))?.into_iter().filter(|e| tables.contains(&e.table)) {
            if let Err(e) = crate::export::run_once(&export.name, self.project, self.declaration.clone(), self.admit) {
                eprintln!("export `{}`: {e:#}", export.name);
                first.get_or_insert(e);
            }
        }
        first.map_or(Ok(()), |e| Err(failed(e)))
    }

    fn arrivals(&mut self, arrivals: &BTreeSet<(String, String)>) -> Result<(), Failure> {
        let files = if self.located.exists() { crate::project::manifests(&self.located).map_err(failed)? } else { Vec::new() };
        let mut first = None;
        for declared in collect(&files).map_err(|e| failed(e.into()))? {
            let spec = declared.spec;
            if spec.source.name != OBJECT_SOURCE {
                continue;
            }
            let Ok(config) = ObjectConfig::parse(&spec.source.config) else { continue };
            if !arrivals.iter().any(|(bucket, key)| *bucket == config.bucket && config.address.covers(key)) {
                continue;
            }
            if let Err(e) = self.fire(&spec.id) {
                eprintln!("{e:#}");
                first.get_or_insert(e);
            }
        }
        first.map_or(Ok(()), |e| Err(failed(e)))
    }
}
