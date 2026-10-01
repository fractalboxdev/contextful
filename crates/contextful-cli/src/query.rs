//! `contextful query` — the operator's raw statement verb.
//!
//! A thin adapter: operator text runs raw (`read.guard.statement-provenance`) through the
//! read face's one response serializer, and prints that projection. The verb has no
//! counterpart on the tool protocol or any network face.

use crate::project::{locate, Located};
use anyhow::{Context, Result};
use contextful_context::read::{operator_query, Face, ReadOptions};
use contextful_context::{ContextError, Store};
use contextful_core::store::StoreError;
use contextful_policy::enforce::mask::Pepper;
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct QueryArgs {
    /// Print the JSON response projection: `columns`, `rows`, `truncated`.
    #[arg(long, required = true)]
    json: bool,
    /// Register the tables of the store at `.contextful/context/<project>/` under their
    /// bare names; absent discovers the project from the nearest `contextful.toml`.
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

/// The project the statement reads: named, discovered, or none when neither flag is
/// given and no `contextful.toml` names a project (`read.query.undiscovered-runs-bare`).
fn project(args: &QueryArgs) -> Result<Option<Located>> {
    match locate(args.project.as_deref(), args.declaration.clone()) {
        Ok(located) => Ok(Some(located)),
        Err(e) if args.project.is_none() && args.declaration.is_none() && undiscovered(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

fn undiscovered(e: &anyhow::Error) -> bool {
    matches!(e.downcast_ref::<ContextError>(), Some(ContextError::Store(StoreError::StoreProjectUndiscovered(_))))
}

/// The manifest text; a default declaration absent from disk is no manifest
/// (`read.query.declaration-default`).
fn manifest(located: &Located, explicit: bool) -> Result<String> {
    let path = &located.declaration;
    match std::fs::read_to_string(path) {
        Err(e) if !explicit && e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        other => other.with_context(|| format!("reading the declaration `{}`", path.display())),
    }
}

pub fn run(args: QueryArgs) -> Result<()> {
    let opts = ReadOptions { limit: args.limit, ..ReadOptions::default() };
    let response = match project(&args)? {
        Some(located) => {
            let store = Store::open(&located.project.dir, &located.project.name)?;
            let face = Face::open(store, &manifest(&located, args.declaration.is_some())?, Pepper::resolve(|k| std::env::var(k).ok()))?;
            face.operator_query(&args.sql, opts)?
        }
        None => operator_query(&args.sql, opts)?,
    };
    println!("{}", serde_json::to_string(&response.to_json())?);
    Ok(())
}
