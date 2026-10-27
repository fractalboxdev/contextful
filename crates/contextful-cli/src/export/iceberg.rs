//! `run.export-iceberg`: one audience's admitted rows of one landed table, published as an
//! Apache Iceberg table in a REST catalog. The export reads through the read face, writes
//! fresh Parquet and manifests, and commits one snapshot; it never names a store file.

use super::{cursor_dir, read_identity, source_frontier, ProjectArgs};
use crate::admit::{face, AdmitArgs};
use crate::clock::SystemClock;
use crate::project::Located;
use anyhow::{bail, Context, Result};
use contextful_context::read::face::ReadOptions;
use contextful_context::store::{FileLock, LOCK_WAIT_SECS};
use contextful_core::export::iceberg::{plan, IcebergRecord, FIXED_SIZE_LIST_PROPERTY, ICEBERG_COMMIT_ATTEMPTS};
use contextful_core::export::{Export, ExportCursor, ExportError, EXPORT_BATCH_ROWS};
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::reconcile::ColumnType;
use contextful_core::store::relation::ident;
use contextful_core::store::reserve::{COMMIT_SEQ, ROW_SEQ};
use contextful_policy::enforce::session::Request;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

mod commit;
mod encode;
mod remote;

use remote::{Catalog, Commit, Storage};

/// A commit sent whose outcome is unknown, and every file its export wrote
/// (`run.export-iceberg.commit-unknown`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pending {
    #[serde(flatten)]
    record: IcebergRecord,
    #[serde(default)]
    files: Vec<String>,
}

/// The export's record: the snapshot it last committed, and the one it sent last whose
/// outcome is unknown (`run.export-iceberg.frontier`, `run.export-iceberg.commit-unknown`).
#[derive(Debug, Default, Serialize, Deserialize)]
struct Ledger {
    committed: Option<IcebergRecord>,
    pending: Option<Pending>,
}

fn read_ledger(store: &contextful_context::Store, path: &Path) -> Result<Ledger> {
    match store.metadata_files().read_optional(path)? {
        Some(bytes) => serde_json::from_slice(&bytes).with_context(|| format!("reading the export record `{}`", path.display())),
        None => Ok(Ledger::default()),
    }
}

fn write_ledger(store: &contextful_context::Store, path: &Path, ledger: &Ledger) -> Result<()> {
    store.metadata_files().replace(path, &serde_json::to_vec(ledger)?)?;
    Ok(())
}

/// What one export commits under: its source frontier, read identity and declared target.
struct Under<'a> {
    name: &'a str,
    frontier: String,
    identity: String,
    declared: String,
    namespace: &'a str,
    table: &'a str,
}

