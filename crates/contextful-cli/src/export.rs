//! `contextful export` — the manifest's `[[export]]` blocks on the command line.
//!
//! `run` delivers one export once: it admits the export's credential, reads the rows past
//! the export's cursor through the read face in commit order, posts each batch to the OTLP
//! target through the mediated client, and commits the cursor after each acknowledgement.

use crate::admit::{face, AdmitArgs};
use crate::clock::SystemClock;
use crate::run::ProjectArgs;
use anyhow::{Context, Result};
use clap::Subcommand;
use contextful_context::read::face::ReadOptions;
use contextful_context::store::{replace_file, FileLock, LOCK_WAIT_SECS};
use contextful_core::connector::attach::Allowlist;
use contextful_core::export::{change_events, change_state, log_records, parse_exports, typed_batch_len, typed_cell, Export, ExportCursor, ExportError, Signal, EXPORT_BATCH_ROWS};
use contextful_core::store::catalog::MACHINE_CATALOG_FILE;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::relation::ident;
use contextful_outbound::{Client, HeaderValue};
use contextful_policy::enforce::session::{Request, Session};
use contextful_sqlite::ExportLedger;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Subcommand)]
pub enum ExportCmd {
    /// Deliver every row an export's cursor has not passed to its OTLP target.
    Run {
        name: String,
        #[command(flatten)]
        project: ProjectArgs,
        /// The project manifest holding the export block; absent, the project's `contextful.toml`.
        #[arg(long)]
        declaration: Option<PathBuf>,
        #[command(flatten)]
        admit: AdmitArgs,
    },
    /// Keep scheduled typed exports delivering, retrying the same pending events after failure.
    Watch {
        name: String,
        #[command(flatten)]
        project: ProjectArgs,
        #[arg(long)]
        declaration: Option<PathBuf>,
        #[command(flatten)]
        admit: AdmitArgs,
    },
}

/// The export cursors of one project, outside the synced store root
/// (`run.export.cursor-after-ack`).
fn cursor_dir(project_dir: &Path, project: &str) -> PathBuf {
    project_dir.join(".contextful").join("exports").join(project)
}

fn read_cursor(path: &Path) -> Result<ExportCursor> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| format!("reading the export cursor `{}`", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ExportCursor::default()),
        Err(e) => Err(e).with_context(|| format!("reading the export cursor `{}`", path.display())),
    }
}

pub fn run(cmd: ExportCmd) -> Result<()> {
    match cmd {
        ExportCmd::Run { name, project, declaration, admit } => run_once(&name, &project, declaration, &admit),
        ExportCmd::Watch { name, project, declaration, admit } => {
            let l = project.locate(declaration.clone())?;
            let text = std::fs::read_to_string(&l.declaration)?;
            let export = parse_exports(&text)?.into_iter().find(|e| e.name == name).with_context(|| format!("no export `{name}` is declared"))?;
            if export.signal != Signal::ChangesV1 {
                anyhow::bail!("export `{name}` has no typed delivery schedule");
            }
            let schedule = export.schedule.expect("typed declaration requires schedule");
            let mut retry = 1u64;
            loop {
                match run_once(&name, &project, declaration.clone(), &admit) {
                    Ok(()) => {
                        retry = 1;
                        let now = wall_clock()?;
                        let due = schedule.next_after(now);
                        let wait = now.secs_until(due).max(1) as u64;
                        std::thread::sleep(std::time::Duration::from_secs(wait));
                    }
                    Err(e) => {
                        if matches!(e.downcast_ref::<ExportError>(), Some(ExportError::ExportEventTooLarge(_))) {
                            return Err(e);
                        }
                        eprintln!("export `{name}`: {e:#}; retrying in {retry}s");
                        std::thread::sleep(std::time::Duration::from_secs(retry));
                        retry = retry.saturating_mul(2).min(60);
                    }
                }
            }
        }
    }
}

fn wall_clock() -> Result<contextful_core::time::Instant> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
    Ok(contextful_core::time::Instant::from_unix_nanos(i128::try_from(nanos)?)?)
}

