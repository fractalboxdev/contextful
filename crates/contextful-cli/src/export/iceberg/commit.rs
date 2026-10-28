//! The snapshot one export commits: an `append` creating the table's first snapshot, or an
//! `overwrite` adding the export's files and marking every live file of the parent deleted
//! (`run.export-iceberg.create`, `run.export-iceberg.overwrite`). The library's transaction
//! offers no overwrite and keeps its commit builder private, so the manifests, manifest list
//! and `updateTable` body are assembled here from its public writers
//! (`A-run#iceberg-overwrite-commit`).

use super::remote::Storage;
use anyhow::{Context, Result};
use iceberg::io::FileIO;
use iceberg::spec::{
    DataFile, FormatVersion, ManifestContentType, ManifestListWriter, ManifestWriterBuilder, Operation, Snapshot, SnapshotReference, SnapshotRetention, Summary, TableMetadata, MAIN_BRANCH,
};
use iceberg::table::Table;
use iceberg::{TableIdent, TableRequirement, TableUpdate};
use serde_json::{json, Value};
use std::collections::HashMap;

/// One commit attempt: its snapshot id and the request body.
pub(super) struct Attempt {
    pub snapshot_id: i64,
    pub body: Value,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or_default()
}

/// Copy `location` from storage into the in-memory file system.
async fn fetch(io: &FileIO, storage: &Storage, location: &str) -> Result<()> {
    let bytes = storage.get(location)?;
    io.new_output(location)?.write(bytes.into()).await?;
    Ok(())
}

async fn upload(io: &FileIO, storage: &Storage, location: &str, uploaded: &mut Vec<String>) -> Result<()> {
    let bytes = io.new_input(location)?.read().await?;
    // Recorded before the upload, so a lost answer to a kept object still deletes it.
    uploaded.push(location.to_string());
    storage.put(location, &bytes)
}

/// Build and upload one attempt's manifests and manifest list against `metadata`'s current
/// snapshot, and the `updateTable` body requiring the table uuid and `main` at that snapshot.
/// Every manifest and manifest list the attempt uploads is pushed onto `uploaded`, a failure
/// included.
pub(super) fn attempt(rt: &tokio::runtime::Runtime, metadata: &TableMetadata, namespace: &str, table: &str, storage: &Storage, added: &[DataFile], uploaded: &mut Vec<String>) -> Result<Attempt> {
    rt.block_on(build(metadata, namespace, table, storage, added, uploaded))
}

