//! An overwrite commit iceberg-rust 0.10.1's `Transaction` does not offer: one snapshot that
//! adds the export's files and marks every live file of the parent snapshot deleted, sent
//! as a hand-written REST `updateTable` request. iceberg-rust supplies the manifest,
//! manifest-list and snapshot types; `TableCommit`'s builder and the transaction-action
//! trait are crate-private, so the request body is serialized here.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use iceberg::spec::{
    DataFile, FormatVersion, ManifestContentType, ManifestListWriter, ManifestWriterBuilder,
    Operation, Snapshot, SnapshotReference, SnapshotRetention, Summary, MAIN_BRANCH,
};
use iceberg::table::Table;
use iceberg::{TableRequirement, TableUpdate};

use crate::Env;

pub async fn overwrite(env: &Env, table: &Table, added: Vec<DataFile>) -> Result<i64> {
    let meta = table.metadata();
    let io = table.file_io();
    let version = meta.format_version();
    let parent = meta.current_snapshot_id();
    let seq = meta.next_sequence_number();
    let commit = uuid::Uuid::new_v4();
    let snapshot_id = loop {
        let (a, b) = uuid::Uuid::new_v4().as_u64_pair();
        let id = ((a ^ b) >> 1) as i64;
        if meta.snapshots().all(|s| s.snapshot_id() != id) {
            break id;
        }
    };
    let schema = meta.current_schema().clone();
    let spec = meta.default_partition_spec().as_ref().clone();
    let new_writer = |n: usize| -> Result<_> {
        let path = format!("{}/metadata/{commit}-m{n}.avro", meta.location());
        let b = ManifestWriterBuilder::new(io.new_output(path)?, Some(snapshot_id), schema.clone(), spec.clone());
        Ok(match version {
            FormatVersion::V3 => b.build_v3_data(),
            _ => b.build_v2_data(),
        })
    };

    let (added_files, added_rows) = (added.len(), added.iter().map(|f| f.record_count()).sum::<u64>());
    let mut manifests = Vec::new();
    let mut w = new_writer(0)?;
    for f in added {
        w.add_file(f, -1)?; // data sequence number inherited at commit
    }
    manifests.push(w.write_manifest_file().await?);

    // Every live data file of the parent snapshot becomes a DELETED entry.
    let (mut deleted_files, mut deleted_rows) = (0usize, 0u64);
    if let Some(snap) = meta.current_snapshot() {
        let list = table.manifest_list_reader(snap).load().await?;
        let mut w = new_writer(1)?;
        for mf in list.entries() {
            if mf.content != ManifestContentType::Data {
                bail!("parent snapshot carries delete files; the spike handles data files only");
            }
            for e in mf.load_manifest(io).await?.entries() {
                if e.is_alive() {
                    deleted_files += 1;
                    deleted_rows += e.data_file().record_count();
                    w.add_delete_file(
                        e.data_file().clone(),
                        e.sequence_number().context("live entry without sequence number")?,
                        e.file_sequence_number,
                    )?;
                }
            }
        }
        if deleted_files > 0 {
            manifests.push(w.write_manifest_file().await?);
        }
    }

    let list_path = format!("{}/metadata/snap-{snapshot_id}-0-{commit}.avro", meta.location());
    let out = io.new_output(list_path.clone())?.writer().await?;
    let first_row_id = meta.next_row_id();
    let mut lw = match version {
        FormatVersion::V3 => ManifestListWriter::v3(out, snapshot_id, parent, seq, Some(first_row_id)),
        _ => ManifestListWriter::v2(out, snapshot_id, parent, seq),
    };
    lw.add_manifests(manifests.into_iter())?;
    let next_row_id = lw.next_row_id();
    lw.close().await?;

    let summary = Summary {
        operation: Operation::Overwrite,
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
        .with_sequence_number(seq)
        .with_summary(summary)
        .with_schema_id(meta.current_schema_id())
        .with_timestamp_ms(now_ms());
    let snapshot = match next_row_id {
        Some(n) => builder.with_row_range(first_row_id, n - first_row_id).build(),
        None => builder.build(),
    };

    let updates = vec![
        TableUpdate::AddSnapshot { snapshot },
        TableUpdate::SetSnapshotRef {
            ref_name: MAIN_BRANCH.to_string(),
            reference: SnapshotReference::new(snapshot_id, SnapshotRetention::branch(None, None, None)),
        },
    ];
    let requirements = vec![
        TableRequirement::UuidMatch { uuid: meta.uuid() },
        TableRequirement::RefSnapshotIdMatch { r#ref: MAIN_BRANCH.to_string(), snapshot_id: parent },
    ];
    post_commit(env, &requirements, &updates).await?;
    Ok(snapshot_id)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

/// `POST /v1/{prefix}/namespaces/{ns}/tables/{table}`, the REST `updateTable` operation.
async fn post_commit(env: &Env, requirements: &[TableRequirement], updates: &[TableUpdate]) -> Result<()> {
    let http = reqwest::Client::new();
    let auth = |r: reqwest::RequestBuilder| match &env.token {
        Some(t) => r.bearer_auth(t),
        None => r,
    };
    let mut cfg = http.get(format!("{}/v1/config", env.uri));
    if let Some(w) = &env.warehouse {
        cfg = cfg.query(&[("warehouse", w)]);
    }
    let cfg: serde_json::Value = auth(cfg).send().await?.error_for_status()?.json().await?;
    let prefix = cfg["overrides"]["prefix"]
        .as_str()
        .or(cfg["defaults"]["prefix"].as_str())
        .map(|p| format!("{p}/"))
        .unwrap_or_default();
    let url = format!("{}/v1/{prefix}namespaces/{}/tables/{}", env.uri, env.ns, env.table);
    let body = serde_json::json!({
        "identifier": { "namespace": [env.ns], "name": env.table },
        "requirements": requirements,
        "updates": updates,
    });
    let res = auth(http.post(url).json(&body)).send().await?;
    let status = res.status();
    if !status.is_success() {
        let text = res.text().await.unwrap_or_default();
        bail!("commit refused: HTTP {status}: {}", text.chars().take(400).collect::<String>());
    }
    Ok(())
}
