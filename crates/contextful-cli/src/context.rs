//! `contextful context` — the store surface of the command line.
//!
//! Each subcommand is a thin adapter: it reads the declaration and flags, calls the
//! store adapter, and prints. No store rule lives here.

use crate::admit::{AdmitArgs, Author};
use crate::project::locate;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use contextful_context::catalog::rebuild;
use contextful_context::fold::fold;
use contextful_context::land::{land_run, Batch, Position, RunContext};
use contextful_context::scan::scan;
use contextful_context::{node, ContextError, Store};
use contextful_core::store::bound_time::{Bound, Bounds};
use contextful_core::store::catalog::DERIVED_CATALOG_FILE;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::fold::{scheduled, FoldOutcome};
use contextful_core::store::reconcile::ColumnType;
use contextful_core::store::reserve::Injection;
use contextful_core::time::Instant;
use contextful_sqlite::DerivedSqlite;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub struct StoreArgs {
    /// The project whose store root is `.contextful/context/<project>/` under the working
    /// directory; absent, the nearest `contextful.toml` upward names it.
    #[arg(long)]
    project: Option<String>,
    /// The pipeline manifest holding the `[[pipeline.tables]]` declarations; absent, the
    /// project's `contextful.toml`.
    #[arg(long)]
    declaration: Option<PathBuf>,
}

#[derive(Subcommand)]
pub enum ContextCmd {
    /// Land a JSON Lines batch into a table as one committed run.
    Land {
        table: String,
        #[command(flatten)]
        store: StoreArgs,
        /// JSON Lines, one object per row.
        #[arg(long)]
        rows: PathBuf,
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        site_id: String,
        /// Repeatable `<column>=<type>`, fixing a column's type instead of reading it off the values.
        #[arg(long = "type")]
        types: Vec<String>,
        /// Commit instant (RFC 3339); absent reads the system clock.
        #[arg(long)]
        now: Option<String>,
        #[command(flatten)]
        admit: AdmitArgs,
    },
    /// Print a table's data files, relative to the store root, one per line.
    Files {
        table: String,
        #[command(flatten)]
        store: StoreArgs,
        /// Transaction-time bound: an RFC 3339 instant, or a date meaning before the next day.
        #[arg(long)]
        as_of: Option<String>,
    },
    /// Print a table's resolved file list, relation and bounds as JSON.
    Scan {
        table: String,
        #[command(flatten)]
        store: StoreArgs,
        #[arg(long)]
        as_of: Option<String>,
        /// Valid-time bound over the table's declared pair.
        #[arg(long)]
        valid_as_of: Option<String>,
    },
    /// Fold a table's committed runs into a new snapshot; with no table, every table whose trigger fired.
    Compact {
        table: Option<String>,
        #[command(flatten)]
        store: StoreArgs,
        #[arg(long)]
        now: Option<String>,
    },
    /// Reconstruct `derived.sqlite` from the tree and print the rows it now holds as JSON.
    RebuildCatalog {
        /// The project whose store root is `.contextful/context/<project>/` under the working
        /// directory; absent, the nearest `contextful.toml` upward names it.
        #[arg(long)]
        project: Option<String>,
    },
}

fn now(flag: Option<String>) -> Result<Instant> {
    match flag {
        Some(s) => Ok(Instant::parse(&s)?),
        None => {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
            Ok(Instant::from_unix_nanos(i128::try_from(nanos)?)?)
        }
    }
}

struct Opened {
    store: Store,
    decls: Vec<TableDecl>,
    /// The declaration's text.
    manifest: String,
}

impl Opened {
    fn open(args: &StoreArgs) -> Result<Opened> {
        let l = locate(args.project.as_deref(), args.declaration.clone())?;
        let store = Store::open(&l.project.dir, &l.project.name)?;
        let text = std::fs::read_to_string(&l.declaration)
            .with_context(|| format!("reading the declaration `{}`", l.declaration.display()))?;
        let decls = TableDecl::parse_pipeline(&text).with_context(|| format!("`{}`", l.declaration.display()))?;
        Ok(Opened { store, decls, manifest: text })
    }

    /// The table's declaration block; a table with none declares no key.
    fn decl(&self, table: &str) -> TableDecl {
        self.decls.iter().find(|d| d.name == table).cloned().unwrap_or_else(|| TableDecl::named(table))
    }
}

