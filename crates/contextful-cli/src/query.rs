//! `contextful query` — the operator's raw statement verb.
//!
//! A thin adapter: operator text runs raw (`read.guard.statement-provenance`) through the
//! read face's one response serializer, and prints that projection. The verb has no
//! counterpart on the tool protocol or any network face.

use crate::project::Located;
use anyhow::{Context, Result};
use contextful_context::read::{operator_query, Face, ReadOptions};
use contextful_context::{ContextError, Store};
use contextful_core::store::StoreError;
use contextful_policy::enforce::mask::Pepper;
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub struct QueryArgs {
    /// Print the JSON response projection: `columns`, `rows`, `truncated`.
    #[arg(long, required = true)]
    json: bool,
    /// Register the tables of the store at `.contextful/context/<project>/` under their bare
    /// names; absent registers the project the nearest `contextful.toml` names, if any.
    #[arg(long)]
    project: Option<String>,
    /// The pipeline manifest; absent reads the project's `contextful.toml` when present.
    #[arg(long)]
    declaration: Option<PathBuf>,
    /// Deliver at most this many rows, setting `truncated` when more exist.
    #[arg(long)]
    limit: Option<u64>,
    /// The statement, run raw.
    sql: String,
}

/// The manifest text: the located declaration, or none when the default one is absent
/// (`read.query.declaration-default`).
fn manifest(path: &Path, named: bool) -> Result<String> {
    match std::fs::read_to_string(path) {
        Err(e) if !named && e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        other => other.with_context(|| format!("reading the declaration `{}`", path.display())),
    }
}

/// The verb's project: `--project`, else the one discovery finds
/// (`read.query.discovered-project`). An undiscovered project is `None` unless a
/// `--declaration` asks for one (`store.init.undiscovered`).
fn locate(project: Option<&str>, declaration: Option<PathBuf>) -> Result<Option<Located>> {
    let named = declaration.is_some();
    match crate::project::locate(project, declaration) {
        Ok(located) => Ok(Some(located)),
        Err(e) if !named && undiscovered(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

fn undiscovered(e: &anyhow::Error) -> bool {
    let store = e.downcast_ref::<ContextError>().and_then(ContextError::store).or_else(|| e.downcast_ref::<StoreError>());
    matches!(store, Some(StoreError::StoreProjectUndiscovered(_)))
}

pub fn run(args: QueryArgs) -> Result<()> {
    let opts = ReadOptions { limit: args.limit, ..ReadOptions::default() };
    let named = args.declaration.is_some();
    let response = match locate(args.project.as_deref(), args.declaration)? {
        Some(Located { project, declaration }) => {
            let store = Store::open(&project.dir, &project.name)?;
            let face = Face::open(store, &manifest(&declaration, named)?, Pepper::resolve(|k| std::env::var(k).ok()))?;
            face.operator_query(&args.sql, opts)?
        }
        None => operator_query(&args.sql, opts)?,
    };
    println!("{}", serde_json::to_string(&response.to_json())?);
    Ok(())
}
