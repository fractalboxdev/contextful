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

/// Choose a rebuildable catalog without a plaintext file for a bound encrypted store.
pub fn derived_catalog(path: &Path, encrypted: bool) -> Result<DerivedSqlite> {
    if encrypted {
        Ok(DerivedSqlite::open_ephemeral(path)?)
    } else {
        Ok(DerivedSqlite::open(path)?)
    }
}

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
    /// Resolve one citation through Read admission and the canonical tool/audit owner.
    Reference {
        table: String,
        run: String,
        seq: i64,
        #[command(flatten)]
        store: StoreArgs,
        #[command(flatten)]
        admit: AdmitArgs,
        #[arg(long)]
        issuer_key: Option<PathBuf>,
        #[arg(long)]
        max_duration_ms: Option<u64>,
        #[arg(long)]
        max_response_bytes: Option<u64>,
        #[arg(long)]
        json: bool,
    },
    /// Erase an explicitly admitted subject or declared key set through one signed publication.
    Erase {
        #[command(flatten)]
        store: StoreArgs,
        /// Resume an authenticated committed frontier without selecting or signing another erasure.
        #[arg(long)]
        recover: bool,
        #[arg(long, required_unless_present_any = ["keyset_file", "recover"], conflicts_with_all = ["keyset_file", "recover"])]
        subject: Option<String>,
        #[arg(long = "key-set", alias = "keyset-file", conflicts_with = "recover")]
        keyset_file: Option<PathBuf>,
        #[arg(long, value_delimiter = ',', required_unless_present_any = ["recover", "keyset_file"], conflicts_with = "recover")]
        tables: Vec<String>,
        /// Explicit configured signing port; verification pins remain independent.
        #[arg(long, required_unless_present = "recover", conflicts_with = "recover")]
        issuer_key: Option<PathBuf>,
        #[command(flatten)]
        admit: AdmitArgs,
        #[arg(long)]
        json: bool,
    },
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
        let store = crate::project::with_erasure_keys(Store::open_declared(&l.project.dir, &l.project.name, &l.declaration)?, &l.project, None, None)?;
        let text = std::fs::read_to_string(&l.declaration)
            .with_context(|| format!("reading the declaration `{}`", l.declaration.display()))?;
        let decls = TableDecl::parse_declaration_set(&text, &crate::project::pipeline_files(&l.declaration)?).with_context(|| format!("`{}`", l.declaration.display()))?;
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

fn erasure_boundary<'a>(project: &contextful_context::project::Project, admit: &'a AdmitArgs)
    -> impl Fn(&contextful_policy::verify::AdmittedAuthority) -> std::result::Result<(), contextful_core::AuthorityError> + 'a {
    use contextful_core::ports::Clock;
    let ledger = crate::admit::LedgerFile::at(&project.dir, admit.keyset.as_deref());
    move |authority| {
        let state = ledger.read().map_err(|error| contextful_core::AuthorityError::KeySetUnavailable(error.to_string()))?;
        let revocation = crate::admit::revocation_state(admit.denylist.as_deref(), &state)
            .map_err(|error| contextful_core::AuthorityError::KeySetUnavailable(error.to_string()))?;
        contextful_policy::verify::effect_boundary(authority, &contextful_policy::verify::Admission::new(crate::clock::SystemClock.now(), &revocation))
    }
}

fn run_erasure_recovery(args: &StoreArgs, admit: &AdmitArgs, json: bool) -> Result<()> {
    let located = locate(args.project.as_deref(), args.declaration.clone())?;
    let (authority, _) = admit.admit(args.project.as_deref(), "context erase recovery")?;
    let store = crate::project::open_store(&located.project, admit.public_key.as_deref(), admit.keyset.as_deref())?;
    let transaction = contextful_context::erase::recover_admitted_erasure(&store, &authority, &erasure_boundary(&located.project, admit))?;
    if json { println!("{}", serde_json::json!({"transaction_id":transaction,"physical_collection":"complete"})); }
    else if let Some(transaction) = transaction { println!("erasure {transaction}: physical collection complete"); }
    else { println!("unpublished erasure replacements: physical collection complete"); }
    Ok(())
}

