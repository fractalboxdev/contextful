//! Benchmark tables for the Basin SQL query matrix: a fact table whose `key` is sorted
//! and whose `observed_at` advances with it, written as one file, as one large file per
//! day partition, or as many small files per day partition; and a 1k-row dimension.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use arrow_array::builder::{FixedSizeListBuilder, Float32Builder};
use arrow_array::{
    Decimal256Array, Float64Array, Int64Array, LargeStringArray, RecordBatch,
    TimestampMicrosecondArray, UInt32Array,
};
use arrow_buffer::i256;
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use futures::stream::{self, StreamExt, TryStreamExt};
use iceberg::spec::{DataFile, DataFileFormat, FormatVersion, Literal, PartitionKey, Struct, Transform, UnboundPartitionSpec};
use iceberg::table::Table;
use iceberg::writer::base_writer::data_file_writer::DataFileWriterBuilder;
use iceberg::writer::file_writer::location_generator::{DefaultFileNameGenerator, DefaultLocationGenerator};
use iceberg::writer::file_writer::rolling_writer::RollingFileWriterBuilder;
use iceberg::writer::file_writer::ParquetWriterBuilder;
use iceberg::writer::{IcebergWriter, IcebergWriterBuilder};
use iceberg::{Catalog, NamespaceIdent, TableCreation, TableIdent};
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;

use crate::{commit, open_catalog, policy, Env};

/// 2026-01-01T00:00:00Z in microseconds and in days since the epoch.
const BASE_US: i64 = 1_767_225_600_000_000;
const BASE_DAY: i32 = 20_454;
const DAY_US: i64 = 86_400_000_000;
pub const DIMS: i64 = 1_000;

pub struct FactSpec {
    pub rows: i64,
    pub days: i64,
    /// Files per day partition; 0 writes the whole table, unpartitioned, as one file.
    pub files_per_day: usize,
    pub payload: usize,
    pub epoch: i64,
    pub parallel: usize,
}

fn fact_schema() -> Schema {
    Schema::new(vec![
        Field::new("key", DataType::Int64, false),
        Field::new("observed_at", DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())), false),
        Field::new("dim_id", DataType::Int64, false),
        Field::new("label", DataType::LargeUtf8, false),
        Field::new("hits", DataType::UInt32, false),
        Field::new("amount", DataType::Decimal256(38, 6), true),
        Field::new(
            "embedding",
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), 4),
            true,
        ),
        Field::new("payload", DataType::LargeUtf8, true),
        Field::new("epoch", DataType::Int64, false),
    ])
}

/// Rows `[from, to)` of the fact table: `key` is the row number, so it is sorted, and
/// `observed_at` spreads the rows evenly over `days` days in key order.
fn fact_batch(spec: &FactSpec, from: i64, to: i64) -> Result<RecordBatch> {
    let n = (to - from) as usize;
    let step = spec.days * DAY_US / spec.rows;
    let dim = |k: i64| (k * 7919) % DIMS;
    let mut emb = FixedSizeListBuilder::new(Float32Builder::new(), 4);
    for k in from..to {
        for d in 0..4 {
            emb.values().append_value(k as f32 + d as f32 / 10.0);
        }
        emb.append(true);
    }
    // Incompressible-enough filler so file sizes match the requested layout.
    const ALPHA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut state = 0x9E37_79B9_7F4A_7C15u64 ^ from as u64;
    let mut buf = String::with_capacity(spec.payload);
    let payload = LargeStringArray::from_iter_values((0..n).map(|_| {
        buf.clear();
        for _ in 0..spec.payload {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            buf.push(ALPHA[(state % 62) as usize] as char);
        }
        buf.clone()
    }));
    Ok(RecordBatch::try_new(
        Arc::new(fact_schema()),
        vec![
            Arc::new(Int64Array::from_iter_values(from..to)),
            Arc::new(TimestampMicrosecondArray::from_iter_values((from..to).map(|k| BASE_US + k * step)).with_timezone("UTC")),
            Arc::new(Int64Array::from_iter_values((from..to).map(dim))),
            Arc::new(LargeStringArray::from_iter_values((from..to).map(|k| format!("k{:03}", dim(k))))),
            Arc::new(UInt32Array::from_iter_values((from..to).map(|k| (k % 997) as u32))),
            Arc::new(
                Decimal256Array::from_iter_values((from..to).map(|k| i256::from_i128(k as i128 * 1_000_001)))
                    .with_precision_and_scale(38, 6)?,
            ),
            Arc::new(emb.finish()),
            Arc::new(payload),
            Arc::new(Int64Array::from_iter_values(std::iter::repeat_n(spec.epoch, n))),
        ],
    )?)
}

fn dim_batch() -> Result<RecordBatch> {
    let regions = ["apac", "emea", "amer", "latam", "anz", "mena", "nordics", "dach"];
    Ok(RecordBatch::try_new(
        Arc::new(Schema::new(vec![
            Field::new("dim_id", DataType::Int64, false),
            Field::new("region", DataType::LargeUtf8, false),
            Field::new("weight", DataType::Float64, false),
        ])),
        vec![
            Arc::new(Int64Array::from_iter_values(0..DIMS)),
            Arc::new(LargeStringArray::from_iter_values((0..DIMS).map(|i| regions[(i % 8) as usize]))),
            Arc::new(Float64Array::from_iter_values((0..DIMS).map(|i| i as f64 / 10.0))),
        ],
    )?)
}