pub(super) fn run(l: &Located, export: &Export, project: &ProjectArgs, admit: &AdmitArgs) -> Result<()> {
    let name = export.name.as_str();
    let target = export.iceberg.as_ref().context("an Iceberg export carries its catalog target")?;
    if crate::storage::preflight(&l.project)?.is_some() {
        bail!("ProfileCapabilityAbsent: export `{name}`: an Iceberg export records its snapshots in the local storage profile alone");
    }

    // One admitted credential, the one the block names (`run.export-iceberg.audience`).
    let (authority, _) = admit.admit(project.project.as_deref(), "an export")?;
    let subject = authority.subject().to_subject();
    if subject.on_behalf_of.as_deref() != Some(target.credential.as_str()) {
        bail!(
            "export `{name}` publishes for `{}`; the admitted credential acts for `{}`, and a namespace serves one credential",
            target.credential,
            subject.on_behalf_of.as_deref().unwrap_or("no principal")
        );
    }

    // The token resolves before any row is read (`run.export-iceberg.token-preflight`).
    let vars: BTreeMap<String, String> = std::env::vars().collect();
    let resolver = contextful_outbound::assemble(&vars, Arc::new(SystemClock))?;
    resolver.preflight(std::iter::once(&target.token))?;

    // Committed runs only, under the admitted credential (`run.export-iceberg.read`).
    let face = face(l)?;
    let session = face.session(&authority, &Request { zone: None }, Bounds::default())?;
    let probe = face.query(&session, &format!("SELECT * FROM {} LIMIT 0", ident(&export.table)), ReadOptions::default())?;
    let Some(stored) = face.store().try_schema(&export.table)? else {
        bail!("export `{name}`: table `{}` holds no landed rows, so no schema to publish", export.table);
    };
    let types: BTreeMap<String, ColumnType> = stored.columns.into_iter().map(|c| (c.name, c.ty)).collect();
    // A masked column takes the type the session projection reads it as (`run.export-iceberg.masked`).
    let masks = session.policy(&export.table);
    let columns: Vec<(String, ColumnType)> = probe
        .columns
        .iter()
        .map(|c| {
            let stored = types.get(c).cloned().unwrap_or(ColumnType::Null);
            let read = masks.and_then(|p| p.columns.get(c)).map_or_else(|| stored.clone(), |p| p.projected_type(&stored));
            (c.clone(), read)
        })
        .collect();
    // Every column takes its type before any row is read (`run.export-iceberg.type-refused`).
    let planned = plan(name, &columns, target.format_version)?;
    for system in [COMMIT_SEQ, ROW_SEQ] {
        if !probe.columns.iter().any(|c| c == system) {
            bail!("export `{name}` reads `{}` without `{system}`, which orders its pages", export.table);
        }
    }

    let dir = cursor_dir(&l.project.dir, &l.project.name);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating `{}`", dir.display()))?;
    let _held = FileLock::acquire(&dir.join(format!("{name}.lock")), std::time::Duration::from_secs(LOCK_WAIT_SECS))?;
    let path = dir.join(format!("{name}.iceberg.json"));
    let mut ledger = read_ledger(face.store(), &path)?;

    let under = Under {
        name,
        frontier: source_frontier(face.store(), &export.table)?,
        identity: read_identity(&face, &session, &authority, export)?,
        declared: target.key(),
        namespace: &target.namespace,
        table: &target.table_name,
    };
    let catalog = Catalog::open(target, &resolver)?;
    let loaded = catalog.load()?;
    // Every object request reaches the declared storage host alone (`run.export-iceberg.egress`).
    let existing = match loaded {
        Some(table) => {
            let storage = Storage::open(name, target, &table.config)?;
            Some((table.metadata, storage))
        }
        None => None,
    };
    let current = existing.as_ref().and_then(|(m, _)| m.current_snapshot_id());

    // A commit whose outcome was unknown counts as taken when the table holds its snapshot;
    // one the table never took leaves files only its writer removes
    // (`run.export-iceberg.commit-unknown`, `run.export-iceberg.orphans`).
    if let Some(pending) = ledger.pending.take() {
        match &existing {
            Some((metadata, _)) if metadata.snapshot_by_id(pending.record.snapshot_id).is_some() => ledger.committed = Some(pending.record),
            Some((_, storage)) => storage.delete_all(&pending.files)?,
            None => {}
        }
        write_ledger(face.store(), &path, &ledger)?;
    }
    if ledger.committed.as_ref().is_some_and(|r| r.holds(current, &under.frontier, &under.identity, &under.declared)) {
        println!("{name}: unchanged at snapshot {}", current.unwrap_or_default());
        return Ok(());
    }

    let schema = encode::schema(&planned)?;
    let properties: HashMap<String, String> = planned.properties.iter().map(|(k, v)| (k.clone(), v.clone())).chain([("format-version".to_string(), target.format_version.to_string())]).collect();
    let (metadata, storage) = match existing {
        Some(existing) => existing,
        None => {
            let table = catalog.create(&schema, &properties)?;
            match Storage::open(name, target, &table.config) {
                Ok(storage) => (table.metadata, storage),
                Err(e) => {
                    // The table this export created for an undeclared host goes with the refusal.
                    let _ = catalog.drop_table();
                    return Err(e);
                }
            }
        }
    };
    let widths = |p: &HashMap<String, String>| -> BTreeMap<String, String> { p.iter().filter(|(k, _)| k.starts_with(FIXED_SIZE_LIST_PROPERTY)).map(|(k, v)| (k.clone(), v.clone())).collect() };
    if encode::shape(metadata.current_schema()) != encode::shape(&schema) || widths(metadata.properties()) != widths(&properties) {
        return Err(ExportError::ExportSchemaChanged(format!(
            "export `{name}`: table `{}.{}` no longer matches the columns of `{}`; publish the changed schema under a new `table_name`",
            target.namespace, target.table_name, export.table
        ))
        .into());
    }

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let (mut written, mut keep) = (Vec::new(), false);
    let outcome = publish_and_commit(&rt, &face, &session, export, &catalog, &storage, metadata, &under, (&path, &mut ledger), &mut written, &mut keep);
    // A failure before the catalog may hold the snapshot deletes every file the export wrote.
    if outcome.is_err() && !keep {
        let _ = storage.delete_all(&written);
    }
    let (rows, files, snapshot) = outcome?;
    println!("{name}: committed {rows} rows in {files} files · snapshot {snapshot}");
    Ok(())
}

