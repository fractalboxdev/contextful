//! `contextful query` — the operator's raw statement verb.
//!
//! A thin adapter: operator text runs raw (`read.guard.statement-provenance`) through the
//! read face's one response serializer, and prints that projection. The verb has no
//! counterpart on the tool protocol or any network face.

use anyhow::{Context, Result};
use crate::admit::open_face;
use contextful_context::read::{operator_query, ReadOptions};
use contextful_context::Store;
use contextful_policy::enforce::mask::Pepper;
use std::path::PathBuf;

/// The manifest read when `--declaration` is absent.
const DEFAULT_DECLARATION: &str = "contextful.toml";

#[derive(clap::Args)]
pub struct QueryArgs {
    /// Print the JSON response projection: `columns`, `rows`, `truncated`.
    #[arg(long, required = true)]
    json: bool,
    /// Register the tables of the store at `.contextful/context/<project>/` under their bare names.
    #[arg(long)]
    project: Option<String>,
    /// The pipeline manifest; absent reads `contextful.toml` when present.
    #[arg(long, requires = "project")]
    declaration: Option<PathBuf>,
    /// Deliver at most this many rows, setting `truncated` when more exist.
    #[arg(long)]
    limit: Option<u64>,
    /// The statement, run raw.
    sql: String,
}

/// The project manifest's path and text: the named file, else `contextful.toml` when present.
fn manifest(declaration: Option<PathBuf>) -> Result<(PathBuf, String)> {
    let Some(path) = declaration else {
        let text = match std::fs::read_to_string(DEFAULT_DECLARATION) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            other => other.with_context(|| format!("reading the declaration `{DEFAULT_DECLARATION}`"))?,
        };
        return Ok((PathBuf::from(DEFAULT_DECLARATION), text));
    };
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading the declaration `{}`", path.display()))?;
    Ok((path, text))
}

pub fn run(args: QueryArgs) -> Result<()> {
    let opts = ReadOptions { limit: args.limit, ..ReadOptions::default() };
    let response = match args.project {
        Some(project) => {
            let store = Store::open(&std::env::current_dir()?, &project)?;
            let (path, text) = manifest(args.declaration)?;
            let face = open_face(store, &path, &text, Pepper::resolve(|k| std::env::var(k).ok()))?;
            face.operator_query(&args.sql, opts)?
        }
        None => operator_query(&args.sql, opts)?,
    };
    println!("{}", serde_json::to_string(&response.to_json())?);
    Ok(())
}
