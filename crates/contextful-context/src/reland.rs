//! Relanding: a run the pointer's snapshot folds, committed again from that snapshot's
//! parts under a run id derived from the snapshot and the run (`store.pull.reland`).

use crate::error::{ContextError, IoPath, Result};
use crate::land::{commit_relanded, Batch, RunContext};
use crate::rows::batch_rows;
use crate::scan::scan_at;
use crate::store::Store;
use contextful_core::run::journal::sha256_hex;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::{NodeId, SnapshotId, MANIFEST_FILE, RUNS_DIR};
use contextful_core::store::reserve::{Injection, AUTHORED_AGENT, AUTHORED_BY, BATCH_SEQ, COMMIT_SEQ, INGESTED_AT, OPTIONAL, RUN_ID, SITE_ID, TAINT};
use contextful_core::store::StoreError;
use contextful_core::time::Instant;
use serde_json::{Map, Value};

/// One run committed again from a snapshot's parts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Relanded {
    /// The run key, `<run-id>/<node-id>`, the snapshot folds.
    pub from: String,
    /// The run id the rows commit under.
    pub run_id: String,
    /// The rows committed.
    pub rows: u64,
    /// The rows the folded-runs index records the run held, where it records them; a keyed
    /// snapshot keeps fewer when a later run superseded a key.
    pub held: Option<u64>,
    /// The landing replayed a run an earlier reland committed (`store.lay-out.run-replay`).
    pub replay: bool,
}

/// The run id a reland of run `from` out of `snapshot` commits under: `reland-` and 16 hex
/// digits of SHA-256 over both, so a repeat replays (`store.pull.reland-lineage`).
pub fn relanded_run_id(snapshot: &SnapshotId, from: &str) -> String {
    format!("reland-{}", &sha256_hex(format!("{snapshot}\n{from}").as_bytes())[..16])
}

/// Commit run `from`'s rows out of the parts of `decl`'s pointer snapshot as one run on `node`
/// (`store.pull.reland-rows`), its manifest naming `from` (`store.pull.reland-lineage`), and
/// sync the run's files to disk before returning (`store.pull.reland-order`). A run whose rows
/// the snapshot holds none of commits nothing.
pub fn reland(store: &Store, decl: &TableDecl, from: &str, node: &NodeId) -> Result<Relanded> {
    let table = decl.name.as_str();
    let snapshot = store
        .current_snapshot(table)?
        .ok_or_else(|| ContextError::Invalid(format!("table `{table}` has no pointer, so no snapshot holds run `{from}`")))?;
    let run_id = from.split_once('/').map_or(from, |(run, _)| run);
    let folded = store.folded_runs(table, |k| k == from)?.into_iter().find_map(|(_, row)| row);
    let (seq, instant) = (folded.as_ref().and_then(|r| r.commit_seq), folded.as_ref().and_then(|r| r.committed_at));
    // A run id repeats across nodes; only the index's position tells their rows apart.
    if seq.is_none() && instant.is_none() {
        let siblings = store.folded_runs(table, |k| k != from && k.split_once('/').is_some_and(|(run, _)| run == run_id))?;
        if let Some((other, _)) = siblings.first() {
            return Err(StoreError::SyncLocalFoldUnrecoverable(format!(
                "snapshot `{}` folds `{from}` and `{other}` under one run id, and no folded-runs row records `{from}`'s commit, so its rows are not told apart",
                snapshot.snapshot_id
            ))
            .into());
        }
    }
    let matches = |row: &Map<String, Value>| -> bool {
        row.get(RUN_ID).and_then(Value::as_str) == Some(run_id)
            && seq.is_none_or(|s| row.get(COMMIT_SEQ).and_then(Value::as_i64) == Some(s))
            && instant.is_none_or(|t| row.get(INGESTED_AT).and_then(Value::as_str).and_then(|v| Instant::parse(v).ok()) == Some(t))
    };
    let scan = scan_at(store, decl, Bounds::default(), Some(&snapshot))?;
    let mut rows: Vec<Map<String, Value>> = Vec::new();
    for file in &scan.files {
        store.each_parquet_batch(&store.logical_path(file)?, &mut |batch| {
            let names: Vec<String> = batch.schema().fields().iter().map(|f| f.name().clone()).collect();
            let columns: Vec<&str> = names.iter().map(String::as_str).collect();
            rows.extend(batch_rows(&batch, &columns)?.into_iter().filter(|r| matches(r)));
            Ok(())
        })?;
    }
    drop(scan);
    let id = relanded_run_id(&snapshot.snapshot_id, from);
    let held = folded.as_ref().and_then(|r| r.rows);
    let Some(first) = rows.first().cloned() else {
        return Ok(Relanded { from: from.to_string(), run_id: id, rows: 0, held, replay: false });
    };
    let text = |c: &str| first.get(c).and_then(Value::as_str).map(str::to_string);
    let committed_at = match instant {
        Some(t) => t,
        None => text(INGESTED_AT).and_then(|v| Instant::parse(&v).ok()).ok_or_else(|| ContextError::Invalid(format!("run `{from}`: its rows carry no `{INGESTED_AT}`")))?,
    };
    let injection = Injection {
        run_id: id.clone(),
        site_id: text(SITE_ID).unwrap_or_default(),
        batch_seq: first.get(BATCH_SEQ).and_then(Value::as_i64).and_then(|v| i32::try_from(v).ok()),
        authored_by: text(AUTHORED_BY),
        authored_agent: text(AUTHORED_AGENT),
        taint: text(TAINT).and_then(|v| v.parse().ok()),
    };
    // The engine's columns are injected again; a producer's own `_` columns stay.
    let producer = |c: &str| !c.starts_with('_') || OPTIONAL.contains(&c);
    let count = rows.len() as u64;
    let rows: Vec<Map<String, Value>> = rows.into_iter().map(|r| r.into_iter().filter(|(c, _)| producer(c)).collect()).collect();
    let types = store
        .try_schema(table)?
        .map(|s| s.columns.into_iter().filter(|c| producer(&c.name)).map(|c| (c.name, c.ty)).collect())
        .unwrap_or_default();
    let ctx = RunContext { node: node.clone(), injection, committed_at };
    let landing = commit_relanded(store, decl, &Batch { rows, types }, &ctx, from)?;
    let manifest = landing.committed()?;
    let node_dir = store.table_dir(table)?.join(RUNS_DIR).join(&id).join(node.as_str());
    for name in manifest.parts.iter().map(|p| p.name.as_str()).chain(std::iter::once(MANIFEST_FILE)) {
        let path = node_dir.join(name);
        std::fs::File::open(&path).at(&path)?.sync_all().at(&path)?;
    }
    contextful_fs::open_dir_for_sync(&node_dir).at(&node_dir)?.sync_all().at(&node_dir)?;
    Ok(Relanded { from: from.to_string(), run_id: id, rows: count, held, replay: landing.replay })
}
