//! `run.model` and `run.publish` over the store: a model build materialized into staging,
//! published through the pointer with its manifest section, held, and logged.
#![cfg(feature = "read")]

use crate::support::{at, decl, Fixture};
use contextful_context::build::{build, current_section, hold, holds, manifests, BuildRequest, Built};
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
        build(&face, &BuildRequest { model: &spec, site_id: "site-a", started_at: at(now), completed_at: at(now) })
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

/// A published table's contract identity, freshness and current build ride the snapshot manifest commit that publishes its data; no separate file is authoritative for any of them.
// spec: run.publish.manifest-commit@27d757e1
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
    assert_eq!(section["watermark"]["inputs"]["events"]["runs"][0].as_str().unwrap().split('/').next(), Some("r1"));
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

/// `unique_key` is the model's grain; a build whose rows repeat one grain value, or hold a null in a grain column, refuses as {{run.publish.contract-mismatch}}.
// spec: run.model.unique-key@1c96b64b
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
// spec: run.model.sql@4c7d6e3d
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

/// A build whose schema fingerprint differs from the last published build's under an unchanged major version refuses as {{run.publish.contract-mismatch}}, naming the version to bump.
// spec: run.model.contract-major@8c9debed
#[test]
fn a_fingerprint_moving_under_one_major_is_refused_until_the_major_moves() {
    let p = Project::new();
    p.build(MODEL, "2030-01-01T01:00:00Z").unwrap();
    let widened = MODEL.replace("CAST(count(*) AS BIGINT)", "CAST(count(*) AS DOUBLE)").replace("type = \"int64\"", "type = \"float64\"");
    let e = refused(p.build(&widened.replace("\"1.0.0\"", "\"1.1.0\""), "2030-01-01T02:00:00Z"), "PipelineContractMismatch");
    assert!(e.contains("under major version 1") && e.contains("2.0.0"), "{e}");
    p.build(&widened.replace("\"1.0.0\"", "\"2.0.0\""), "2030-01-01T03:00:00Z").unwrap();
    let history: Vec<ContractHistoryEntry> = p.log(CONTRACT_HISTORY_LOG);
    assert_eq!(history.iter().map(|h| h.contract_version.as_str()).collect::<Vec<_>>(), ["1.0.0", "2.0.0"]);
    // A minor bump over an unchanged fingerprint publishes; the version change is a new identity entry.
    p.build(&widened.replace("\"1.0.0\"", "\"2.1.0\""), "2030-01-01T04:00:00Z").unwrap();
    assert_eq!(p.log::<ContractHistoryEntry>(CONTRACT_HISTORY_LOG).len(), 3, "a version change is a new identity entry");
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
    assert_eq!(section.semantics_version, Some(2));
}

/// A build's watermark is `{at, inputs}`: per input table the snapshot id and the committed runs it omits, and `at` the newest commit instant among them.
// spec: run.model.watermark@bf26ede3
#[test]
fn the_watermark_names_each_input_frontier() {
    let p = Project::new();
    p.land("r2", json!([{"day": "d3", "v": 1}]), "2030-01-01T00:20:00Z");
    let built = p.build(MODEL, "2030-01-01T01:00:00Z").unwrap();
    assert_eq!(built.watermark.at, Some(at("2030-01-01T00:20:00Z")));
    let events = &built.watermark.inputs["events"];
    assert_eq!(events.snapshot_id, None);
    assert_eq!(events.runs.iter().map(|r| r.split('/').next().unwrap()).collect::<Vec<_>>(), ["r1", "r2"]);
    // A fold moves the runs into a snapshot the next build names.
    contextful_context::fold::fold(&p.fx.store, &decl("name = \"events\""), at("2030-01-01T02:00:00Z")).unwrap();
    let next = p.build(MODEL, "2030-01-01T03:00:00Z").unwrap();
    let events = &next.watermark.inputs["events"];
    assert!(events.snapshot_id.is_some() && events.runs.is_empty(), "{events:?}");
    assert_eq!(next.watermark.at, Some(at("2030-01-01T02:00:00Z")));
}

/// A hold records build id, placing principal and expiry; collection skips a held build, and a hold confers no other authority.
// spec: run.publish.hold@f4fa4cd4
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
// spec: run.model.hold-manifest@49450f19
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
// spec: run.model.log-regeneration@fda3ed75
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

/// A build records a digest over the `class`, `policy` and `visibility` its table declares, set-valued fields sorted, in the manifest and the build log, and sets `withheld_cells` when any is declared.
// spec: run.publish.disclosure-digest@fea4a83b
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

/// A build reading a table that declares `class`, `policy` or `visibility` raises `ModelInputRestricted`, naming the table and the declared keys.
// spec: run.model.restricted-input@25682f0f
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