fn run_erasure(args: &StoreArgs, subject: Option<&str>, keyset_file: Option<&Path>, tables: &[String], issuer_key: &Path, admit: &AdmitArgs, json: bool) -> Result<()> {
    use contextful_context::erase::{erase, EraseRequest, EraseSelector};
    use contextful_core::disclosure::erase::RetainedRows;
    use contextful_policy::enforce::erase::ForgetAdmission;
    use contextful_policy::issue::SeedSigner;
    let located = locate(args.project.as_deref(), args.declaration.clone())?;
    let text = std::fs::read_to_string(&located.declaration)?;
    let declarations = TableDecl::parse_declaration_set(&text, &crate::project::pipeline_files(&located.declaration)?)?;
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct TableKeys { subject_hash: String, keys: Vec<KeyItem> }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ColumnKeys { subject_hash: String, column: String, keys: Vec<serde_json::Value> }
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum KeyFile { Tables(TableKeys), Column(ColumnKeys) }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct KeyItem { table: String, key: Option<serde_json::Value>, keys: Option<serde_json::Map<String, serde_json::Value>> }
    let key_file = keyset_file.map(|path| -> Result<KeyFile> { Ok(serde_json::from_slice(&std::fs::read(path)?)?) }).transpose()?;
    let mut keys = RetainedRows::new();
    let mut selected_tables = tables.to_vec();
    if let Some(KeyFile::Column(file)) = &key_file {
        if !tables.is_empty() { bail!("ErasureScopeUnsupported: a column key set requires declaration-derived scope"); }
        selected_tables = contextful_core::disclosure::erase::column_key_set(&declarations, &file.column, &file.keys)?.into_keys().collect();
    }
    if let Some(KeyFile::Tables(file)) = &key_file {
        for item in &file.keys {
            let decl = declarations.iter().find(|decl| decl.name == item.table).context("ErasureScopeUnsupported: key selector names an undeclared table")?;
            let key = match (&item.key, &item.keys) {
                (Some(value), None) => {
                    let column = decl.erasure_key.as_ref().or_else(|| decl.primary_key.as_ref().filter(|columns| columns.len() == 1).map(|columns| &columns[0]))
                        .context("ErasureScopeUnsupported: a composite key needs its complete keys object")?;
                    serde_json::Map::from_iter([(column.clone(), value.clone())])
                }
                (None, Some(values)) => values.clone(),
                _ => bail!("ErasureScopeUnsupported: select exactly one key or keys object"),
            };
            keys.entry(item.table.clone()).or_default().push(key);
        }
    }
    let selector = match (subject, &key_file) {
        (Some(subject), None) => EraseSelector::Subject(subject),
        (None, Some(KeyFile::Tables(file))) => EraseSelector::KeySet { subject_hash: &file.subject_hash, keys: &keys },
        (None, Some(KeyFile::Column(file))) => EraseSelector::ColumnKeySet { subject_hash: &file.subject_hash, column: &file.column, keys: &file.keys },
        _ => bail!("ErasureScopeUnsupported: select one subject or key set"),
    };
    let (authority, _) = admit.admit(args.project.as_deref(), "context erase")?;
    let names = declarations.iter().map(|decl| decl.name.as_str()).collect::<Vec<_>>();
    let admitted = ForgetAdmission::admit(&authority, &names)?;
    let store = crate::project::open_store(&located.project, admit.public_key.as_deref(), admit.keyset.as_deref())?;
    let signer = std::sync::Arc::new(SeedSigner::resolve(Some(issuer_key))?);
    let clock = crate::clock::SystemClock;
    let boundary = erasure_boundary(&located.project, admit);
    let audit_key = contextful_context::project::audit_key(&store, &located.project)?;
    let audit_dir = located.project.audit_dir();
    let erased = erase(&store, EraseRequest { declarations: &declarations, tables: &selected_tables, selector, admission: &admitted,
        signer: Some(signer), audit_dir: &audit_dir, audit_key: &audit_key, boundary: &boundary, clock: &clock })?;
    let receipt = serde_json::json!({ "transaction_id": erased.transaction_id, "subject_hash": erased.subject_hash,
        "affected_counts": erased.affected_counts, "physical_collection": "complete" });
    if json { println!("{}", serde_json::to_string(&receipt)?); }
    else { println!("erasure {}: physical collection complete", erased.transaction_id); }
    Ok(())
}

pub fn run(cmd: ContextCmd) -> Result<()> {
    match cmd {
        ContextCmd::Reference { table, run, seq, store, admit, issuer_key, max_duration_ms, max_response_bytes, json:_ } => {
            use contextful_core::ports::Clock;
            use contextful_policy::verify::{effect_boundary, Admission, AdmittedAuthority};
            let (authority, revocation) = admit.admit(store.project.as_deref(), "context reference")?;
            let located = locate(store.project.as_deref(), store.declaration)?;
            let face = crate::admit::face_with_pins(&located, admit.public_key.as_deref(), admit.keyset.as_deref())?;
            let audit = crate::project::read_audit(&located.project, issuer_key.as_deref(), admit.public_key.as_deref(), admit.keyset.as_deref())?;
            let clock = crate::clock::SystemClock;
            let boundary = |authority: &AdmittedAuthority| effect_boundary(authority, &Admission::new(clock.now(), &revocation));
            let server = contextful_agent::mcp::Server::new(&face, authority, &boundary, &clock, &audit).map_err(anyhow::Error::msg)?;
            let mut arguments = serde_json::json!({ "table":table, "run":run, "seq":seq });
            if let Some(value) = max_duration_ms { arguments["max_duration_ms"] = value.into(); }
            if let Some(value) = max_response_bytes { arguments["max_response_bytes"] = value.into(); }
            let message = serde_json::json!({ "jsonrpc":"2.0", "id":1, "method":"tools/call", "params": { "name":"context.reference", "arguments":arguments } });
            let answer = server.handle(&message.to_string()).ok_or_else(|| anyhow::anyhow!("the reference request has no answer"))?;
            if answer.get("error").is_some() || answer["result"]["isError"] == true { bail!("{}", answer); }
            let value = &answer["result"]["structuredContent"];
            println!("{}", serde_json::to_string(value)?);
            Ok(())
        }
        ContextCmd::Erase { store, recover, subject, keyset_file, tables, issuer_key, admit, json } => {
            if recover { run_erasure_recovery(&store, &admit, json) }
            else { run_erasure(&store, subject.as_deref(), keyset_file.as_deref(), &tables, issuer_key.as_deref().context("erasure has no explicit signing port")?, &admit, json) }
        }
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
            let store = crate::project::open_store(&l.project, None, None)?;
            let catalog = derived_catalog(&store.root().join(DERIVED_CATALOG_FILE), store.encrypted())?;
            let rows = rebuild(&store, &catalog)?;
            println!("{}", serde_json::to_string_pretty(&rows)?);
            Ok(())
        }
    }
}