/// Write the export's data files, then commit them, retrying a conflict. Every file written
/// is pushed onto `written`; `keep` is set once the catalog may hold the snapshot.
#[allow(clippy::too_many_arguments)]
fn publish_and_commit(
    rt: &tokio::runtime::Runtime,
    face: &contextful_context::read::face::Face,
    session: &contextful_policy::enforce::session::Session,
    export: &Export,
    catalog: &Catalog,
    storage: &Storage,
    mut metadata: iceberg::spec::TableMetadata,
    under: &Under<'_>,
    (path, ledger): (&Path, &mut Ledger),
    written: &mut Vec<String>,
    keep: &mut bool,
) -> Result<(u64, usize, i64)> {
    let name = under.name;
    let added = publish(rt, face, session, export, &metadata, storage, &under.frontier, written)?;
    let rows: u64 = added.iter().map(|f| f.record_count()).sum();
    for attempt in 1..=ICEBERG_COMMIT_ATTEMPTS {
        let mut manifests = Vec::new();
        let built = commit::attempt(rt, &metadata, under.namespace, under.table, storage, &added, &mut manifests);
        let built = match built {
            Ok(built) => built,
            Err(e) => {
                written.extend(manifests);
                return Err(e);
            }
        };
        let record = IcebergRecord { snapshot_id: built.snapshot_id, frontier: under.frontier.clone(), identity: under.identity.clone(), target: under.declared.clone() };
        ledger.pending = Some(Pending { record: record.clone(), files: written.iter().chain(&manifests).cloned().collect() });
        if let Err(e) = write_ledger(face.store(), path, ledger) {
            written.extend(manifests);
            return Err(e);
        }
        match catalog.commit(&built.body)? {
            Commit::Taken => {
                *keep = true;
                ledger.pending = None;
                ledger.committed = Some(record);
                write_ledger(face.store(), path, ledger)?;
                return Ok((rows, added.len(), built.snapshot_id));
            }
            Commit::Unknown(why) => {
                // The catalog may have applied it: every file stays (`run.export-iceberg.commit-unknown`).
                *keep = true;
                bail!("export `{name}`: the commit's outcome is unknown ({why}); every file stays, and the next run reloads the table to learn whether snapshot {} was taken", built.snapshot_id);
            }
            Commit::Refused(why) => {
                written.extend(manifests);
                ledger.pending = None;
                write_ledger(face.store(), path, ledger)?;
                bail!("export `{name}`: the catalog refused the commit, which took no effect: {why}");
            }
            Commit::Conflict(why) => {
                // A retry replaces the manifests and keeps the data files (`run.export-iceberg.orphans`).
                let _ = storage.delete_all(&manifests);
                ledger.pending = None;
                write_ledger(face.store(), path, ledger)?;
                let reloaded = catalog.load()?.map(|t| t.metadata);
                let Some(next) = reloaded.filter(|m| m.current_schema_id() == metadata.current_schema_id() && m.default_partition_spec_id() == metadata.default_partition_spec_id()) else {
                    return Err(ExportError::ExportCommitConflict(format!("export `{name}`: the table's schema or partition spec changed under it; every file this export wrote is deleted")).into());
                };
                if attempt == ICEBERG_COMMIT_ATTEMPTS {
                    return Err(ExportError::ExportCommitConflict(format!("export `{name}`: the last of {ICEBERG_COMMIT_ATTEMPTS} attempts conflicted: {why}; every file this export wrote is deleted")).into());
                }
                metadata = next;
            }
        }
    }
    Err(ExportError::ExportCommitConflict(format!("export `{name}`: no commit attempt remained")).into())
}

/// Read every admitted row in commit order, page by page, into data files; refuse a read the
/// source committed under (`run.export-iceberg.read`).
#[allow(clippy::too_many_arguments)]
fn publish(
    rt: &tokio::runtime::Runtime,
    face: &contextful_context::read::face::Face,
    session: &contextful_policy::enforce::session::Session,
    export: &Export,
    metadata: &iceberg::spec::TableMetadata,
    storage: &Storage,
    frontier: &str,
    written: &mut Vec<String>,
) -> Result<Vec<iceberg::spec::DataFile>> {
    let missing = face.query(session, &export.missing_statement(), ReadOptions::default())?;
    let count = missing.rows.first().and_then(|r| r.first()).and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0);
    if let Some(refusal) = export.missing(count) {
        return Err(refusal.into());
    }
    let schema = metadata.current_schema().clone();
    let arrow = encode::arrow_schema(&schema)?;
    let mut files = encode::Files::new(rt, schema, metadata.location(), metadata.default_partition_spec_id(), storage);
    let mut cursor = ExportCursor::default();
    loop {
        let opts = ReadOptions { limit: Some(EXPORT_BATCH_ROWS as u64), ..ReadOptions::default() };
        let page = face.query(session, &export.batch_statement(&cursor), opts)?;
        if page.truncated {
            bail!("export `{}`: its admitted table read is truncated; nothing is committed", export.name);
        }
        if page.rows.is_empty() {
            break;
        }
        let at = |column: &str| page.columns.iter().position(|c| c == column);
        let (Some(c), Some(r)) = (at(COMMIT_SEQ), at(ROW_SEQ)) else { bail!("export `{}`: a page carries no commit order", export.name) };
        let Some(last) = page.rows.last() else { break };
        let position = |i: usize| last.get(i).and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())));
        cursor = ExportCursor { commit_seq: position(c).context("a row carries no commit sequence")?, row_seq: position(r).context("a row carries no row sequence")? };
        files.write(&encode::batch(&arrow, &page.columns, &page.rows)?, written)?;
        if page.rows.len() < EXPORT_BATCH_ROWS {
            break;
        }
    }
    let added = files.finish(written)?;
    if source_frontier(face.store(), &export.table)? != frontier {
        bail!("export `{}`: the source committed during the read; nothing is committed", export.name);
    }
    Ok(added)
}