pub(crate) fn read_rows(path: &Path) -> Result<Vec<serde_json::Map<String, serde_json::Value>>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading `{}`", path.display()))?;
    let mut rows = Vec::new();
    for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        match serde_json::from_str(line) {
            Ok(serde_json::Value::Object(m)) => rows.push(m),
            Ok(_) => bail!("{}:{}: a row is a JSON object", path.display(), i + 1),
            Err(e) => bail!("{}:{}: {e}", path.display(), i + 1),
        }
    }
    Ok(rows)
}

fn bound(flag: Option<String>) -> Result<Option<Bound>> {
    Ok(flag.map(|s| Bound::parse(&s)).transpose()?)
}

pub fn run(cmd: ContextCmd) -> Result<()> {
    match cmd {
        ContextCmd::Land { table, store, rows, run_id, site_id, types, now: at, admit } => {
            let o = Opened::open(&store)?;
            let author = admit.author(None, &o.manifest, &[&table])?;
            let (node, _) = node::resolve(&o.store, |k| std::env::var(k).ok())?;
            let mut fixed = HashMap::new();
            for t in types {
                let (col, ty) = t.split_once('=').with_context(|| format!("`--type {t}` is not <column>=<type>"))?;
                let ty = ColumnType::parse(ty).with_context(|| format!("`--type {t}`: unknown type `{ty}`"))?;
                fixed.insert(col.to_string(), ty);
            }
            let batch = Batch { rows: read_rows(&rows)?, types: fixed };
            let ctx = RunContext {
                node,
                injection: Injection { run_id, site_id, batch_seq: Some(0), authored_by: author.as_ref().and_then(Author::on_behalf_of), taint: None },
                committed_at: now(at)?,
            };
            let precommit = || match &author {
                Some(a) => a.boundary().map_err(|e| ContextError::Invalid(format!("{e:#}"))),
                None => Ok(()),
            };
            let l = land_run(&o.store, &o.decl(&table), std::slice::from_ref(&batch), &ctx, &Position::default(), &precommit)?;
            let (m, verb) = (&l.manifest, if l.replay { "replayed" } else { "committed" });
            println!("{table}: {verb} {} on {} ({} parts, {} rows)", m.run_id, m.node_id, m.parts.len(), batch.rows.len());
            Ok(())
        }
        ContextCmd::Files { table, store, as_of } => {
            let o = Opened::open(&store)?;
            let s = scan(&o.store, &o.decl(&table), Bounds { as_of: bound(as_of)?, valid_as_of: None })?;
            for f in s.files {
                println!("{f}");
            }
            Ok(())
        }
        ContextCmd::Scan { table, store, as_of, valid_as_of } => {
            let o = Opened::open(&store)?;
            let s = scan(&o.store, &o.decl(&table), Bounds { as_of: bound(as_of)?, valid_as_of: bound(valid_as_of)? })?;
            let mut out = serde_json::json!({ "files": s.files, "relation": s.relation });
            if let Some(b) = s.bounds {
                out["contextful.bounds"] = b;
            }
            println!("{}", serde_json::to_string_pretty(&out)?);
            Ok(())
        }
        ContextCmd::Compact { table, store, now: at } => {
            let o = Opened::open(&store)?;
            let at = now(at)?;
            let tables = match &table {
                // A named table the tree does not declare halts the command.
                Some(t) => {
                    o.store.schema(t)?;
                    vec![t.clone()]
                }
                None => o.store.tables()?,
            };
            let mut failed = 0;
            for t in tables {
                let decl = o.decl(&t);
                let outcome = match table.is_none().then(|| o.store.state(&decl).map(|s| scheduled(&s, at))) {
                    Some(Ok(false)) => {
                        println!("{t}: not due");
                        continue;
                    }
                    Some(Err(e)) => FoldOutcome::Failed(e.to_string()),
                    Some(Ok(true)) | None => fold(&o.store, &decl, at).unwrap_or_else(|e| FoldOutcome::Failed(e.to_string())),
                };
                if outcome.is_failure() {
                    failed += 1;
                }
                println!("{t}: {outcome}");
            }
            if failed > 0 {
                bail!("{failed} table(s) failed to fold");
            }
            Ok(())
        }
        ContextCmd::RebuildCatalog { project } => {
            let l = locate(project.as_deref(), None)?;
            let store = Store::open(&l.project.dir, &l.project.name)?;
            let catalog = DerivedSqlite::open(&store.root().join(DERIVED_CATALOG_FILE))?;
            let rows = rebuild(&store, &catalog)?;
            println!("{}", serde_json::to_string_pretty(&rows)?);
            Ok(())
        }
    }
}
