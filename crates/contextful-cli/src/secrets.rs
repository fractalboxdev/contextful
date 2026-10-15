//! `contextful secrets list`: the inventory of configured credential bindings, their
//! operator records, value-free provider observations and the latest run attribution
//! (`connector.record`).

use crate::run::ProjectArgs;
use anyhow::Result;
use clap::Subcommand;
use contextful_core::connector::record::{bound_names, inventory_row, read_records, Answered, Expiry, Mark, Observation, OperatorRecord, Row, Unavailable, RECORD_BLOCK};
use contextful_core::connector::ConnectorError;
use contextful_core::connector::reference::SecretName;
use contextful_core::pipeline::declare::ManifestFile;
use contextful_core::ports::Clock;
use contextful_core::run::record::RunRow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Subcommand)]
pub enum SecretsCmd {
    /// List every configured `secret://` binding with its configured adapters, operator
    /// record, provider observations and the adapter that last answered it in a run.
    List {
        #[command(flatten)]
        project: ProjectArgs,
        /// The manifest to read; absent, the project's `contextful.toml`.
        #[arg(long)]
        declaration: Option<PathBuf>,
        /// One JSON object per binding per line.
        #[arg(long)]
        json: bool,
    },
}

/// Every logical name the manifests in `files` bind.
pub(crate) fn configured(files: &[ManifestFile]) -> Result<BTreeSet<String>> {
    let mut bound = BTreeSet::new();
    for f in files {
        let value: toml::Value = if f.path.ends_with(".json") {
            // A JSON manifest's references are its string values; its text holds each one verbatim.
            toml::Value::String(f.text.clone())
        } else {
            toml::from_str(&f.text).map_err(|e| anyhow::anyhow!("{}: {e}", f.path))?
        };
        bound.extend(bound_names(&value));
    }
    Ok(bound)
}

/// Read the operator records of the declaration in `files` against the names the manifests
/// bind, whatever backend the environment selects. A record naming no bound name, or a
/// `[secret.<name>]` table in any other manifest, refuses (`connector.record.record-rejected`).
pub(crate) fn records(declaration: &Path, files: &[ManifestFile]) -> Result<(BTreeSet<String>, BTreeMap<String, OperatorRecord>)> {
    let bound = configured(files)?;
    let path = declaration.display().to_string();
    let mut records = BTreeMap::new();
    for f in files.iter().filter(|f| f.path.ends_with(".toml")) {
        let value: toml::Value = toml::from_str(&f.text).map_err(|e| anyhow::anyhow!("{}: {e}", f.path))?;
        if f.path == path {
            records = read_records(&path, &value, &bound)?;
        } else if value.get(RECORD_BLOCK).is_some() {
            return Err(ConnectorError::SecretRecordRejected(format!("{}: `{RECORD_BLOCK}` records live in the declaration {path}, not a pipeline manifest", f.path)).into());
        }
    }
    Ok((bound, records))
}

/// Validate the declaration's operator records.
pub(crate) fn check_records(declaration: &Path, files: &[ManifestFile]) -> Result<()> {
    records(declaration, files).map(|_| ())
}

/// The adapter that answered each name in the latest recorded run naming it.
fn latest_answered(project: &crate::project::Located, now: &Option<String>) -> Result<BTreeMap<String, Answered>> {
    let mut out: BTreeMap<String, Answered> = BTreeMap::new();
    if !project.project.store_root().exists() {
        return Ok(out);
    }
    let w = crate::run::wire_at(&project.project, now)?;
    let mut runs: Vec<RunRow> = w.engine.catalog.runs(None)?.into_iter().filter(|r| !r.credential_providers.is_empty()).collect();
    runs.sort_by_key(|r| r.ended_at.unwrap_or(r.started_at));
    for r in runs {
        let at = r.ended_at.unwrap_or(r.started_at);
        for (name, adapter) in &r.credential_providers {
            out.insert(name.clone(), Answered { adapter: adapter.clone(), run_id: r.run_id.clone(), at });
        }
    }
    Ok(out)
}

