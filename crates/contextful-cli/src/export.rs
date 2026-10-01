//! `contextful export` — the manifest's `[[export]]` blocks on the command line.
//!
//! `run` delivers one export once: it admits the export's credential, reads the rows past
//! the export's cursor through the read face in commit order, posts each batch to the OTLP
//! target through the mediated client, and commits the cursor after each acknowledgement.

use crate::admit::{face, AdmitArgs};
use crate::run::{ProjectArgs, SystemClock};
use anyhow::{Context, Result};
use clap::Subcommand;
use contextful_context::read::face::ReadOptions;
use contextful_context::store::{replace_file, FileLock, LOCK_WAIT_SECS};
use contextful_core::connector::attach::Allowlist;
use contextful_core::export::{log_records, parse_exports, ExportCursor, ExportError, EXPORT_BATCH_ROWS};
use contextful_core::store::bound_time::Bounds;
use contextful_outbound::{Client, HeaderValue};
use contextful_policy::enforce::session::Request;
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
        ExportCmd::Run { name, project, declaration, admit } => {
            let l = project.locate(declaration)?;
            let text = std::fs::read_to_string(&l.declaration).with_context(|| format!("reading the declaration `{}`", l.declaration.display()))?;
            let export = parse_exports(&text)
                .with_context(|| l.declaration.display().to_string())?
                .into_iter()
                .find(|e| e.name == name)
                .with_context(|| format!("no export `{name}` is declared"))?;

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
                    headers.push((header.clone(), if t.has_reference() { HeaderValue::Sensitive(v) } else { HeaderValue::Plain(v.reveal().to_string()) }));
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
    }
}