fn run_once(name: &str, project: &ProjectArgs, declaration: Option<PathBuf>, admit: &AdmitArgs) -> Result<()> {
            let l = project.locate(declaration)?;
            let text = std::fs::read_to_string(&l.declaration).with_context(|| format!("reading the declaration `{}`", l.declaration.display()))?;
            let export = parse_exports(&text)
                .with_context(|| l.declaration.display().to_string())?
                .into_iter()
                .find(|e| e.name == name)
                .with_context(|| format!("no export `{name}` is declared"))?;

            if export.signal == Signal::ChangesV1 {
                return run_changes(&l, &export, project, admit);
            }

            // Every header reference resolves before any row is read (`run.export.secret-preflight`).
            let vars: BTreeMap<String, String> = std::env::vars().collect();
            let resolver = contextful_outbound::assemble(&vars, Arc::new(SystemClock))?;
            resolver.preflight(export.headers.values())?;
            let allow = Allowlist::parse(&[export.endpoint.host_str().unwrap_or_default()])?;
            if export.headers.values().any(|t| t.has_reference()) {
                allow.check_bound()?;
            }
            let client = Client::new(allow, export.endpoint.clone()).with_body_limit(64 * 1024);

            let (authority, _) = admit.admit(project.project.as_deref(), "an export")?;
            let face = face(&l)?;
            let types = face.store().try_schema(&export.table)?.map(|s| s.columns.into_iter().map(|c| (c.name, c.ty)).collect()).unwrap_or_default();

            let dir = cursor_dir(&l.project.dir, &l.project.name);
            std::fs::create_dir_all(&dir).with_context(|| format!("creating `{}`", dir.display()))?;
            // One run of an export at a time, so one cursor never passes rows another run holds unsent.
            let _held = FileLock::acquire(&dir.join(format!("{name}.lock")), std::time::Duration::from_secs(LOCK_WAIT_SECS))?;
            let path = dir.join(format!("{name}.json"));
            let mut cursor = read_cursor(&path)?;

            // Committed runs only, under the admitted credential (`run.export.post-commit-read`).
            let session = face.session(&authority, &Request { zone: None }, Bounds::default())?;
            // A row without a commit sequence refuses before any batch leaves (`run.export.commit-seq-missing`).
            let missing = face.query(&session, &export.missing_statement(), ReadOptions::default())?;
            let count = missing.rows.first().and_then(|r| r.first()).and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0);
            if let Some(refusal) = export.missing(count) {
                return Err(refusal.into());
            }
            let (mut rows, mut batches) = (0usize, 0usize);
            loop {
                let opts = ReadOptions { limit: Some(EXPORT_BATCH_ROWS as u64), ..ReadOptions::default() };
                let response = face.query(&session, &export.batch_statement(&cursor), opts)?;
                if response.rows.is_empty() {
                    break;
                }
                let (body, last) = log_records(&export, &response.columns, &response.rows, &types)?;
                let Some(last) = last else { break };
                let mut headers = vec![("Content-Type".to_string(), HeaderValue::Plain("application/json".to_string()))];
                for (header, t) in &export.headers {
                    let v = resolver.render(t)?;
                    headers.push((header.clone(), if t.has_reference() { HeaderValue::Sensitive(v.into()) } else { HeaderValue::Plain(v.reveal().to_string()) }));
                }
                let bytes = serde_json::to_vec(&body)?;
                let refused = |why: String| ExportError::ExportDeliveryRefused(format!("export `{name}`: {why}; the cursor stays at {}.{}", cursor.commit_seq, cursor.row_seq));
                match client.send("POST", &export.endpoint, &headers, Some(&bytes)) {
                    Ok(r) if (200..300).contains(&r.status) => {}
                    Ok(r) => return Err(refused(format!("the target answered {}", r.status)).into()),
                    Err(f) => return Err(refused(f.message).into()),
                }
                // The acknowledgement precedes the commit (`run.export.cursor-after-ack`).
                replace_file(&path, &serde_json::to_vec(&last)?)?;
                cursor = last;
                rows += response.rows.len();
                batches += 1;
            }
            println!("{name}: delivered {rows} rows in {batches} batches · cursor at commit {} row {}", cursor.commit_seq, cursor.row_seq);
            Ok(())
}

fn source_publication(face: &contextful_context::read::face::Face, table: &str) -> Result<String> {
    let store = face.store();
    let mut runs = store.committed_runs(table)?;
    runs.sort_by(|a, b| (a.commit_seq, &a.run_id, &a.node_id).cmp(&(b.commit_seq, &b.run_id, &b.node_id)));
    let pointer = store.pointer(table)?.map(|(p, _)| p.snapshot_id.to_string());
    let material = serde_json::to_vec(&(pointer, runs))?;
    Ok(format!("{:x}", Sha256::digest(material)))
}