async fn build(meta: &TableMetadata, namespace: &str, name: &str, storage: &Storage, added: &[DataFile], uploaded: &mut Vec<String>) -> Result<Attempt> {
    let io = FileIO::new_with_memory();
    let ident = TableIdent::from_strs([namespace, name])?;
    let table = Table::builder().metadata(meta.clone()).identifier(ident.clone()).file_io(io.clone()).runtime(iceberg::Runtime::current()).metadata_location(format!("{}/metadata/current.metadata.json", meta.location())).build()?;
    let version = meta.format_version();
    let parent = meta.current_snapshot_id();
    let sequence = meta.next_sequence_number();
    let commit = uuid::Uuid::new_v4();
    let snapshot_id = loop {
        let (a, b) = uuid::Uuid::new_v4().as_u64_pair();
        let id = ((a ^ b) >> 1) as i64;
        if id != 0 && meta.snapshots().all(|s| s.snapshot_id() != id) {
            break id;
        }
    };
    let schema = meta.current_schema().clone();
    // A manifest holds entries of one partition spec; a parent written under an earlier spec
    // keeps that spec in the manifest marking its files deleted.
    let manifest = |n: usize, spec_id: i32| -> Result<_> {
        let spec = meta.partition_spec_by_id(spec_id).with_context(|| format!("the table holds no partition spec {spec_id}"))?.as_ref().clone();
        let path = format!("{}/metadata/{commit}-m{n}.avro", meta.location());
        let builder = ManifestWriterBuilder::new(io.new_output(&path)?, Some(snapshot_id), schema.clone(), spec);
        Ok((path, match version {
            FormatVersion::V3 => builder.build_v3_data(),
            _ => builder.build_v2_data(),
        }))
    };

    let (added_files, added_rows) = (added.len(), added.iter().map(DataFile::record_count).sum::<u64>());
    let mut manifests = Vec::new();
    if !added.is_empty() {
        let (path, mut writer) = manifest(0, meta.default_partition_spec_id())?;
        for file in added {
            // A negative sequence number leaves the entry to inherit the snapshot's at commit.
            writer.add_file(file.clone(), -1)?;
        }
        manifests.push(writer.write_manifest_file().await?);
        upload(&io, storage, &path, uploaded).await?;
    }

    // Every live data file of the parent snapshot becomes a `DELETED` entry, one manifest per
    // partition spec its manifests carry.
    let (mut deleted_files, mut deleted_rows) = (0usize, 0u64);
    if let Some(snapshot) = meta.current_snapshot() {
        fetch(&io, storage, snapshot.manifest_list()).await?;
        let list = table.manifest_list_reader(snapshot).load().await?;
        let mut writers = std::collections::BTreeMap::new();
        for file in list.entries() {
            // Delete files another engine wrote apply to the data this export replaces; the new
            // snapshot's manifest list leaves them out, so they stop applying with it.
            if file.content != ManifestContentType::Data {
                continue;
            }
            fetch(&io, storage, &file.manifest_path).await?;
            let next = writers.len() + 1;
            if !writers.contains_key(&file.partition_spec_id) {
                writers.insert(file.partition_spec_id, (manifest(next, file.partition_spec_id)?, 0usize));
            }
            let Some(((_, writer), count)) = writers.get_mut(&file.partition_spec_id) else { continue };
            for entry in file.load_manifest(&io).await?.entries() {
                if entry.is_alive() {
                    deleted_files += 1;
                    *count += 1;
                    deleted_rows += entry.data_file().record_count();
                    writer.add_delete_file(entry.data_file().clone(), entry.sequence_number().context("a live manifest entry carries no sequence number")?, entry.file_sequence_number)?;
                }
            }
        }
        for ((path, writer), count) in writers.into_values() {
            if count > 0 {
                manifests.push(writer.write_manifest_file().await?);
                upload(&io, storage, &path, uploaded).await?;
            }
        }
    }

    let list_path = format!("{}/metadata/snap-{snapshot_id}-1-{commit}.avro", meta.location());
    let out = io.new_output(&list_path)?.writer().await?;
    let first_row_id = meta.next_row_id();
    let mut list = match version {
        FormatVersion::V3 => ManifestListWriter::v3(out, snapshot_id, parent, sequence, Some(first_row_id)),
        _ => ManifestListWriter::v2(out, snapshot_id, parent, sequence),
    };
    list.add_manifests(manifests.into_iter())?;
    let next_row_id = list.next_row_id();
    list.close().await?;
    upload(&io, storage, &list_path, uploaded).await?;

    let operation = if parent.is_some() { Operation::Overwrite } else { Operation::Append };
    let summary = Summary {
        operation,
        additional_properties: HashMap::from([
            ("added-data-files".into(), added_files.to_string()),
            ("added-records".into(), added_rows.to_string()),
            ("deleted-data-files".into(), deleted_files.to_string()),
            ("deleted-records".into(), deleted_rows.to_string()),
            ("total-data-files".into(), added_files.to_string()),
            ("total-records".into(), added_rows.to_string()),
            ("total-delete-files".into(), "0".into()),
        ]),
    };
    let builder = Snapshot::builder()
        .with_manifest_list(list_path)
        .with_snapshot_id(snapshot_id)
        .with_parent_snapshot_id(parent)
        .with_sequence_number(sequence)
        .with_summary(summary)
        .with_schema_id(meta.current_schema_id())
        .with_timestamp_ms(now_ms().max(meta.last_updated_ms() + 1));
    let snapshot = match next_row_id {
        Some(next) => builder.with_row_range(first_row_id, next - first_row_id).build(),
        None => builder.build(),
    };
    let updates = vec![
        TableUpdate::AddSnapshot { snapshot },
        TableUpdate::SetSnapshotRef { ref_name: MAIN_BRANCH.to_string(), reference: SnapshotReference::new(snapshot_id, SnapshotRetention::branch(None, None, None)) },
    ];
    // An absent parent requires that `main` does not exist (`run.export-iceberg.create`).
    // The files were written against this table's current schema and spec, so a commit
    // requires both beside `main` at the parent.
    let requirements = vec![
        TableRequirement::UuidMatch { uuid: meta.uuid() },
        TableRequirement::RefSnapshotIdMatch { r#ref: MAIN_BRANCH.to_string(), snapshot_id: parent },
        TableRequirement::CurrentSchemaIdMatch { current_schema_id: meta.current_schema_id() },
        TableRequirement::DefaultSpecIdMatch { default_spec_id: meta.default_partition_spec_id() },
    ];
    let body = json!({
        "identifier": { "namespace": [namespace], "name": name },
        "requirements": requirements,
        "updates": updates,
    });
    Ok(Attempt { snapshot_id, body })
}
