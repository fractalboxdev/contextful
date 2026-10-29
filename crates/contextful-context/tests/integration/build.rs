//! `run.model` and `run.publish` over the store: a model build materialized into staging,
//! published through the pointer with its manifest section, held, and logged.
#![cfg(feature = "read")]

use crate::support::{at, decl, Fixture};
use contextful_context::build::{build, current_section, hold, holds, manifests, status, BuildRequest, Built};
use contextful_context::read::{Face, ReadFault};
use contextful_core::pipeline::declare::{collect, ManifestFile};
use contextful_core::pipeline::model::{
    collect_models, BuildEntry, ContractHistoryEntry, HoldRecord, ModelSpec, Receipt, BUILDS_LOG, CONTRACT_HISTORY_LOG, HOLDS_LOG,
};
use contextful_core::pipeline::model::disclosure_digest;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::{MANIFEST_FILE, POINTER_FILE};
use contextful_policy::enforce::mask::Pepper;
use serde_json::{json, Value};
use std::path::PathBuf;

const MODEL: &str = r#"
[[model]]
id = "daily"
sql = "SELECT day, CAST(count(*) AS BIGINT) AS n FROM events GROUP BY day"
unique_key = ["day"]

[model.contract]
version = "1.0.0"
columns = [{ name = "day", type = "utf8", nullable = false }, { name = "n", type = "int64", nullable = false }]

[model.freshness]
max_lag = "1d"

[[model.test]]
name = "positive"
sql = "SELECT * FROM daily WHERE n <= 0"
"#;

const TABLES: &str = "[[pipeline.tables]]\nname = \"events\"\n";

struct Project {
    fx: Fixture,
}

impl Project {
    fn new() -> Project {
        let p = Project { fx: Fixture::new() };
        p.land("r1", json!([{"day": "d1", "v": 1}, {"day": "d1", "v": 2}, {"day": "d2", "v": 3}]), "2030-01-01T00:00:00Z");
        p
    }

    fn land(&self, run: &str, rows: Value, now: &str) {
        self.fx.land(&decl("name = \"events\""), run, rows, now).unwrap();
    }

    fn face(&self, manifest: &str) -> Face {
        Face::open(self.fx.store.clone(), &format!("{TABLES}{manifest}"), Pepper::resolve(|_| None)).unwrap()
    }

    fn build(&self, manifest: &str, now: &str) -> Result<Built, ReadFault> {
        self.build_over(TABLES, manifest, now)
    }

    /// Build under `tables` in place of the default `events` declaration.
    fn build_over(&self, tables: &str, manifest: &str, now: &str) -> Result<Built, ReadFault> {
        let spec = model_over(tables, manifest);
        let face = Face::open(self.fx.store.clone(), &format!("{tables}{manifest}"), Pepper::resolve(|_| None)).unwrap();
        build(&face, &BuildRequest { model: &spec, site_id: "site-a", started_at: at(now), completed_at: at(now), available_as_of: None })
    }

    fn rows(&self, sql: &str) -> Vec<Vec<Value>> {
        self.face("").operator_query(sql, Default::default()).unwrap().rows
    }

    fn dir(&self) -> PathBuf {
        self.fx.table_dir("daily")
    }

    fn log<T: serde::de::DeserializeOwned>(&self, name: &str) -> Vec<T> {
        std::fs::read_to_string(self.dir().join(name)).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }
}

fn model_over(tables: &str, manifest: &str) -> ModelSpec {
    let files = [ManifestFile { path: "contextful.toml".into(), text: format!("{tables}{manifest}") }];
    collect_models(&files, &collect(&files).unwrap()).unwrap().remove(0).spec
}

fn refused(r: Result<Built, ReadFault>, error: &str) -> String {
    let e = r.expect_err(error).to_string();
    assert!(e.starts_with(&format!("{error}:")), "expected {error}, got {e}");
    e
}

/// The snapshot manifest commit publishes a table's data, contract identity, freshness and current build together. No
/// separate file is authoritative for these values.
// spec: run.publish.manifest-commit@222ca604
#[test]
fn a_build_publishes_its_section_in_the_snapshot_manifest_the_pointer_names() {
    let p = Project::new();
    let built = p.build(MODEL, "2030-01-01T01:00:00Z").unwrap();
    assert_eq!(built.rows, 2);
    let pointer: Value = serde_json::from_slice(&std::fs::read(p.dir().join(POINTER_FILE)).unwrap()).unwrap();
    let snapshot = pointer["snapshot_id"].as_str().unwrap();
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(p.dir().join("data/snapshots").join(snapshot).join(MANIFEST_FILE)).unwrap()).unwrap();
    let section = &manifest["publish"];
    assert_eq!(section["build_id"], snapshot);
    assert_eq!(section["contract_version"], "1.0.0");
    assert_eq!(section["max_lag"], "1d");
    assert_eq!(section["last_build_status"], "published");
    assert_eq!(section["watermark"]["inputs"]["events"]["omitted"]["count"], 1);
    assert_eq!(section["watermark"]["inputs"]["events"]["through"], 1);
    // The logs are derived: removing every one leaves the section, and the published rows, standing.
    for log in [BUILDS_LOG, HOLDS_LOG, CONTRACT_HISTORY_LOG] {
        let _ = std::fs::remove_file(p.dir().join(log));
    }
    assert_eq!(current_section(&p.fx.store, "daily").unwrap().unwrap().build_id, snapshot);
    assert_eq!(p.rows("SELECT day, n FROM daily ORDER BY day"), [[json!("d1"), json!("2")], [json!("d2"), json!("1")]]);
}

/// A build's id is the snapshot id it publishes.
// spec: run.model.build-id@ab02924b
#[test]
fn a_build_id_is_the_snapshot_id() {
    let p = Project::new();
    let built = p.build(MODEL, "2030-01-01T01:00:00Z").unwrap();
    assert_eq!(built.build_id, "snapshot-01893459600000000000");
    assert!(p.dir().join("data/snapshots").join(&built.build_id).join(MANIFEST_FILE).is_file());
    assert_eq!(p.rows("SELECT DISTINCT _run_id FROM daily"), [[json!(built.build_id)]]);
}

/// A materialization whose columns, types or grain miss the declared contract raises `PipelineContractMismatch`, naming the column and the expectation.
// spec: run.publish.contract-mismatch@5e070932
#[test]
fn rows_missing_the_contract_are_refused_naming_the_column() {
    let p = Project::new();
    let typed = MODEL.replace("CAST(count(*) AS BIGINT)", "CAST(count(*) AS VARCHAR)");
    let e = refused(p.build(&typed, "2030-01-01T01:00:00Z"), "PipelineContractMismatch");
    assert!(e.contains("`n` is VARCHAR") && e.contains("declares int64"), "{e}");
    let missing = MODEL.replace("CAST(count(*) AS BIGINT) AS n", "CAST(count(*) AS BIGINT) AS total");
    let e = refused(p.build(&missing, "2030-01-01T01:00:00Z"), "PipelineContractMismatch");
    assert!(e.contains("`total`"), "{e}");
    let nulls = MODEL.replace("SELECT day,", "SELECT CAST(NULL AS VARCHAR) AS day,").replace("GROUP BY day", "GROUP BY events.day");
    let e = refused(p.build(&nulls, "2030-01-01T01:00:00Z"), "PipelineContractMismatch");
    assert!(e.contains("`day`") && e.contains("non-null"), "{e}");
}