fn read_state(face: &contextful_context::read::face::Face, session: &Session, export: &Export) -> Result<contextful_core::export::ChangeState> {
    let order = export.key.iter().map(|k| ident(k)).collect::<Vec<_>>().join(", ");
    let types: BTreeMap<_, _> = face.store().try_schema(&export.table)?.map(|s| s.columns.into_iter().map(|c| (c.name, c.ty)).collect()).unwrap_or_default();
    let mut rows = Vec::new();
    loop {
        let statement = format!("SELECT * FROM {} ORDER BY {order} LIMIT {EXPORT_BATCH_ROWS} OFFSET {}", ident(&export.table), rows.len());
        let response = face.query(session, &statement, ReadOptions { limit: Some(EXPORT_BATCH_ROWS as u64), ..ReadOptions::default() })?;
        let count = response.rows.len();
        for values in response.rows {
            let row: serde_json::Map<String, serde_json::Value> = response.columns.iter().cloned().zip(values).map(|(name, value)| {
                let typed = typed_cell(value, types.get(&name));
                (name, typed)
            }).collect();
            rows.push(serde_json::Value::Object(row));
        }
        if count < EXPORT_BATCH_ROWS { break; }
    }
    Ok(change_state(export, &rows)?)
}

fn run_changes(l: &crate::project::Located, export: &Export, project: &ProjectArgs, admit: &AdmitArgs) -> Result<()> {
    let vars: BTreeMap<String, String> = std::env::vars().collect();
    let resolver = contextful_outbound::assemble(&vars, Arc::new(SystemClock))?;
    resolver.preflight(export.headers.values())?;
    let allow = Allowlist::parse(&[export.endpoint.host_str().unwrap_or_default()])?;
    if export.headers.values().any(|t| t.has_reference()) { allow.check_bound()?; }
    let client = Client::new(allow, export.endpoint.clone()).with_body_limit(64 * 1024);
    let (authority, _) = admit.admit(project.project.as_deref(), "an export")?;
    let face = face(l)?;
    let dir = cursor_dir(&l.project.dir, &l.project.name);
    std::fs::create_dir_all(&dir)?;
    let _held = FileLock::acquire(&dir.join(format!("{}.lock", export.name)), std::time::Duration::from_secs(LOCK_WAIT_SECS))?;
    let mut ledger = ExportLedger::open(&l.project.store_root().join(MACHINE_CATALOG_FILE))?;
    if ledger.position(&export.name)?.pending_publication.is_none() {
        let before = source_publication(&face, &export.table)?;
        if ledger.position(&export.name)?.source_publication.as_deref() != Some(&before) {
            let session = face.session(&authority, &Request { zone: None }, Bounds::default())?;
            let state = read_state(&face, &session, export)?;
            let after = source_publication(&face, &export.table)?;
            if before != after { anyhow::bail!("export `{}`: source publication moved during the read", export.name); }
            let position = ledger.position(&export.name)?;
            let prior = ledger.state(&export.name)?;
            let events = change_events(export, &before, position.next_sequence, &prior, &state)?;
            ledger.stage(&export.name, &before, &events, &state)?;
        }
    }
    let mut delivered = 0usize;
    loop {
        let available = ledger.pending(&export.name, EXPORT_BATCH_ROWS)?;
        let count = typed_batch_len(&available)?;
        let batch = &available[..count];
        let Some(last) = batch.last() else { break };
        let through = last.sequence;
        let mut headers = vec![("Content-Type".to_string(), HeaderValue::Plain("application/json".to_string()))];
        for (header, template) in &export.headers {
            let value = resolver.render(template)?;
            headers.push((header.clone(), if template.has_reference() { HeaderValue::Sensitive(Arc::new(value)) } else { HeaderValue::Plain(value.reveal().to_string()) }));
        }
        let body = serde_json::to_vec(&serde_json::json!({"version": 1, "events": batch}))?;
        match client.send("POST", &export.endpoint, &headers, Some(&body)) {
            Ok(response) if (200..300).contains(&response.status) => ledger.acknowledge(&export.name, through)?,
            Ok(response) => return Err(ExportError::ExportDeliveryRefused(format!("export `{}`: target answered {}", export.name, response.status)).into()),
            Err(failure) => return Err(ExportError::ExportDeliveryRefused(format!("export `{}`: {}", export.name, failure.message)).into()),
        }
        delivered += batch.len();
    }
    println!("{}: acknowledged {delivered} typed events", export.name);
    Ok(())
}