pub fn run(cmd: SecretsCmd) -> Result<()> {
    match cmd {
        SecretsCmd::List { project, declaration, json } => {
            let located = project.locate(declaration)?;
            let files = crate::project::manifests(&located.declaration)?;
            let vars: BTreeMap<String, String> = std::env::vars().collect();
            let clock: Arc<dyn Clock + Send + Sync> = crate::run::clock(&project.now)?;
            let assembly = contextful_outbound::secrets::assemble_all(&vars, clock.clone())?;
            let (mut bound, mut records) = records(&located.declaration, &files)?;
            bound.extend(assembly.bootstrap.as_ref().map(|n| n.as_str().to_string()));
            let answered = latest_answered(&located, &project.now)?;
            let now = clock.now();
            for name in &bound {
                let logical = SecretName::parse(name)?;
                let observations: Vec<Observation> = assembly.observers.iter().filter(|o| o.covers(&logical)).map(|o| o.observe(&logical, now)).collect();
                let row = inventory_row(name, assembly.configured(&logical), records.remove(name), observations, answered.get(name).cloned(), now);
                if json {
                    println!("{}", serde_json::to_string(&row)?);
                } else {
                    print!("{}", text(&row));
                }
            }
            Ok(())
        }
    }
}

fn expiry(e: &Expiry) -> String {
    match e {
        Expiry::Unknown => "unknown".into(),
        Expiry::Never => "never".into(),
        Expiry::At(at) => at.to_rfc3339(),
    }
}

/// The human listing of one row: every operator field labelled `operator`, every
/// observation with its adapter and time, the answering adapter apart from the configured ones.
fn text(row: &Row) -> String {
    let unknown = || "unknown".to_string();
    let mut out = format!("{}\n  configured  {}\n", row.name, if row.configured.is_empty() { "none".into() } else { row.configured.join(", ") });
    match &row.answered {
        Some(a) => out.push_str(&format!("  answered    {} in run {} at {}\n", a.adapter, a.run_id, a.at.to_rfc3339())),
        None => out.push_str("  answered    none recorded\n"),
    }
    let r = row.operator.clone().unwrap_or_default();
    out.push_str(&format!("  operator    description: {}\n", r.description.unwrap_or_else(unknown)));
    out.push_str(&format!("  operator    account: {}\n", r.account.unwrap_or_else(unknown)));
    out.push_str(&format!("  operator    grants: {}\n", r.grants.map_or_else(unknown, |g| g.join(", "))));
    let surfaces: Vec<String> = r.surfaces.iter().map(|s| format!("{} ({})", s.surface, s.binding)).collect();
    out.push_str(&format!("  operator    surfaces: {}\n", if surfaces.is_empty() { unknown() } else { surfaces.join(", ") }));
    out.push_str(&format!("  operator    rotate: {}\n", r.rotate.unwrap_or_else(unknown)));
    out.push_str(&format!("  operator    created: {}\n", r.created.map_or_else(unknown, |c| c.to_rfc3339())));
    for o in &row.observations {
        match o {
            Observation::Observed { adapter, at, present, version, .. } => out.push_str(&format!(
                "  observed    {adapter} at {}: {}{}\n",
                at.to_rfc3339(),
                if *present { "present" } else { "absent" },
                version.as_ref().map(|v| format!(", version {v}")).unwrap_or_default()
            )),
            Observation::Unavailable { adapter, at, reason } => {
                let (kind, why) = match reason {
                    Unavailable::Unsupported(w) => ("unsupported", w),
                    Unavailable::Denied(w) => ("denied", w),
                    Unavailable::Failed(w) => ("failed", w),
                };
                out.push_str(&format!("  observed    {adapter} at {}: unavailable, {kind}: {why}\n", at.to_rfc3339()));
            }
        }
    }
    for e in &row.expiries {
        let days = e.days_remaining.map(|d| format!(", {d} d remaining")).unwrap_or_default();
        let mark = match e.mark {
            Some(Mark::Within) => " [expires within 30 d]",
            Some(Mark::Elapsed) => " [expired]",
            None => "",
        };
        out.push_str(&format!("  expires     {}: {}{days}{mark}\n", e.provenance, expiry(&e.expires)));
    }
    out
}