/// `unique_key` defines the model's row identity. A build with duplicate key values or a null in a key column refuses
/// as {{run.publish.contract-mismatch}}.
// spec: run.model.unique-key@b5e84bb5
#[test]
fn rows_repeating_the_grain_are_refused() {
    let p = Project::new();
    let repeated = MODEL.replace("GROUP BY day", "GROUP BY day, v");
    let e = refused(p.build(&repeated, "2030-01-01T01:00:00Z"), "PipelineContractMismatch");
    assert!(e.contains("1 rows repeat") && e.contains("(day)"), "{e}");
    let nullable = MODEL.replace("nullable = false }, { name = \"n\"", "nullable = true }, { name = \"n\"");
    p.land("r2", json!([{"v": 9}]), "2030-01-01T00:30:00Z");
    let e = refused(p.build(&nullable, "2030-01-01T01:00:00Z"), "PipelineContractMismatch");
    assert!(e.contains("grain column `day` holds 1 nulls"), "{e}");
}

/// A build materializes into staging and publishes through {{run.publish.manifest-commit}}; a refused build leaves the last published state serving.
// spec: run.publish.staging@ab759e12
#[test]
fn a_refused_build_leaves_the_last_published_state_serving() {
    let p = Project::new();
    let first = p.build(MODEL, "2030-01-01T01:00:00Z").unwrap();
    p.land("r2", json!([{"day": "d3", "v": 1}]), "2030-01-01T02:00:00Z");
    let broken = MODEL.replace("CAST(count(*) AS BIGINT)", "CAST(count(*) AS VARCHAR)");
    refused(p.build(&broken, "2030-01-01T03:00:00Z"), "PipelineContractMismatch");
    assert_eq!(current_section(&p.fx.store, "daily").unwrap().unwrap().build_id, first.build_id);
    assert_eq!(p.rows("SELECT count(*) FROM daily"), [[json!("2")]]);
    // Nothing the refused build staged stays behind.
    let dirs: Vec<String> = std::fs::read_dir(p.dir().join("data/snapshots")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(dirs, std::slice::from_ref(&first.build_id));
    let second = p.build(MODEL, "2030-01-01T04:00:00Z").unwrap();
    assert_eq!(p.rows("SELECT count(*) FROM daily"), [[json!("3")]]);
    assert_ne!(second.build_id, first.build_id);
}

/// A failing test raises `ModelTestFailed`, naming the test and its row count; the build publishes nothing.
// spec: run.model.test-failed@c6f5ce3c
#[test]
fn a_failing_test_refuses_the_build() {
    let p = Project::new();
    let first = p.build(MODEL, "2030-01-01T01:00:00Z").unwrap();
    let strict = MODEL.replace("WHERE n <= 0", "WHERE n < 2");
    let e = refused(p.build(&strict, "2030-01-01T02:00:00Z"), "ModelTestFailed");
    assert!(e.contains("`positive`") && e.contains("returned 1 rows"), "{e}");
    assert_eq!(current_section(&p.fx.store, "daily").unwrap().unwrap().build_id, first.build_id);
}

/// A `[[model.test]]` carries a `name` and one `SELECT` over the staged rows, registered under the model's id, and the store's tables; a test returning any row fails.
// spec: run.model.test-block@88134371
#[test]
fn a_test_reads_the_staged_rows_beside_the_store_tables() {
    let p = Project::new();
    // The staged rows, not the published ones, answer the test: the first build has none published.
    let joined = MODEL.replace(
        "SELECT * FROM daily WHERE n <= 0",
        "SELECT d.day FROM daily d LEFT JOIN (SELECT day FROM events GROUP BY day) e ON d.day = e.day WHERE e.day IS NULL",
    );
    p.build(&joined, "2030-01-01T01:00:00Z").unwrap();
    let bad = MODEL.replace("SELECT * FROM daily WHERE n <= 0", "SELECT * FROM read_csv('x.csv')");
    let e = p.build(&bad, "2030-01-01T02:00:00Z").unwrap_err().to_string();
    assert!(e.contains("TableFunctionRefused"), "{e}");
}

/// The selected statement is one read-only SELECT over store tables.
// spec: run.model.sql@fe4f6b92
#[test]
fn model_sql_is_admitted_over_store_tables() {
    let p = Project::new();
    let e = p.build(&MODEL.replace("FROM events GROUP BY day", "FROM read_parquet('x.parquet') GROUP BY day"), "2030-01-01T01:00:00Z");
    assert!(e.unwrap_err().to_string().contains("TableFunctionRefused"));
    let e = p.build(&MODEL.replace("FROM events", "FROM elsewhere"), "2030-01-01T01:00:00Z").unwrap_err().to_string();
    assert!(e.contains("elsewhere"), "{e}");
    assert!(!p.dir().join("data/snapshots").read_dir().is_ok_and(|mut d| d.next().is_some()), "no row was staged");
    p.build(MODEL, "2030-01-01T01:00:00Z").unwrap();
    let over = "[[model]]\nid = \"busy\"\nsql = \"SELECT day FROM daily WHERE n > 1\"\npublish = false\n";
    let built = p.build(over, "2030-01-01T02:00:00Z").unwrap();
    assert_eq!(built.watermark.inputs.keys().collect::<Vec<_>>(), ["daily"]);
    assert_eq!(p.rows("SELECT day FROM busy"), [[json!("d1")]]);
}

/// A model declaring `publish = false` commits its rows with no manifest section, so no build entry, hold or contract history names it.
// spec: run.model.unpublished@fb4b6a24
#[test]
fn an_unpublished_model_commits_rows_without_a_section() {
    let p = Project::new();
    let m = "[[model]]\nid = \"daily\"\nsql = \"SELECT day, count(*) AS n FROM events GROUP BY day\"\npublish = false\n";
    let built = p.build(m, "2030-01-01T01:00:00Z").unwrap();
    assert!(built.section.is_none());
    assert_eq!(p.rows("SELECT count(*) FROM daily"), [[json!("2")]]);
    assert!(manifests(&p.fx.store, "daily").unwrap()[0].publish.is_none());
    assert!(!p.dir().join(BUILDS_LOG).exists());
    let e = hold(&p.fx.store, "daily", &built.build_id, "ops", 60, at("2030-01-01T02:00:00Z")).unwrap_err().to_string();
    assert!(e.starts_with("ModelBuildUnknown:"), "{e}");
}

/// The `MODEL` contract at `version`, with the column list `columns` and the select list `select`.
fn reshaped(version: &str, select: &str, columns: &str) -> String {
    MODEL
        .replace("\"1.0.0\"", &format!("\"{version}\""))
        .replace("day, CAST(count(*) AS BIGINT) AS n", select)
        .replace(r#"{ name = "day", type = "utf8", nullable = false }, { name = "n", type = "int64", nullable = false }"#, columns)
        .replace("SELECT * FROM daily WHERE n <= 0", "SELECT * FROM daily WHERE day IS NULL")
}

/// Under the published build's major, a contract that drops or retypes a published column, makes a non-null one nullable, adds a non-null column or changes the grain refuses as {{run.publish.contract-mismatch}}, naming the column and the major to bump.
// spec: run.model.contract-major@4949f27a
#[test]
fn a_breaking_change_under_one_major_is_refused_naming_the_column_until_the_major_moves() {
    let p = Project::new();
    let first = p.build(MODEL, "2030-01-01T01:00:00Z").unwrap().build_id;
    let day = r#"{ name = "day", type = "utf8", nullable = false }"#;
    // The contract itself drops `n`, under a minor bump.
    let dropped = reshaped("1.1.0", "day", day);
    let e = refused(p.build(&dropped, "2030-01-01T02:00:00Z"), "PipelineContractMismatch");
    assert!(e.contains("`n`") && e.contains("under major version 1") && e.contains("2.0.0"), "{e}");
    // A retyped column names itself.
    let retyped = reshaped("1.1.0", "day, CAST(count(*) AS DOUBLE) AS n", &format!(r#"{day}, {{ name = "n", type = "float64", nullable = false }}"#));
    let e = refused(p.build(&retyped, "2030-01-01T02:00:00Z"), "PipelineContractMismatch");
    assert!(e.contains("`n`") && e.contains("float64") && e.contains("2.0.0"), "{e}");
    // An added column a reader cannot find null in breaks the old readers' inserts and joins alike.
    let required = reshaped(
        "1.1.0",
        "day, CAST(count(*) AS BIGINT) AS n, 'x' AS tag",
        &format!(r#"{day}, {{ name = "n", type = "int64", nullable = false }}, {{ name = "tag", type = "utf8", nullable = false }}"#),
    );
    let e = refused(p.build(&required, "2030-01-01T02:00:00Z"), "PipelineContractMismatch");
    assert!(e.contains("`tag`") && e.contains("2.0.0"), "{e}");
    // A non-null column made nullable lets a reader meet a null it never handled.
    let loosened = reshaped("1.1.0", "day, CAST(count(*) AS BIGINT) AS n", &format!(r#"{day}, {{ name = "n", type = "int64", nullable = true }}"#));
    let e = refused(p.build(&loosened, "2030-01-01T02:00:00Z"), "PipelineContractMismatch");
    assert!(e.contains("`n`") && e.contains("nullable") && e.contains("2.0.0"), "{e}");
    // A moved grain changes what one row means.
    let regrained = MODEL.replace("\"1.0.0\"", "\"1.1.0\"").replace(r#"unique_key = ["day"]"#, r#"unique_key = ["day", "n"]"#);
    let e = refused(p.build(&regrained, "2030-01-01T02:00:00Z"), "PipelineContractMismatch");
    assert!(e.contains("grain") && e.contains("2.0.0"), "{e}");
    // The last published build keeps serving.
    assert_eq!(current_section(&p.fx.store, "daily").unwrap().unwrap().build_id, first);
    assert_eq!(p.rows("SELECT day, n FROM daily ORDER BY day"), [[json!("d1"), json!("2")], [json!("d2"), json!("1")]]);
    p.build(&dropped.replace("\"1.1.0\"", "\"2.0.0\""), "2030-01-01T03:00:00Z").unwrap();
    let history: Vec<ContractHistoryEntry> = p.log(CONTRACT_HISTORY_LOG);
    assert_eq!(history.iter().map(|h| h.contract_version.as_str()).collect::<Vec<_>>(), ["1.0.0", "2.0.0"]);
    // A released build's grain, less the contributor key, holds to the same rule.
    let g = Project::new();
    build_governed(&g, GOVERNED, "2030-01-01T01:00:00Z").unwrap();
    let regrained = GOVERNED.replace("\"1.0.0\"", "\"1.1.0\"").replace(r#"unique_key = ["day", "who"]"#, r#"unique_key = ["day", "n", "who"]"#);
    let e = refused(build_governed(&g, &regrained, "2030-01-01T02:00:00Z"), "PipelineContractMismatch");
    assert!(e.contains("grain") && e.contains("2.0.0"), "{e}");
}

/// Under the published build's major, a contract that only adds nullable columns or makes nullable ones non-null publishes once its minor exceeds the published one, else refuses as {{run.publish.contract-mismatch}}, naming the added or tightened column and the bumped minor.
// spec: run.model.contract-minor@c6510d6d
#[test]
fn a_nullable_column_added_under_a_minor_bump_publishes_its_new_identity() {
    let p = Project::new();
    p.build(MODEL, "2030-01-01T01:00:00Z").unwrap();
    let columns = r#"{ name = "day", type = "utf8", nullable = false }, { name = "n", type = "int64", nullable = false }, { name = "tag", type = "utf8" }"#;
    let added = |version: &str| reshaped(version, "day, CAST(count(*) AS BIGINT) AS n, NULL::VARCHAR AS tag", columns);
    // The same version over a moved fingerprint names the minor to bump.
    let e = refused(p.build(&added("1.0.0"), "2030-01-01T02:00:00Z"), "PipelineContractMismatch");
    assert!(e.contains("`tag`") && e.contains("1.1.0"), "{e}");
    let built = p.build(&added("1.1.0"), "2030-01-01T03:00:00Z").unwrap();
    let section = built.section.unwrap();
    assert_eq!(section.contract_version, "1.1.0");
    assert_eq!(Some(section.schema_fingerprint.clone()), model_over(TABLES, &added("1.1.0")).schema_fingerprint());
    let committed = current_section(&p.fx.store, "daily").unwrap().unwrap();
    assert_eq!((committed.contract_version, committed.schema_fingerprint), ("1.1.0".to_string(), section.schema_fingerprint));
    assert_eq!(p.rows("SELECT day, n, tag FROM daily ORDER BY day"), [[json!("d1"), json!("2"), Value::Null], [json!("d2"), json!("1"), Value::Null]]);
    let history: Vec<ContractHistoryEntry> = p.log(CONTRACT_HISTORY_LOG);
    assert_eq!(history.iter().map(|h| h.contract_version.as_str()).collect::<Vec<_>>(), ["1.0.0", "1.1.0"]);
    // Tightening `tag` to non-null is additive and names the column.
    let tight = r#"{ name = "day", type = "utf8", nullable = false }, { name = "n", type = "int64", nullable = false }, { name = "tag", type = "utf8", nullable = false }"#;
    let tightened = |version: &str| reshaped(version, "day, CAST(count(*) AS BIGINT) AS n, 'x' AS tag", tight);
    let e = refused(p.build(&tightened("1.1.0"), "2030-01-01T04:00:00Z"), "PipelineContractMismatch");
    assert!(e.contains("`tag`") && e.contains("non-null") && e.contains("1.2.0"), "{e}");
    assert_eq!(p.build(&tightened("1.2.0"), "2030-01-01T05:00:00Z").unwrap().section.unwrap().contract_version, "1.2.0");
    // Reordering the columns alone names the reorder.
    let reordered = reshaped("1.2.0", "day, CAST(count(*) AS BIGINT) AS n, 'x' AS tag", r#"{ name = "tag", type = "utf8", nullable = false }, { name = "day", type = "utf8", nullable = false }, { name = "n", type = "int64", nullable = false }"#);
    let e = refused(p.build(&reordered, "2030-01-01T06:00:00Z"), "PipelineContractMismatch");
    assert!(e.contains("reorders") && e.contains("1.3.0"), "{e}");
    // A released build, whose published schema omits the contributor key, takes the same minor bump.
    let g = Project::new();
    build_governed(&g, GOVERNED, "2030-01-01T01:00:00Z").unwrap();
    let tagged = GOVERNED
        .replace("\"1.0.0\"", "\"1.1.0\"")
        .replace("CAST(count(*) AS BIGINT) AS n FROM", "CAST(count(*) AS BIGINT) AS n, NULL::VARCHAR AS tag FROM")
        .replace(r#"{ name = "n", type = "int64", nullable = false }]"#, r#"{ name = "n", type = "int64", nullable = false }, { name = "tag", type = "utf8" }]"#);
    assert_eq!(build_governed(&g, &tagged, "2030-01-01T02:00:00Z").unwrap().section.unwrap().contract_version, "1.1.0");
}

/// A build lands `_ingested_at` as its start instant, `_run_id` as its build id, `_row_seq` as the row's position, `_commit_seq` as its commit's value and `_site_id`, replacing any injected column the SQL selects; `semantics_version` is 2.
// spec: run.model.injected-columns@e92dca85
#[test]
fn a_build_injects_the_five_columns_over_any_the_sql_selects() {
    let p = Project::new();
    let m = "[[model]]\nid = \"daily\"\nsql = \"SELECT * FROM events\"\npublish = false\n";
    let built = p.build(m, "2030-01-01T01:00:00Z").unwrap();
    let rows = p.rows("SELECT DISTINCT CAST(_ingested_at AS VARCHAR), _run_id, _site_id FROM daily");
    assert_eq!(rows.len(), 1);
    assert!(rows[0][0].as_str().unwrap().starts_with("2030-01-01 01:00:00"), "{rows:?}");
    assert_eq!(rows[0][1], json!(built.build_id));
    assert_eq!(rows[0][2], json!("site-a"));
    assert_eq!(p.rows("SELECT list(_row_seq ORDER BY _row_seq) FROM daily"), [[json!(["0", "1", "2"])]]);
    assert_eq!(p.rows("SELECT DISTINCT _commit_seq FROM daily"), [[json!("1")]]);
    let section = p.build(MODEL, "2030-01-01T02:00:00Z").unwrap().section.unwrap();
    assert_eq!(section.semantics_version, Some(3));
}

/// A build's watermark is `{at, inputs}`: per input table the snapshot id, `through`, the greatest `commit_seq` read, and `omitted`, the count and digest of runs read outside the snapshot chain; `at` is the newest commit instant.
// spec: run.model.watermark@a38ca23e
#[test]
fn the_watermark_names_each_input_frontier() {
    use contextful_core::pipeline::model::Omitted;
    let p = Project::new();
    p.land("r2", json!([{"day": "d3", "v": 1}]), "2030-01-01T00:20:00Z");
    let built = p.build(MODEL, "2030-01-01T01:00:00Z").unwrap();
    assert_eq!(built.watermark.at, Some(at("2030-01-01T00:20:00Z")));
    let events = &built.watermark.inputs["events"];
    assert_eq!(events.snapshot_id, None);
    assert_eq!(events.through, Some(2));
    assert_eq!(events.omitted, Some(Omitted::of(["r1/ingest-a", "r2/ingest-a"])));
    assert!(events.runs.is_empty());
    // A fold moves the runs into a snapshot the next build names.
    contextful_context::fold::fold(&p.fx.store, &decl("name = \"events\""), at("2030-01-01T02:00:00Z")).unwrap();
    let next = p.build(MODEL, "2030-01-01T03:00:00Z").unwrap();
    let events = &next.watermark.inputs["events"];
    assert!(events.snapshot_id.is_some(), "{events:?}");
    assert_eq!(events.omitted, Some(Omitted::of(Vec::<String>::new())));
    assert_eq!(events.through, Some(2));
    assert_eq!(next.watermark.at, Some(at("2030-01-01T02:00:00Z")));
}

/// An input no fold covers adds no bytes per run to the watermark the build commits.
#[test]
fn the_watermark_stays_a_fixed_size_across_unfolded_runs() {
    let p = Project::new();
    let size = |p: &Project, now: &str| {
        let built = p.build(MODEL, now).unwrap();
        let section = current_section(&p.fx.store, "daily").unwrap().unwrap();
        assert_eq!(section.watermark, built.watermark);
        serde_json::to_vec(&section.watermark.inputs["events"]).unwrap().len()
    };
    let one = size(&p, "2030-01-02T00:00:00Z");
    for i in 0..40 {
        p.land(&format!("x{i:02}"), json!([{"day": "d9", "v": i}]), "2030-01-01T00:30:00Z");
    }
    let many = size(&p, "2030-01-03T00:00:00Z");
    // One more digit in each of `through` and `count`.
    assert!(many <= one + 2, "{one} bytes over 1 run, {many} over 41");
}

/// A build reads past a grouped run awaiting its group marker, so a group that never
/// publishes holds no later build back; once the marker lands below `through`, the input
/// is stale at once (`run.publish.staleness-omitted-check`).
#[test]
fn a_group_published_below_through_makes_the_build_stale() {
    use contextful_context::land::{commit_parts_group, publish_group, Position, RunContext};
    use contextful_core::store::lay_out::NodeId;
    use contextful_core::store::reserve::Injection;
    let p = Project::new();
    let d = decl("name = \"events\"");
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: "grouped".into(), site_id: "site-a".into(), batch_seq: None, authored_by: None, authored_agent: None, taint: None },
        committed_at: at("2030-01-01T00:10:00Z"),
    };
    commit_parts_group(&p.fx.store, &d, &[], &ctx, &Position::default(), "events", &[], &|| Ok(()), &|_| Ok(())).unwrap();
    p.land("plain", json!([{"day": "d9", "v": 1}]), "2030-01-01T00:20:00Z");
    let built = p.build(MODEL, "2030-01-01T01:00:00Z").unwrap();
    let events = &built.watermark.inputs["events"];
    assert_eq!(events.through, Some(3), "{events:?}");
    assert_eq!(events.omitted.as_ref().map(|o| o.count), Some(2));
    assert_eq!(p.rows("SELECT count(*) FROM daily WHERE day = 'd9'"), [[json!("1")]]);
    // A fold of the readable runs leaves the unpublished group outside the snapshot.
    contextful_context::fold::fold(&p.fx.store, &d, at("2030-01-01T01:30:00Z")).unwrap();
    let folded = p.build(MODEL, "2030-01-01T02:00:00Z").unwrap();
    assert_eq!(folded.watermark.inputs["events"].through, Some(3));
    assert_eq!(status(&p.fx.store, "daily", at("2030-01-01T03:00:00Z")).unwrap()["stale"], false);
    publish_group(&p.fx.store, "events", &["events".to_string()], &ctx).unwrap();
    let stale = status(&p.fx.store, "daily", at("2030-01-01T03:00:00Z")).unwrap();
    assert_eq!(stale["stale_inputs"], json!(["events"]), "{stale}");
}

/// An input whose runs outside the watermark snapshot chain, at or below `through`, differ in count or digest from `omitted` holds a waiting run counted as committed before `t` minus `max_lag`.
// spec: run.publish.staleness-omitted-check@07cef60d
#[test]
fn a_run_below_through_the_build_never_read_makes_the_input_stale() {
    let p = Project::new();
    p.build(MODEL, "2030-01-01T01:00:00Z").unwrap();
    assert_eq!(status(&p.fx.store, "daily", at("2030-01-01T02:00:00Z")).unwrap()["stale"], false);
    p.land("merged", json!([{"day": "d9", "v": 1}]), "2030-01-01T01:30:00Z");
    let path = p.fx.table_dir("events").join(contextful_core::store::lay_out::RUNS_DIR).join("merged/ingest-a").join(MANIFEST_FILE);
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["commit_seq"] = json!(1);
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let stale = status(&p.fx.store, "daily", at("2030-01-01T02:00:00Z")).unwrap();
    assert_eq!(stale["stale_inputs"], json!(["events"]), "{stale}");
}

/// A watermark input carrying a `runs` list and no `omitted` reads with those runs included under `run.publish.staleness`; no build writes `runs`, and every echo of that watermark keeps the list.
// spec: run.model.watermark-listed-runs@a0f202c1
#[test]
fn a_watermark_listing_runs_is_still_honored() {
    let p = Project::new();
    let built = p.build(MODEL, "2030-01-01T01:00:00Z").unwrap();
    let path = p.dir().join(contextful_core::store::lay_out::SNAPSHOTS_DIR).join(&built.build_id).join(MANIFEST_FILE);
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["publish"]["watermark"]["inputs"]["events"] = json!({"snapshot_id": null, "runs": ["r1/ingest-a"]});
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert_eq!(current_section(&p.fx.store, "daily").unwrap().unwrap().watermark.inputs["events"].runs, ["r1/ingest-a"]);
    let echoed = status(&p.fx.store, "daily", at("2030-04-01T00:00:00Z")).unwrap();
    assert_eq!(echoed["freshness"]["watermark"]["inputs"]["events"]["runs"], json!(["r1/ingest-a"]), "{echoed}");
    assert_eq!(status(&p.fx.store, "daily", at("2030-04-01T00:00:00Z")).unwrap()["stale"], false);
    p.land("r2", json!([{"day": "d3", "v": 1}]), "2030-04-01T00:00:00Z");
    assert_eq!(status(&p.fx.store, "daily", at("2030-04-02T00:00:01Z")).unwrap()["stale_inputs"], json!(["events"]));
}

/// A published model is stale at `t` when an input holds a committed run outside its watermark snapshot chain, its `commit_seq` above `through`, committed before `t` minus `max_lag`; `build status --json` names each such input under `stale_inputs`.
#[test]
fn a_fold_after_the_build_leaves_it_fresh() {
    let p = Project::new();
    p.land("r2", json!([{"day": "d3", "v": 4}]), "2030-01-01T00:10:00Z");
    p.build(MODEL, "2030-01-01T01:00:00Z").unwrap();
    // The fold moves both runs the build read into a snapshot its watermark never named.
    contextful_context::fold::fold(&p.fx.store, &decl("name = \"events\""), at("2030-01-02T00:00:00Z")).unwrap();
    let fresh = status(&p.fx.store, "daily", at("2030-03-01T00:00:00Z")).unwrap();
    assert_eq!(fresh["stale"], false, "{fresh}");
    assert_eq!(fresh["stale_inputs"], json!([]));
    // A run the build did not read ages it once `max_lag` passes.
    p.land("r3", json!([{"day": "d4", "v": 5}]), "2030-03-01T00:00:00Z");
    assert_eq!(status(&p.fx.store, "daily", at("2030-03-01T23:59:59Z")).unwrap()["stale"], false);
    let stale = status(&p.fx.store, "daily", at("2030-03-02T00:00:01Z")).unwrap();
    assert_eq!(stale["stale"], true);
    assert_eq!(stale["stale_inputs"], json!(["events"]));
}

/// Runs a build read through its watermark snapshot's chain count as included while their manifests stay.
#[test]
fn runs_in_the_watermark_snapshot_chain_count_as_read() {
    let p = Project::new();
    p.land("r2", json!([{"day": "d3", "v": 4}]), "2030-01-01T00:10:00Z");
    contextful_context::fold::fold(&p.fx.store, &decl("name = \"events\""), at("2030-01-01T00:20:00Z")).unwrap();
    let built = p.build(MODEL, "2030-01-01T01:00:00Z").unwrap();
    let events = &built.watermark.inputs["events"];
    assert!(events.snapshot_id.is_some() && events.omitted.as_ref().is_some_and(|o| o.count == 0), "{events:?}");
    let runs = p.fx.store.committed_runs("events").unwrap();
    assert_eq!(runs.len(), 2, "both folded runs keep their manifests");
    let fresh = status(&p.fx.store, "daily", at("2030-03-01T00:00:00Z")).unwrap();
    assert_eq!(fresh["stale_inputs"], json!([]), "{fresh}");
    // A second fold parents on the first; only its new run waits.
    p.land("r3", json!([{"day": "d4", "v": 5}]), "2030-03-01T00:00:00Z");
    contextful_context::fold::fold(&p.fx.store, &decl("name = \"events\""), at("2030-03-01T00:30:00Z")).unwrap();
    assert_eq!(status(&p.fx.store, "daily", at("2030-03-01T23:59:59Z")).unwrap()["stale_inputs"], json!([]));
    assert_eq!(status(&p.fx.store, "daily", at("2030-03-02T00:00:01Z")).unwrap()["stale_inputs"], json!(["events"]));
}

const WEEKLY: &str = r#"
[[model]]
id = "weekly"
sql = "SELECT day, n FROM daily"
unique_key = ["day"]

[model.contract]
version = "1.0.0"
columns = [{ name = "day", type = "utf8", nullable = false }, { name = "n", type = "int64", nullable = false }]

[model.freshness]
max_lag = "1d"
"#;

/// An input that is a model also waits on each of its published builds newer than the watermark snapshot, aged by that build's `created_at`.
// spec: run.publish.staleness-model-input@42f56eee
#[test]
fn a_model_reading_a_model_ages_by_its_rebuilds() {
    let p = Project::new();
    p.build(MODEL, "2030-01-01T01:00:00Z").unwrap();
    let weekly = p.build(WEEKLY, "2030-01-01T02:00:00Z").unwrap();
    assert_eq!(weekly.watermark.inputs.keys().collect::<Vec<_>>(), ["daily"]);
    assert_eq!(status(&p.fx.store, "weekly", at("2030-04-01T00:00:00Z")).unwrap()["stale_inputs"], json!([]));
    // The upstream rebuild is a landing `weekly` has not read.
    p.build(MODEL, "2030-04-01T00:00:00Z").unwrap();
    assert_eq!(status(&p.fx.store, "weekly", at("2030-04-01T23:59:59Z")).unwrap()["stale_inputs"], json!([]));
    let stale = status(&p.fx.store, "weekly", at("2030-04-02T00:00:01Z")).unwrap();
    assert_eq!(stale["stale"], true, "{stale}");
    assert_eq!(stale["stale_inputs"], json!(["daily"]));
    // A rebuild over the current frontier clears it.
    p.build(WEEKLY, "2030-04-02T01:00:00Z").unwrap();
    assert_eq!(status(&p.fx.store, "weekly", at("2030-05-01T00:00:00Z")).unwrap()["stale_inputs"], json!([]));
}

/// A hold is a {{type:contextful_core::pipeline::model::HoldRecord}} committed as `holds/<build id>.json` in the
/// model's table directory, replacing any earlier hold on that build; collection skips a held build, and a hold
/// confers no other authority.
// spec: run.publish.hold@8b09a8b1
#[test]
fn collection_skips_a_held_build() {
    let p = Project::new();
    let held = p.build(MODEL, "2030-01-01T00:00:00Z").unwrap();
    let loose = p.build(MODEL, "2030-01-01T00:00:01Z").unwrap();
    let (_, record) = hold(&p.fx.store, "daily", &held.build_id, "ops", 30 * 86_400, at("2030-01-01T00:00:02Z")).unwrap();
    assert_eq!((record.build_id.as_str(), record.principal.as_str()), (held.build_id.as_str(), "ops"));
    assert_eq!(record.expires_at, at("2030-01-31T00:00:02Z"));
    p.build(MODEL, "2030-01-01T00:00:03Z").unwrap();
    // Past the 7 d retention window, the next build's collection takes the unheld build alone.
    p.build(MODEL, "2030-01-09T00:00:00Z").unwrap();
    let snapshots = p.dir().join("data/snapshots");
    assert!(snapshots.join(&held.build_id).is_dir(), "a held build survives collection");
    assert!(!snapshots.join(&loose.build_id).exists(), "an unheld build past the window is collected");
    // Once the hold expires, collection takes the held build too.
    p.build(MODEL, "2030-02-10T00:00:00Z").unwrap();
    assert!(!snapshots.join(&held.build_id).exists());
}

/// A hold commits as the hold manifest `holds/<build id>.json` in the model's table directory, replacing any earlier hold on that build.
// spec: run.publish.hold@8b09a8b1
#[test]
fn a_hold_commits_one_manifest_per_build() {
    let p = Project::new();
    let b = p.build(MODEL, "2030-01-01T00:00:00Z").unwrap();
    let (first, _) = hold(&p.fx.store, "daily", &b.build_id, "ops", 3_600, at("2030-01-01T01:00:00Z")).unwrap();
    let (second, record) = hold(&p.fx.store, "daily", &b.build_id, "audit", 7_200, at("2030-01-01T01:30:00Z")).unwrap();
    assert_eq!((first, second), (Receipt::Held, Receipt::Renewed));
    let on_disk: HoldRecord = serde_json::from_slice(&std::fs::read(p.dir().join("holds").join(format!("{}.json", b.build_id))).unwrap()).unwrap();
    assert_eq!(on_disk, record);
    assert_eq!(holds(&p.fx.store, "daily").unwrap(), [record]);
    // An expired hold is placed anew.
    let (third, _) = hold(&p.fx.store, "daily", &b.build_id, "ops", 60, at("2030-01-02T00:00:00Z")).unwrap();
    assert_eq!(third, Receipt::Held);
}

/// A hold naming a build no committed manifest of the model records raises `ModelBuildUnknown`.
// spec: run.model.hold-unknown-build@3454fc73
#[test]
fn a_hold_on_an_unknown_build_is_refused() {
    let p = Project::new();
    let b = p.build(MODEL, "2030-01-01T00:00:00Z").unwrap();
    let e = hold(&p.fx.store, "daily", "snapshot-00000000000000000001", "ops", 60, at("2030-01-01T01:00:00Z")).unwrap_err().to_string();
    assert!(e.starts_with("ModelBuildUnknown:") && e.contains(&b.build_id), "{e}");
    assert!(!p.dir().join("holds").join("snapshot-00000000000000000001.json").exists());
}

/// A new hold naming a build whose parts a collection removed raises `ModelBuildCollected`, while an unexpired
/// hold on it renews.
// spec: run.model.hold-collected@5ed41926
#[test]
fn a_hold_on_a_collected_build_is_refused() {
    let p = Project::new();
    let b = p.build(MODEL, "2030-01-01T00:00:00Z").unwrap();
    let (held, _) = hold(&p.fx.store, "daily", &b.build_id, "ops", 30 * 86_400, at("2030-01-01T01:00:00Z")).unwrap();
    let (renewed, _) = hold(&p.fx.store, "daily", &b.build_id, "ops", 30 * 86_400, at("2030-01-26T00:00:00Z")).unwrap();
    assert_eq!((held, renewed), (Receipt::Held, Receipt::Renewed));
    std::fs::write(p.dir().join("data/snapshots").join(&b.build_id).join("_collected.json"), br#"{"collected_at":"2030-02-01T00:00:00Z"}"#).unwrap();
    // An unexpired hold renews; once it lapses, a new hold refuses.
    let (again, _) = hold(&p.fx.store, "daily", &b.build_id, "ops", 60, at("2030-01-27T00:00:00Z")).unwrap();
    assert_eq!(again, Receipt::Renewed);
    let e = hold(&p.fx.store, "daily", &b.build_id, "ops", 60, at("2030-02-05T00:00:00Z")).unwrap_err().to_string();
    assert!(e.starts_with("ModelBuildCollected:") && e.contains(&b.build_id), "{e}");
}

/// `contract-history.jsonl`, `builds.jsonl` and `holds.jsonl` are append-only history derived from committed manifests; a log disagreeing with a manifest is regenerated from it.
#[test]
fn the_logs_follow_the_committed_manifests() {
    let p = Project::new();
    let a = p.build(MODEL, "2030-01-01T00:00:00Z").unwrap();
    let b = p.build(MODEL, "2030-01-01T01:00:00Z").unwrap();
    let builds: Vec<BuildEntry> = p.log(BUILDS_LOG);
    assert_eq!(builds.iter().map(|e| e.build_id.clone()).collect::<Vec<_>>(), [a.build_id.clone(), b.build_id.clone()]);
    let history: Vec<ContractHistoryEntry> = p.log(CONTRACT_HISTORY_LOG);
    assert_eq!(history.len(), 1);
    hold(&p.fx.store, "daily", &a.build_id, "ops", 60, at("2030-01-01T02:00:00Z")).unwrap();
    assert_eq!(p.log::<HoldRecord>(HOLDS_LOG).len(), 1);
}

/// The history logs sit in the model's table directory; each build and hold rewrites them, replacing an entry a manifest disagrees with and keeping entries of collected builds.
// spec: run.publish.history-logs@4e77d8b0
#[test]
fn a_tampered_log_is_rewritten_and_collected_history_kept() {
    let p = Project::new();
    let a = p.build(MODEL, "2030-01-01T00:00:00Z").unwrap();
    let path = p.dir().join(BUILDS_LOG);
    let tampered = std::fs::read_to_string(&path).unwrap().replace("\"published\"", "\"refused\"");
    std::fs::write(&path, format!("{tampered}not json\n")).unwrap();
    let b = p.build(MODEL, "2030-01-01T00:00:01Z").unwrap();
    let builds: Vec<BuildEntry> = p.log(BUILDS_LOG);
    assert_eq!(builds.len(), 2);
    assert!(builds.iter().all(|e| e.status == contextful_core::pipeline::model::BuildStatus::Published));
    // Retention collects `a` and `b`; their entries stay in the history.
    p.build(MODEL, "2030-01-01T00:00:02Z").unwrap();
    let c = p.build(MODEL, "2030-01-09T00:00:00Z").unwrap();
    assert!(!p.dir().join("data/snapshots").join(&a.build_id).exists());
    assert!(!p.log::<BuildEntry>(BUILDS_LOG).is_empty(), "the exclusion below ranges over no element");
    assert!(p.log::<BuildEntry>(BUILDS_LOG).iter().all(|e| e.status == contextful_core::pipeline::model::BuildStatus::Published),
        "retention preserves successful build outcomes");
    let ids: Vec<String> = p.log::<BuildEntry>(BUILDS_LOG).into_iter().map(|e| e.build_id).collect();
    assert_eq!(ids.len(), 4);
    assert_eq!((ids[0].as_str(), ids[1].as_str(), ids[3].as_str()), (a.build_id.as_str(), b.build_id.as_str(), c.build_id.as_str()));
}

#[test]
fn a_missing_build_log_is_recovered_before_retention_collects_its_manifests() {
    let p = Project::new();
    let first = p.build(MODEL, "2030-01-01T00:00:00Z").unwrap();
    p.build(MODEL, "2030-01-01T00:00:01Z").unwrap();
    std::fs::remove_file(p.dir().join(BUILDS_LOG)).unwrap();
    p.build(MODEL, "2030-01-09T00:00:00Z").unwrap();
    assert!(!p.dir().join("data/snapshots").join(&first.build_id).exists());
    let builds = p.log::<BuildEntry>(BUILDS_LOG);
    assert_eq!(builds.len(), 3);
    assert!(builds.iter().all(|e| e.status == contextful_core::pipeline::model::BuildStatus::Published),
        "committed manifests establish success before collection");
}

/// A build records a digest over its table's {{type:contextful_core::store::declare::TableDecl}} `class`, `policy`
/// and `visibility`, sorting set-valued fields; the manifest and build log carry it, and any declaration sets
/// `withheld_cells`.
// spec: run.publish.disclosure-digest@5283e83c
#[test]
fn the_disclosure_digest_covers_the_policy_the_model_table_reads_under() {
    let p = Project::new();
    let open = p.build(MODEL, "2030-01-01T00:00:00Z").unwrap().section.unwrap();
    assert_eq!(open.disclosure_digest, disclosure_digest(&TableDecl::named("daily")));
    assert!(!open.withheld_cells, "an undeclared table withholds no cell");
    let masked = format!("{MODEL}[[pipeline.tables]]\nname = \"daily\"\n[pipeline.tables.policy.columns]\nn = {{ strategy = \"drop\" }}\n");
    let section = p.build(&masked, "2030-01-01T01:00:00Z").unwrap().section.unwrap();
    assert_ne!(section.disclosure_digest, open.disclosure_digest);
    assert!(section.withheld_cells, "a masked table withholds cells");
    let logged: Vec<String> = p.log::<BuildEntry>(BUILDS_LOG).into_iter().map(|e| e.disclosure_digest).collect();
    assert_eq!(logged, [open.disclosure_digest.clone(), section.disclosure_digest.clone()]);
    assert_eq!(current_section(&p.fx.store, "daily").unwrap().unwrap().disclosure_digest, section.disclosure_digest);
}

/// A model's `id` names the store table it builds; an id declared twice, equal to a pipeline destination table, or naming a table a landing wrote refuses as {{run.declare.table-name-collision}}.
// spec: run.model.model-id@237baca3
#[test]
fn a_model_id_naming_a_landed_table_is_refused() {
    let p = Project::new();
    p.fx.land(&decl("name = \"daily\""), "r9", json!([{"note": "keep-me"}]), "2030-01-01T00:10:00Z").unwrap();
    let e = refused(p.build(MODEL, "2030-01-01T01:00:00Z"), "PipelineTableNameCollision");
    assert!(e.contains("`daily`"), "{e}");
    assert_eq!(p.rows("SELECT note FROM daily"), [[json!("keep-me")]]);
    // A fold moves the runs into a snapshot, and the table still belongs to its landing.
    contextful_context::fold::fold(&p.fx.store, &decl("name = \"daily\""), at("2030-01-01T02:00:00Z")).unwrap();
    refused(p.build(MODEL, "2030-01-01T03:00:00Z"), "PipelineTableNameCollision");
    assert_eq!(p.rows("SELECT note FROM daily"), [[json!("keep-me")]]);
}

/// A build reading a table whose {{type:contextful_core::store::declare::TableDecl}} declares `class`, `policy` or
/// `visibility` raises `ModelInputRestricted`, naming the table and the declared keys.
// spec: run.model.restricted-input@1cf9c278
#[test]
fn a_build_over_a_restricted_input_is_refused() {
    let p = Project::new();
    let masked = "[[pipeline.tables]]\nname = \"events\"\n[pipeline.tables.policy.columns]\nv = { strategy = \"drop\" }\n";
    let e = refused(p.build_over(masked, MODEL, "2030-01-01T01:00:00Z"), "ModelInputRestricted");
    assert!(e.contains("`events`") && e.contains("policy"), "{e}");
    let classed = "[[pipeline.tables]]\nname = \"events\"\nclass = \"email\"\n";
    let e = refused(p.build_over(classed, MODEL, "2030-01-01T01:00:00Z"), "ModelInputRestricted");
    assert!(e.contains("class"), "{e}");
    assert!(current_section(&p.fx.store, "daily").unwrap().is_none(), "nothing was published");
    assert!(!p.dir().join("data/snapshots").read_dir().is_ok_and(|mut d| d.next().is_some()), "no row was staged");
    p.build(MODEL, "2030-01-01T02:00:00Z").unwrap();
}

/// A model table's declared `retain_runs` governs the collection each of its builds runs.
#[test]
fn collection_after_a_build_follows_the_declared_window() {
    let p = Project::new();
    let kept = format!("{MODEL}[[pipeline.tables]]\nname = \"daily\"\nretain_runs = \"30d\"\n");
    let first = p.build(&kept, "2030-01-01T01:00:00Z").unwrap();
    p.build(&kept, "2030-01-01T01:00:01Z").unwrap();
    p.build(&kept, "2030-01-09T00:00:00Z").unwrap();
    assert!(p.dir().join("data/snapshots").join(&first.build_id).is_dir(), "a 30 d window keeps a build 8 days old");
}

// spec: run.model.build-boundary@942d06ef
#[test]
fn a_build_publication_boundary_refusal_preserves_rows_and_watermark() {
    let p = Project::new();
    let old = p.build(MODEL, "2030-01-01T01:00:00Z").unwrap();
    let rows = p.rows("SELECT day,n FROM daily ORDER BY day");
    p.land("new", json!([{"day":"d3","v":4}]), "2030-01-01T02:00:00Z");
    let old_schema = serde_json::to_value(p.fx.store.try_schema("daily").unwrap()).unwrap();
    let changed = MODEL.replace("1.0.0", "2.0.0").replace("BIGINT", "VARCHAR").replace("type = \"int64\"", "type = \"utf8\"").replace("n <= 0", "n = '0'");
    let spec = model_over(TABLES, &changed);
    let face = p.face(&changed);
    let result = contextful_context::build::build_guarded(&face, &BuildRequest {
        model: &spec, site_id: "site-a", started_at: at("2030-01-01T03:00:00Z"), completed_at: at("2030-01-01T03:00:00Z"), available_as_of: None,
    }, &|| Err(contextful_context::ContextError::Catalog(contextful_core::run::Failure::new(contextful_core::run::FailureTag::Canceled, "execution stopped"))));
    assert!(result.unwrap_err().to_string().contains("execution stopped"));
    let section = current_section(&p.fx.store, "daily").unwrap().unwrap();
    assert_eq!(serde_json::to_value(p.fx.store.try_schema("daily").unwrap()).unwrap(), old_schema);
    assert_eq!(section.build_id, old.build_id);
    assert_eq!(section.watermark, old.watermark);
    assert_eq!(p.rows("SELECT day,n FROM daily ORDER BY day"), rows);
}

const GOVERNED: &str = r#"
[[model]]
id = "daily"
sql = "SELECT day, CAST(v AS VARCHAR) AS who, CAST(count(*) AS BIGINT) AS n FROM events GROUP BY day, v"
unique_key = ["day", "who"]

[model.contract]
version = "1.0.0"
columns = [{ name = "day", type = "utf8", nullable = false }, { name = "who", type = "utf8", nullable = false }, { name = "n", type = "int64", nullable = false }]

[model.disclosure]
grouping_allowlist = ["day"]
contributor_key = "who"
min_group_size = 2
"#;

/// Build `manifest` through a release step that publishes the statement's rows less the contributor key `who`.
fn build_governed(p: &Project, manifest: &str, now: &str) -> Result<Built, ReadFault> {
    use contextful_core::disclosure::release::{Breakdown, DerivedTable};
    let spec = model_over(TABLES, manifest);
    let face = p.face(manifest);
    let step = |_: &str, b: Breakdown| {
        let columns = b.columns.into_iter().filter(|c| c != "who").collect();
        let rows = b
            .rows
            .into_iter()
            .map(|mut r| {
                r.remove("who");
                r
            })
            .collect();
        Ok(DerivedTable { name: "daily".into(), columns, rows })
    };
    let req = BuildRequest { model: &spec, site_id: "site-a", started_at: at(now), completed_at: at(now), available_as_of: None };
    contextful_context::build::build_released(&face, &req, &step)
}

/// A released build hands the release step the statement's per-contributor rows under the
/// build's id, and publishes the rows the step returns, without the contributor key.
#[test]
fn a_released_build_publishes_the_release_steps_rows_without_the_contributor_key() {
    use contextful_core::disclosure::release::DerivedTable;
    let p = Project::new();
    let spec = model_over(TABLES, GOVERNED);
    let face = p.face(GOVERNED);
    let seen = std::cell::RefCell::new(None);
    let step = |release_id: &str, breakdown: contextful_core::disclosure::release::Breakdown| {
        assert_eq!(breakdown.columns, ["day", "who", "n"]);
        assert_eq!(breakdown.grain, ["day", "who"]);
        assert_eq!(breakdown.rows.len(), 3);
        *seen.borrow_mut() = Some(release_id.to_string());
        Ok(DerivedTable {
            name: "daily".into(),
            columns: vec!["day".into(), "n".into()],
            rows: vec![json!({"day": "d1", "n": 2}).as_object().unwrap().clone(), json!({"day": "__suppressed__", "n": null}).as_object().unwrap().clone()],
        })
    };
    let req = BuildRequest { model: &spec, site_id: "site-a", started_at: at("2030-01-01T01:00:00Z"), completed_at: at("2030-01-01T01:00:00Z"), available_as_of: None };
    let built = contextful_context::build::build_released(&face, &req, &step).unwrap();
    assert_eq!(seen.borrow().as_deref(), Some(built.build_id.as_str()));
    assert_eq!(built.rows, 2);
    assert_eq!(p.rows("SELECT day, n FROM daily ORDER BY day"), [[json!("__suppressed__"), Value::Null], [json!("d1"), json!("2")]]);
    let columns: Vec<String> = p.fx.store.try_schema("daily").unwrap().unwrap().columns.iter().map(|c| c.name.clone()).collect();
    assert_eq!(columns.iter().filter(|c| !c.starts_with('_')).collect::<Vec<_>>(), ["day", "n"], "the contributor key is not published");
}

/// A plain build of a model declaring `[model.disclosure]` refuses before staging and
/// publishes nothing; the refusal names the release verb.
#[test]
fn a_plain_build_of_a_governed_model_raises_release_required() {
    let p = Project::new();
    let e = refused(p.build(GOVERNED, "2030-01-01T01:00:00Z"), "DisclosureReleaseRequired");
    assert!(e.contains("contextful disclosure release daily --project research"), "{e}");
    assert!(current_section(&p.fx.store, "daily").unwrap().is_none(), "nothing was published");
    assert!(!p.dir().join(POINTER_FILE).exists());
    assert!(!p.dir().join("build-attempts").exists(), "no build attempt was recorded");
}

/// A refused release step publishes nothing and names its refusal.
#[test]
fn a_refused_release_step_publishes_nothing() {
    use contextful_core::disclosure::release::ReleaseError;
    use contextful_core::disclosure::DisclosureError;
    let p = Project::new();
    let spec = model_over(TABLES, GOVERNED);
    let face = p.face(GOVERNED);
    let step = |_: &str, _: contextful_core::disclosure::release::Breakdown| Err(ReleaseError::Refused(DisclosureError::UnitBudgetExhausted("3 of 3 units".into())));
    let req = BuildRequest { model: &spec, site_id: "site-a", started_at: at("2030-01-01T01:00:00Z"), completed_at: at("2030-01-01T01:00:00Z"), available_as_of: None };
    refused(contextful_context::build::build_released(&face, &req, &step), "DisclosureUnitBudgetExhausted");
    assert!(current_section(&p.fx.store, "daily").unwrap().is_none(), "nothing was published");
    assert!(!p.dir().join(POINTER_FILE).exists());
}

/// The seed the run-record growth fixtures derive their run ids and instants from.
const RUNS_SEED: u64 = 0x5eed_0615;

const RUN_COUNTS: &str = r#"
[[model]]
id = "run_counts"
sql = "SELECT status, CAST(count(*) AS BIGINT) AS n FROM \"_runs\" GROUP BY status"
unique_key = ["status"]

[model.contract]
version = "1.0.0"
columns = [{ name = "status", type = "utf8", nullable = false }, { name = "n", type = "int64", nullable = false }]

[model.freshness]
max_lag = "1d"
"#;

/// Append `units` unit runs to `_runs` on `ingest-a`: per unit a plan row and a commit
/// row, one store run each, 60 s apart from the seed's base instant. Returns the last
/// commit instant.
fn append_unit_runs(p: &Project, units: u64) -> contextful_core::time::Instant {
    use contextful_core::run::record::RunRow;
    let node = contextful_core::store::lay_out::NodeId::parse("ingest-a").unwrap();
    let base = at("2030-01-01T00:00:00Z").plus_secs(RUNS_SEED % 60);
    let mut last = base;
    for i in 0..units {
        let started = base.plus_secs(i * 60);
        for (phase, state, offset) in [("plan", "running", 0), ("commit", "success", 1)] {
            let row: RunRow = serde_json::from_value(json!({
                "run_id": format!("u{RUNS_SEED:x}-{i:06}"), "pipeline_id": "feed", "table": "events", "site_id": "site-a",
                "status": state, "started_at": started.to_rfc3339(), "rows": 0, "bytes": 0, "batches": 0,
                "connector_id": "vendor", "connector_version": "1", "connector_hash": "h",
                "phase": phase, "execution_id": format!("x-{i:06}"),
            }))
            .unwrap();
            last = started.plus_secs(offset);
            contextful_context::run_record::append(&p.fx.store, &node, &row, last).unwrap();
        }
    }
    last
}

/// A model reading `_runs` records a frontier of fixed size whatever the run record holds:
/// no run id, at most 512 bytes, and the count of the store runs the snapshot omits. Under
/// `contextful-ci measure` the fixture holds 524 unit runs, 1048 store runs, 48 past the
/// last multiple of the 50-run fold trigger; elsewhere 24 unit runs exercise the path. The
/// ledger records the watermark's serialized bytes and run ids, and the bytes of the source
/// completion a grouped publication records from it.
#[test]
fn a_watermark_over_the_run_record_stays_bounded_after_n_runs() {
    let measuring = std::env::var_os(contextful_eval::record::MEASURE_DIR_VAR).is_some();
    let units: u64 = if measuring { 524 } else { 24 };
    let p = Project::new();
    let last = append_unit_runs(&p, units);
    let built = p.build(RUN_COUNTS, &last.plus_secs(60).to_rfc3339()).unwrap();
    let watermark = serde_json::to_value(&built.watermark).unwrap();
    let bytes = serde_json::to_vec(&watermark).unwrap().len();
    // Read through JSON so the figure stays measurable whatever shape the frontier takes.
    let ids = watermark["inputs"]["_runs"]["runs"].as_array().map_or(0, Vec::len);
    let completion = contextful_core::export::publication::SourcePublication {
        run: built.build_id.clone(),
        input: watermark.clone(),
        frontiers: [("run_counts".to_string(), built.build_id.clone())].into(),
        commit_seq: 0,
    };
    let completion_bytes = serde_json::to_vec(&completion).unwrap().len();
    eprintln!("watermark over {units} unit runs: {ids} ids, {bytes} B; source completion {completion_bytes} B");
    assert!(watermark["inputs"].get("_runs").is_some(), "{watermark}");
    assert_eq!(ids, 0, "{watermark}");
    assert!(bytes <= 512, "{bytes} B over {units} unit runs");
    let unfolded = p.fx.store.state(&contextful_context::run_record::decl()).unwrap().unfolded_runs().len();
    assert_eq!(watermark["inputs"]["_runs"]["omitted"]["count"], json!(unfolded), "{watermark}");
    assert!(completion_bytes > bytes);
    contextful_eval::record::emit("watermark-bytes-runs-record", bytes as f64, units, RUNS_SEED);
    contextful_eval::record::emit("watermark-ids-runs-record", ids as f64, units, RUNS_SEED);
    contextful_eval::record::emit("source-completion-bytes", completion_bytes as f64, units, RUNS_SEED);
}

/// `build status` over a model whose `_runs` input holds every run-record store run, the
/// record folding as its trigger fires (`run.record.record-folds`): p95 of 200 timed calls after 20 warm-up, the median of 5 batches
/// (`assurance.measure.timing-iterations`, `assurance.measure.timing-batches`), in
/// microseconds. Under `contextful-ci measure` the fixture holds 1000 unit runs, 2000 store
/// runs; elsewhere 24 unit runs and 5 calls after 1 exercise the path.
#[test]
fn status_over_an_unfolded_run_record_at_two_thousand_runs() {
    use contextful_eval::trend::median_batch;
    use std::time::Instant as Clock;
    let measuring = std::env::var_os(contextful_eval::record::MEASURE_DIR_VAR).is_some();
    let (units, warm_up, repeats, batches): (u64, usize, usize, usize) = if measuring { (1000, 20, 200, 5) } else { (24, 1, 5, 1) };
    let p = Project::new();
    let last = append_unit_runs(&p, units);
    let built = p.build(RUN_COUNTS, &last.plus_secs(60).to_rfc3339()).unwrap();
    let now = last.plus_secs(120);
    let first = status(&p.fx.store, "run_counts", now).unwrap();
    assert_eq!(first["stale"], false, "{first}");
    assert_eq!(first["last_build_id"], json!(built.build_id), "{first}");
    let batch = || -> Vec<f64> {
        (0..warm_up + repeats)
            .filter_map(|i| {
                let started = Clock::now();
                std::hint::black_box(status(&p.fx.store, "run_counts", now).unwrap());
                let us = started.elapsed().as_secs_f64() * 1e6;
                (i >= warm_up).then_some(us)
            })
            .collect()
    };
    let timed: Vec<Vec<f64>> = (0..batches).map(|_| batch()).collect();
    let (p50, p95) = median_batch(&timed).unwrap();
    eprintln!("status over {} store runs: p50 {p50:.0} us, p95 {p95:.0} us", 2 * units);
    assert!(p95 > 0.0);
    contextful_eval::record::emit("status-latency-runs-record", p95, repeats as u64, RUNS_SEED);
}