async fn ensure_table(
    catalog: &impl Catalog,
    env: &Env,
    schema: &Schema,
    partitioned: bool,
) -> Result<Table> {
    let ns = NamespaceIdent::new(env.ns.clone());
    if !catalog.namespace_exists(&ns).await? {
        catalog.create_namespace(&ns, HashMap::new()).await?;
    }
    let ident = TableIdent::new(ns.clone(), env.table.clone());
    if catalog.table_exists(&ident).await? {
        return Ok(catalog.load_table(&ident).await?);
    }
    let plan = policy::plan(schema, FormatVersion::V2)?;
    let mut props = plan.properties.clone();
    props.insert("format-version".into(), "2".into());
    props.insert("write.parquet.compression-codec".into(), "zstd".into());
    let builder = TableCreation::builder()
        .name(env.table.clone())
        .schema(plan.iceberg_schema.clone())
        .properties(props)
        .format_version(FormatVersion::V2);
    let creation = if partitioned {
        let source = plan.iceberg_schema.field_by_name("observed_at").context("observed_at")?.id;
        let spec = UnboundPartitionSpec::builder()
            .with_spec_id(0)
            .add_partition_field(source, "observed_at_day", Transform::Day)?
            .build();
        builder.partition_spec(spec).build()
    } else {
        builder.build()
    };
    Ok(catalog.create_table(&ns, creation).await?)
}

fn writer_props() -> WriterProperties {
    WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(1).unwrap()))
        .set_max_row_group_row_count(Some(131_072))
        .build()
}

/// Writes one Parquet file holding `batch`, under `partition` when the table has one.
async fn write_one(table: &Table, batch: RecordBatch, partition: Option<i32>) -> Result<DataFile> {
    let schema = table.metadata().current_schema().clone();
    let pw = ParquetWriterBuilder::new(writer_props(), schema.clone());
    let rolling = RollingFileWriterBuilder::new(
        pw,
        usize::MAX,
        table.file_io().clone(),
        DefaultLocationGenerator::new(table.metadata())?,
        DefaultFileNameGenerator::new(format!("export-{}", uuid::Uuid::new_v4()), None, DataFileFormat::Parquet),
    );
    let key = partition.map(|day| {
        PartitionKey::new(
            table.metadata().default_partition_spec().as_ref().clone(),
            schema.clone(),
            Struct::from_iter([Some(Literal::date(day))]),
        )
    });
    let mut w = DataFileWriterBuilder::new(rolling).build(key).await?;
    w.write(batch).await?;
    let mut files = w.close().await?;
    Ok(files.pop().context("no file written")?)
}

pub async fn export_fact(env: &Env, spec: FactSpec) -> Result<()> {
    let t0 = Instant::now();
    let catalog = open_catalog(env).await?;
    let partitioned = spec.files_per_day > 0;
    let table = ensure_table(&catalog, env, &fact_schema(), partitioned).await?;
    let t_ready = t0.elapsed();

    // One unit per output file: (from, to, partition day).
    let units: Vec<(i64, i64, Option<i32>)> = if partitioned {
        let per_day = spec.rows / spec.days;
        let per_file = per_day / spec.files_per_day as i64;
        (0..spec.days)
            .flat_map(|d| {
                (0..spec.files_per_day as i64).map(move |f| {
                    let from = d * per_day + f * per_file;
                    (from, from + per_file, Some(BASE_DAY + d as i32))
                })
            })
            .collect()
    } else {
        vec![(0, spec.rows, None)]
    };
    let spec = Arc::new(spec);
    let files: Vec<DataFile> = stream::iter(units)
        .map(|(from, to, day)| {
            let (spec, table) = (spec.clone(), table.clone());
            async move {
                tokio::spawn(async move {
                    let batch = policy::cast(&fact_batch(&spec, from, to)?, &table)?;
                    write_one(&table, batch, day).await
                })
                .await?
            }
        })
        .buffer_unordered(spec.parallel)
        .try_collect()
        .await?;
    let bytes: u64 = files.iter().map(|f| f.file_size_in_bytes()).sum();
    let count = files.len();
    let t_write = t0.elapsed();
    let snapshot_id = commit::overwrite(env, &table, files).await?;
    let t_commit = t0.elapsed();
    let ident = TableIdent::new(NamespaceIdent::new(env.ns.clone()), env.table.clone());
    let current = catalog.load_table(&ident).await?.metadata().current_snapshot_id();
    let t_visible = t0.elapsed();
    anyhow::ensure!(current == Some(snapshot_id), "snapshot not current after commit");
    println!(
        "{}",
        serde_json::json!({
            "table": env.table, "rows": spec.rows, "files": count, "bytes": bytes,
            "epoch": spec.epoch, "snapshot_id": snapshot_id,
            "ms": {
                "catalog_ready": t_ready.as_millis(),
                "generate_cast_write_upload": (t_write - t_ready).as_millis(),
                "commit": (t_commit - t_write).as_millis(),
                "reload_visible": (t_visible - t_commit).as_millis(),
                "total": t_visible.as_millis(),
            }
        })
    );
    Ok(())
}

pub async fn export_dim(env: &Env) -> Result<()> {
    let catalog = open_catalog(env).await?;
    let source = dim_batch()?;
    let table = ensure_table(&catalog, env, source.schema().as_ref(), false).await?;
    let file = write_one(&table, policy::cast(&source, &table)?, None).await?;
    let id = commit::overwrite(env, &table, vec![file]).await?;
    println!("{}", serde_json::json!({"table": env.table, "rows": DIMS, "snapshot_id": id}));
    Ok(())
}
