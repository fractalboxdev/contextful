//! `contextful visibility` — the mirrored permission state of a declared source
//! (`disclosure.sweep.sweep-verb`).
//!
//! A thin adapter: it reads the declaration set's visibility blocks and `[[acl_sweep]]` blocks,
//! the `--grants` file and the `--directory` file, and calls the store adapter, which computes
//! and lands the run.

use crate::clock::SystemClock;
use crate::project::locate;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use contextful_context::visibility::{sweep_run, Landing};
use contextful_context::{node, Store};
use contextful_core::disclosure::declare::Binding;
use contextful_core::disclosure::sweep::{parse_directory, parse_grants, SweepBlock, SweepKind};
use contextful_core::ports::Clock;
use contextful_core::run::record::SiteIdSources;
use contextful_core::store::declare::TableDecl;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum VisibilityCmd {
    /// Run one sweep of a declared source and commit what it read as one run.
    Sweep {
        /// The project whose store root is `.contextful/context/<project>/` under the working
        /// directory; absent, the nearest `contextful.toml` upward names it.
        #[arg(long)]
        project: Option<String>,
        /// The declaration whose set (it and `pipelines/*.toml` beside it) holds the
        /// `[[acl_sweep]]` block; absent, the project's `contextful.toml`.
        #[arg(long)]
        declaration: Option<PathBuf>,
        /// The `source` of an `[[acl_sweep]]` block.
        #[arg(long)]
        source: String,
        /// `full` or `incremental`.
        #[arg(long, value_parser = ["full", "incremental"])]
        kind: String,
        /// JSON Lines, one grant per line: the run's read of every governed resource.
        #[arg(long)]
        grants: Option<PathBuf>,
        /// JSON Lines from the identity provider's directory: identity links and group
        /// memberships, replacing the source's current ones; absent, they stand.
        #[arg(long)]
        directory: Option<PathBuf>,
        /// The site id the run carries; absent, the manifest's, else the node id.
        #[arg(long)]
        site_id: Option<String>,
    },
}

pub fn run(cmd: VisibilityCmd) -> Result<()> {
    match cmd {
        VisibilityCmd::Sweep { project, declaration, source, kind, grants, directory, site_id } => {
            let kind = SweepKind::parse(&kind).context("`--kind` is `full` or `incremental`")?;
            let l = locate(project.as_deref(), declaration)?;
            let text = std::fs::read_to_string(&l.declaration).with_context(|| format!("reading the declaration `{}`", l.declaration.display()))?;
            let blocks = SweepBlock::parse_set(&crate::project::manifests(&l.declaration)?)?;
            let Some(block) = blocks.into_iter().find(|b| b.source == source) else {
                bail!("`{}` and its `pipelines/` declare no `[[acl_sweep]]` block for source `{source}`", l.declaration.display());
            };
            let Some(grants) = grants else {
                bail!("source `{source}`: reading the permission endpoint under `credential` takes a connector; pass `--grants <file>`");
            };
            let decls = TableDecl::parse_declaration_set(&text, &crate::project::pipeline_files(&l.declaration)?).with_context(|| format!("`{}`", l.declaration.display()))?;
            let mut bindings = Vec::new();
            for d in &decls {
                bindings.extend(Binding::of(d)?);
            }
            let store = crate::project::with_erasure_keys(Store::open_declared(&l.project.dir, &l.project.name, &l.declaration)?, &l.project, None, None)?;
            let (node, _) = node::resolve(&store, |k| std::env::var(k).ok())?;
            let site_id = match site_id {
                Some(id) => id,
                None => {
                    let declared = SiteIdSources::from_manifest(&text, |k| std::env::var(k).ok()).with_context(|| format!("`{}`", l.declaration.display()))?;
                    if declared.manifest.is_none() && declared.env.is_none() {
                        node.as_str().to_string()
                    } else {
                        crate::project::site_id_for(&text, &l.declaration, None, None)?
                    }
                }
            };
            let began_at = SystemClock.now();
            let lines = std::fs::read_to_string(&grants).with_context(|| format!("reading `{}`", grants.display()))?;
            let lines = parse_grants(&lines).with_context(|| format!("`{}`", grants.display()))?;
            let directory = match directory {
                Some(path) => {
                    let text = std::fs::read_to_string(&path).with_context(|| format!("reading `{}`", path.display()))?;
                    Some(parse_directory(&source, &text).with_context(|| format!("`{}`", path.display()))?)
                }
                None => None,
            };
            let c = sweep_run(&store, &Landing { node, site_id }, &block, &bindings, kind, lines, directory, began_at, SystemClock.now())?;
            let f = &c.freshness;
            println!(
                "{source}: {} sweep {} committed epoch {} ({} resources, {} grants, {} group members, {} identity links, {} tombstones); watermark_at {}",
                kind.as_str(),
                f.sweep_id,
                f.epoch,
                c.resources.len(),
                c.grants.len(),
                c.members.len(),
                c.links.len(),
                c.tombstones.len(),
                f.watermark_at.map_or_else(|| "unset".to_string(), |w| w.to_rfc3339())
            );
            Ok(())
        }
    }
}
