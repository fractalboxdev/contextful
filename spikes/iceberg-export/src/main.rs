//! Throwaway exporter for #734: writes one Arrow batch carrying each problem type as an
//! Iceberg table and commits one snapshot through an Iceberg REST catalog.
//!
//! Subcommands:
//!   types                      print iceberg-rust's verdict and the spike's cast policy per type
//!   export <rows> [v2|v3]      write <rows> rows and commit one snapshot (overwrite)
//!   stale <rows> [v2|v3]       commit against metadata loaded before a concurrent commit
//!
//! Environment: CATALOG_URI, CATALOG_WAREHOUSE, CATALOG_TOKEN (optional), ICEBERG_NS,
//! ICEBERG_TABLE.

mod commit;
mod policy;
mod source;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use iceberg::spec::{DataFileFormat, FormatVersion};
use iceberg::writer::base_writer::data_file_writer::DataFileWriterBuilder;
use iceberg::writer::file_writer::location_generator::{DefaultFileNameGenerator, DefaultLocationGenerator};
use iceberg::writer::file_writer::rolling_writer::RollingFileWriterBuilder;
use iceberg::writer::file_writer::ParquetWriterBuilder;
use iceberg::writer::{IcebergWriter, IcebergWriterBuilder};
use iceberg::{Catalog, CatalogBuilder, NamespaceIdent, TableCreation, TableIdent};
use iceberg_catalog_rest::RestCatalogBuilder;
use iceberg_storage_opendal::OpenDalResolvingStorageFactory;
use parquet::file::properties::WriterProperties;

pub struct Env {
    pub uri: String,
    pub warehouse: Option<String>,
    pub token: Option<String>,
    pub ns: String,
    pub table: String,
}

impl Env {
    fn load() -> Self {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Self {
            uri: get("CATALOG_URI").unwrap_or_else(|| "http://localhost:18181".into()),
            warehouse: get("CATALOG_WAREHOUSE"),
            token: get("CATALOG_TOKEN"),
            ns: get("ICEBERG_NS").unwrap_or_else(|| "audience_a".into()),
            table: get("ICEBERG_TABLE").unwrap_or_else(|| "events".into()),
        }
    }
}

async fn open_catalog(env: &Env) -> Result<impl Catalog> {
    let mut props = HashMap::from([("uri".to_string(), env.uri.clone())]);
    if let Some(w) = &env.warehouse {
        props.insert("warehouse".into(), w.clone());
    }
    if let Some(t) = &env.token {
        props.insert("token".into(), t.clone());
    }
    Ok(RestCatalogBuilder::default()
        .with_storage_factory(Arc::new(OpenDalResolvingStorageFactory::new()))
        .load("export", props)
        .await?)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("types") => policy::report(),
        Some(cmd @ ("export" | "stale")) => {
            let rows: usize = args.get(1).context("rows")?.parse()?;
            let version = match args.get(2).map(String::as_str) {
                None | Some("v2") => FormatVersion::V2,
                Some("v3") => FormatVersion::V3,
                Some(other) => bail!("unknown format {other}"),
            };
            export(&Env::load(), rows, version, cmd == "stale").await
        }
        _ => bail!("usage: iceberg-export-spike types | export <rows> [v2|v3] | stale <rows> [v2|v3]"),
    }
}

async fn export(env: &Env, rows: usize, version: FormatVersion, stale: bool) -> Result<()> {
    let t0 = Instant::now();
    let catalog = open_catalog(env).await?;
    let ns = NamespaceIdent::new(env.ns.clone());
    if !catalog.namespace_exists(&ns).await? {
        catalog.create_namespace(&ns, HashMap::new()).await?;
    }

    // The store's batch, as the read face would hand it over, then cast by the policy.
    let source = source::batch(rows)?;
    let plan = policy::plan(source.schema().as_ref(), version)?;
    let ident = TableIdent::new(ns.clone(), env.table.clone());
    let table = if catalog.table_exists(&ident).await? {
        catalog.load_table(&ident).await?
    } else {
        // iceberg-catalog-rest 0.10.1 drops `TableCreation::format_version` from the
        // createTable request; the `format-version` property is what the server reads.
        let mut props = plan.properties.clone();
        props.insert("format-version".into(), (version as u8).to_string());
        let creation = TableCreation::builder()
            .name(env.table.clone())
            .schema(plan.iceberg_schema.clone())
            .properties(props)
            .format_version(version)
            .build();
        catalog.create_table(&ns, creation).await?
    };
    let t_ready = t0.elapsed();

    // Stale mode: another writer commits between our load and our commit, as Basin
    // Catalog's compaction or expiry would.
    if stale {
        let rival = open_catalog(env).await?;
        let t = rival.load_table(&ident).await?;
        let files = write_files(&t, &policy::cast(&source, &t)?).await?;
        commit::overwrite(env, &t, files).await?;
        eprintln!("rival commit done; committing with stale metadata");
    }

    let batch = policy::cast(&source, &table)?;
    let t_cast = t0.elapsed();
    let files = write_files(&table, &batch).await?;
    let bytes: u64 = files.iter().map(|f| f.file_size_in_bytes()).sum();
    let t_write = t0.elapsed();
    let outcome = commit::overwrite(env, &table, files).await;
    let t_commit = t0.elapsed();
    let snapshot_id = outcome?;

    // Readable: a fresh load reports the committed snapshot as current.
    let reloaded = catalog.load_table(&ident).await?;
    let current = reloaded.metadata().current_snapshot_id();
    let t_visible = t0.elapsed();
    if current != Some(snapshot_id) {
        bail!("current snapshot {current:?} != committed {snapshot_id}");
    }
    println!(
        "{}",
        serde_json::json!({
            "rows": rows,
            "format": format!("{version}"),
            "parquet_bytes": bytes,
            "snapshot_id": snapshot_id,
            "snapshots_in_metadata": reloaded.metadata().snapshots().count(),
            "ms": {
                "catalog_ready": t_ready.as_millis(),
                "cast": (t_cast - t_ready).as_millis(),
                "write_parquet": (t_write - t_cast).as_millis(),
                "commit": (t_commit - t_write).as_millis(),
                "reload_visible": (t_visible - t_commit).as_millis(),
                "total": t_visible.as_millis(),
            }
        })
    );
    Ok(())
}

async fn write_files(
    table: &iceberg::table::Table,
    batch: &arrow_array::RecordBatch,
) -> Result<Vec<iceberg::spec::DataFile>> {
    let schema = table.metadata().current_schema().clone();
    let pw = ParquetWriterBuilder::new(WriterProperties::builder().build(), schema);
    let rolling = RollingFileWriterBuilder::new_with_default_file_size(
        pw,
        table.file_io().clone(),
        DefaultLocationGenerator::new(table.metadata())?,
        DefaultFileNameGenerator::new(
            format!("export-{}", uuid::Uuid::new_v4()),
            None,
            DataFileFormat::Parquet,
        ),
    );
    let mut writer = DataFileWriterBuilder::new(rolling).build(None).await?;
    writer.write(batch.clone()).await?;
    Ok(writer.close().await?)
}
