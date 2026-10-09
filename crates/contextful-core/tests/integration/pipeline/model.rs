//! `run.model` and `run.publish`: the model block, its contract identity, the manifest
//! section, freshness and the history logs derived from committed manifests.

use contextful_core::pipeline::declare::{collect, read_manifest, ManifestFile};
use contextful_core::pipeline::model::{
    build_entries, collect_models, contract_history, duration_secs, regenerate, BuildStatus, HoldRecord, InputFrontier, Materialized,
    ModelSpec, PublishSection, Watermark, FINGERPRINT_RECIPE, SEMANTICS_VERSION,
};
use contextful_core::run::RunError;
use contextful_core::store::lay_out::{SnapshotId, SnapshotManifest};
use contextful_core::time::Instant;
use std::collections::BTreeMap;

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}

fn manifest(text: &str) -> ManifestFile {
    ManifestFile { path: "contextful.toml".into(), text: text.into() }
}

#[test]
fn a_registered_export_block_coexists_with_store_tables_and_refuses_a_misspelled_block() {
    let text = "[[pipeline.tables]]\nname = 'spans'\n[[export]]\nname = 'spans-mirror'\ntable = 'spans'\nendpoint = 'https://example.test/v1/logs'\nformat = 'changes-v1'\nkey = ['span_id']\n";
    assert!(read_manifest(&manifest(text)).unwrap().is_empty());
    let tables = contextful_core::store::declare::TableDecl::parse_pipeline(text).unwrap();
    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0].name, "spans");
    assert!(collect(&[manifest(text)]).unwrap().is_empty());
    let refusal = read_manifest(&manifest(&text.replace("[[export]]", "[[exprot]]"))).unwrap_err();
    assert!(matches!(refusal, RunError::PipelineUnknownBlock(_)), "{refusal}");
    assert!(refusal.to_string().contains("exprot"));
}

const CONTRACT: &str = "\n[model.contract]\nversion = \"1.0.0\"\ncolumns = [{ name = \"day\", type = \"utf8\", nullable = false }, { name = \"n\", type = \"int64\" }]\n";

fn model_doc(extra: &str) -> String {
    format!("[[model]]\nid = \"daily\"\nsql = \"SELECT day, count(*) AS n FROM events GROUP BY day\"\n{extra}{CONTRACT}")
}

fn models(text: &str) -> Result<Vec<ModelSpec>, RunError> {
    let files = [manifest(text)];
    let pipelines = collect(&files)?;
    Ok(collect_models(&files, &pipelines)?.into_iter().map(|m| m.spec).collect())
}

fn model(text: &str) -> ModelSpec {
    models(text).unwrap().remove(0)
}

#[test]
fn a_model_reads_its_disclosure_declaration() {
    let m = model(&model_doc("[model.disclosure]\ngrouping_allowlist = [\"industry\"]\ncontributor_key = \"tenant_id\"\nmin_group_size = 3\n[model.disclosure.metric_bounds]\nrevenue = { lower = 0, upper = 100, quantum = 1 }\n"));
    let policy = m.disclosure.unwrap();
    assert_eq!(policy.grouping_allowlist, ["industry"]);
    assert_eq!(policy.min_group_size, Some(3));
    assert_eq!(policy.metric_bounds["revenue"].quantum, 1.0);
}

#[test]
fn a_model_declares_a_local_statement_file_or_inline_sql() {
    let m = model("[[model]]\nid = \"daily\"\nsql_file = \"daily.sql\"\ndisclosure_opt_out = \"internal rollup\"\n[model.contract]\nversion = \"1.0.0\"\ncolumns = [{ name = \"day\", type = \"utf8\" }, { name = \"n\", type = \"int64\" }]\n");
    assert_eq!(m.sql_file.as_deref(), Some("daily.sql"));
    assert_eq!(m.disclosure_opt_out.as_deref(), Some("internal rollup"));
    assert!(m.validate().is_ok());
    assert!(models(&model_doc("sql_file = \"daily.sql\"\n")).unwrap().remove(0).validate().is_err());
    assert!(models(&model_doc("sql_file = \"  \"\n")).unwrap().remove(0).validate().is_err());
}

#[test]
fn a_model_refuses_an_absolute_statement_file() {
    let m = model("[[model]]\nid = \"daily\"\nsql_file = \"/tmp/daily.sql\"\n");
    assert!(m.validate_statement_source().is_err());
}

// spec: disclosure.set-mode.opt-out-record@ccf54f2d
#[test]
fn a_disclosure_opt_out_requires_a_reason_and_no_policy() {
    assert!(model(&model_doc("disclosure_opt_out = \"internal rollup\"\n")).validate().is_ok());
    assert!(model(&model_doc("disclosure_opt_out = \"  \"\n")).validate().is_err());
    assert!(model(&model_doc("disclosure_opt_out = \"internal rollup\"\n[model.disclosure]\ngrouping_allowlist = [\"day\"]\ncontributor_key = \"tenant\"\nmin_group_size = 3\n")).validate().is_err());
}

fn section(build: &str, version: &str, fingerprint: &str, built: &str) -> PublishSection {
    PublishSection {
        contract_version: version.into(),
        schema_fingerprint: fingerprint.into(),
        build_id: build.into(),
        build_started_at: at(built),
        last_built_at: at(built),
        watermark: Watermark::default(),
        max_lag: None,
        last_build_status: BuildStatus::Published,
        withheld_cells: false,
        disclosure_digest: "d".into(),
        partitions_failed: None,
        semantics_version: Some(SEMANTICS_VERSION),
        fingerprint_recipe: Some(FINGERPRINT_RECIPE.into()),
    }
}

fn snapshot(created: &str, parent: Option<&SnapshotId>, publish: Option<PublishSection>) -> SnapshotManifest {
    let id = SnapshotId::next(at(created), parent);
    SnapshotManifest {
        snapshot_id: id.clone(),
        parent: parent.cloned(),
        ancestors: None,
        table: "daily".into(),
        created_at: at(created),
        includes_runs: vec![],
        primary_key: vec![],
        order_by: None,
        row_count: 0,
        valid_time: None,
        parts: vec![],
        indexes: vec![],
        fence: None,
        commit_seq: None,
        publish: publish.map(|mut s| {
            s.build_id = id.to_string();
            s
        }),
    }
}

/// A model block carries its required identity and declared source, with no unknown key.
// spec: run.model.model-block@aeb44362
#[test]
fn a_model_block_carries_its_keys_and_refuses_an_unknown_one() {
    let m = model(&model_doc(
        "materialized = \"table\"\nunique_key = [\"day\"]\npublish = true\n[model.freshness]\nmax_lag = \"1d\"\n[[model.test]]\nname = \"positive\"\nsql = \"SELECT * FROM daily WHERE n <= 0\"\n",
    ));
    assert_eq!(m.id, "daily");
    assert_eq!(m.grain(), ["day"]);
    assert!(m.publishes());
    assert_eq!(m.max_lag(), Some("1d"));
    assert_eq!(m.tests[0].name, "positive");
    assert_eq!(m.contract.as_ref().unwrap().columns.len(), 2);

    let e = models(&model_doc("owner = \"ops\"\n")).unwrap_err();
    assert!(matches!(&e, RunError::PipelineSpecInvalid(m) if m.contains("model[0]") && m.contains("owner")), "{e}");
    let e = models(&format!("{}\n[[model.test]]\nname = \"t\"\nsql = \"SELECT 1\"\nseverity = \"warn\"\n", model_doc(""))).unwrap_err();
    assert!(matches!(&e, RunError::PipelineSpecInvalid(m) if m.contains("severity")), "{e}");
}

/// A manifest's top-level key outside the set the engine enumerates raises `PipelineUnknownBlock`, naming the key, the file and the accepted set.
// spec: run.model.top-level-block@0e09c709
#[test]
fn a_top_level_key_outside_the_manifest_blocks_is_refused() {
    let e = read_manifest(&manifest("[project]\nname = \"research\"\n\n[[models]]\nid = \"daily\"\nsql = \"SELECT 1\"\n")).unwrap_err();
    match &e {
        RunError::PipelineUnknownBlock(m) => {
            assert!(m.contains("`models`") && m.contains("contextful.toml") && m.contains("model, pack"), "{m}");
        }
        other => panic!("expected PipelineUnknownBlock, got {other}"),
    }
    assert!(e.to_string().starts_with("PipelineUnknownBlock:"));
    // Every block a manifest declares reads, and a single-specification file keeps its own keys.
    read_manifest(&manifest(&format!("[project]\nname = \"research\"\nsite_id = \"a\"\n{}", model_doc("")))).unwrap();
    read_manifest(&ManifestFile {
        path: "pipelines/orders.toml".into(),
        text: "id = \"orders\"\ntables = [\"items\"]\n[source]\nname = \"http\"\n".into(),
    })
    .unwrap();
}

/// A `[node]` block refuses as a node id every machine reconciling the manifest shares, ahead of the unknown-block
/// refusal, naming the file and the node id's home.
#[test]
fn a_node_block_in_a_manifest_refuses_as_a_shared_node_id() {
    let e = read_manifest(&ManifestFile { path: "manifest@v3.toml".into(), text: "[node]\nid = \"ingest-a\"\n".into() }).unwrap_err();
    match &e {
        RunError::Store(contextful_core::store::StoreError::StoreNodeIdShared(m)) => {
            assert!(m.contains("manifest@v3.toml") && m.contains("[node]") && m.contains("config.toml"), "{m}");
        }
        other => panic!("expected StoreNodeIdShared, got {other}"),
    }
}

/// A model's `id` names the store table it builds; an id declared twice, or equal to a pipeline destination table, refuses as {{run.declare.table-name-collision}}.
#[test]
fn a_model_id_is_its_table_and_collides_with_no_other() {
    let twice = format!("{}\n{}", model_doc(""), model_doc(""));
    assert!(matches!(models(&twice), Err(RunError::PipelineTableNameCollision(m)) if m.contains("daily")));
    let pipeline = "[[pipeline]]\nid = \"orders\"\n[pipeline.source]\nname = \"http\"\n[[pipeline.tables]]\nname = \"items\"\n";
    let clash = format!("{pipeline}\n{}", model_doc("").replace("id = \"daily\"", "id = \"orders_items\""));
    let e = models(&clash).unwrap_err();
    assert!(matches!(&e, RunError::PipelineTableNameCollision(m) if m.contains("orders_items") && m.contains("pipeline `orders`")), "{e}");
    let reserved = model_doc("").replace("id = \"daily\"", "id = \"daily__requests\"");
    assert!(model(&reserved).validate().is_err());
}

/// `materialized` is `table`, the default: each build replaces the model's rows whole.
// spec: run.model.materialized@308d27e4
#[test]
fn a_model_materializes_as_a_table_and_nothing_else() {
    assert_eq!(model(&model_doc("")).materialized, None);
    assert_eq!(model(&model_doc("materialized = \"table\"\n")).materialized, Some(Materialized::Table));
    let e = models(&model_doc("materialized = \"incremental\"\n")).unwrap_err();
    assert!(matches!(&e, RunError::PipelineSpecInvalid(m) if m.contains("materialized")), "{e}");
}

/// `[model.contract]` declares `version`, a `<major>.<minor>.<patch>` string, and `columns`, each a `name`, a `type` spelled as {{store.reconcile.typed-landing}} reads it, and `nullable`, default true.
// spec: run.model.contract-block@2515cc33
#[test]
fn a_contract_declares_a_semantic_version_and_typed_columns() {
    let m = model(&model_doc(""));
    let c = m.contract.as_ref().unwrap();
    assert_eq!(c.version.as_str(), "1.0.0");
    assert_eq!(c.version.major(), 1);
    assert!(!c.columns[0].nullable());
    assert!(c.columns[1].nullable());
    // A spelling a landing reads parses to its canonical form.
    let m = model(&model_doc("").replace("type = \"int64\"", "type = \"integer\""));
    assert_eq!(m.contract.unwrap().columns[1].ty, "int64");
    for bad in ["1.0", "v1.0.0", "1.0.0-rc1", "01.0.0"] {
        let e = models(&model_doc("").replace("\"1.0.0\"", &format!("\"{bad}\""))).unwrap_err();
        assert!(matches!(&e, RunError::PipelineSpecInvalid(m) if m.contains("version")), "{bad}: {e}");
    }
    let e = models(&model_doc("").replace("\"int64\"", "\"decimal\"")).unwrap_err();
    assert!(matches!(&e, RunError::PipelineSpecInvalid(m) if m.contains("decimal")), "{e}");
}

/// A model whose `publish` is true, the default, and which declares no `[model.contract]` raises `ModelContractUndeclared` at validation.
// spec: run.model.contract-required@94bc1475
#[test]
fn a_published_model_without_a_contract_is_refused() {
    let bare = "[[model]]\nid = \"daily\"\nsql = \"SELECT 1 AS one\"\n";
    let e = model(bare).validate().unwrap_err();
    assert!(matches!(&e, RunError::ModelContractUndeclared(m) if m.contains("daily")), "{e}");
    assert!(e.to_string().starts_with("ModelContractUndeclared:"));
    model(&format!("{bare}publish = false\n")).validate().unwrap();
    model(&model_doc("")).validate().unwrap();
}

/// `[model.freshness]` declares `max_lag` as an integer followed by `s`, `m`, `h` or `d`; a model declaring none publishes a null `max_lag` and never computes stale.
// spec: run.model.freshness-block@ad9919bb
#[test]
fn freshness_declares_a_max_lag_in_four_units() {
    assert_eq!(duration_secs("90s"), Some(90));
    assert_eq!(duration_secs("15m"), Some(900));
    assert_eq!(duration_secs("2h"), Some(7_200));
    assert_eq!(duration_secs("7d"), Some(604_800));
    for bad in ["0d", "d", "1w", "1.5h", "-1h", "1 h"] {
        assert_eq!(duration_secs(bad), None, "{bad}");
    }
    let e = models(&model_doc("[model.freshness]\nmax_lag = \"1w\"\n")).unwrap_err();
    assert!(matches!(&e, RunError::PipelineSpecInvalid(m) if m.contains("max_lag")), "{e}");
    let undeclared = section("b", "1.0.0", "f", "2030-01-01T00:00:00Z").freshness();
    assert_eq!(undeclared.max_lag, None);
    assert!(!undeclared.stale(at("2099-01-01T00:00:00Z")));
}

/// A published table's identity is `contract_version` beside `schema_fingerprint`, taken over the declared columns, their types and the grain.
// spec: run.publish.contract-identity@add7c87f
#[test]
fn the_schema_fingerprint_moves_with_columns_types_and_grain_alone() {
    let base = model(&model_doc("unique_key = [\"day\"]\n"));
    let fp = base.schema_fingerprint().unwrap();
    assert_eq!(fp.len(), 64);
    // The version, the SQL, the tests and the freshness are outside the identity.
    let same = model(
        &model_doc("unique_key = [\"day\"]\n[model.freshness]\nmax_lag = \"1h\"\n")
            .replace("\"1.0.0\"", "\"1.4.2\"")
            .replace("count(*)", "sum(1)"),
    );
    assert_eq!(same.schema_fingerprint().unwrap(), fp);
    assert_eq!(same.contract.unwrap().version.as_str(), "1.4.2");
    // A spelling alias hashes as its canonical type.
    assert_eq!(model(&model_doc("unique_key = [\"day\"]\n").replace("\"int64\"", "\"integer\"")).schema_fingerprint().unwrap(), fp);
    for moved in [
        model_doc("unique_key = [\"day\"]\n").replace("\"int64\"", "\"float64\""),
        model_doc("unique_key = [\"day\"]\n").replace("name = \"n\"", "name = \"count\""),
        model_doc("unique_key = [\"day\", \"n\"]\n"),
        model_doc(""),
    ] {
        assert_ne!(model(&moved).schema_fingerprint().unwrap(), fp, "{moved}");
    }
    assert_eq!(model("[[model]]\nid = \"d\"\nsql = \"SELECT 1\"\npublish = false\n").schema_fingerprint(), None);
}

/// Freshness carries the newest publishing build id, its watermark, `max_lag`, the last build status and a withheld-cells flag; staleness is derived from watermark against `max_lag` and never stored.
// spec: run.publish.freshness@1cf4dc36
#[test]
fn staleness_is_derived_from_the_watermark_and_max_lag() {
    let mut s = section("snapshot-1", "1.0.0", "f", "2030-01-01T06:00:00Z");
    s.max_lag = Some("1h".into());
    s.watermark = Watermark {
        at: Some(at("2030-01-01T05:00:00Z")),
        inputs: BTreeMap::from([("events".into(), InputFrontier { snapshot_id: None, runs: vec!["r1/ingest-a".into()] })]),
    };
    let f = s.freshness();
    assert_eq!(f.build_id, "snapshot-1");
    assert_eq!(f.watermark, s.watermark);
    assert_eq!(f.last_build_status, BuildStatus::Published);
    assert!(!f.withheld_cells);
    assert!(!f.stale(at("2030-01-01T06:00:00Z")));
    assert!(f.stale(at("2030-01-01T06:00:01Z")));
    // No field of the section records staleness.
    let v = serde_json::to_value(&s).unwrap();
    assert!(!v.as_object().unwrap().is_empty(), "the exclusion below ranges over no element");
    assert!(v.as_object().unwrap().keys().all(|k| !k.contains("stale")), "{v}");
    // Inputs with no commit leave nothing to measure the lag from.
    s.watermark.at = None;
    assert!(s.freshness().stale(at("2030-01-01T05:00:00Z")));
}

/// The manifest section carries `{contract_version, schema_fingerprint, build_id, build_started_at, last_built_at, watermark, max_lag, last_build_status, withheld_cells, disclosure_digest, partitions_failed?, semantics_version?, fingerprint_recipe?}` of the newest publishing build; `watermark` maps each input table to its snapshot and omitted runs.
// spec: run.publish.manifest-section@d25cb29d
#[test]
fn the_manifest_section_carries_its_keys_and_omits_an_absent_optional_one() {
    let mut s = section("snapshot-1", "1.0.0", "f", "2030-01-01T00:00:00Z");
    let keys = |s: &PublishSection| {
        let v = serde_json::to_value(s).unwrap();
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    assert_eq!(
        keys(&s),
        [
            "build_id",
            "build_started_at",
            "contract_version",
            "disclosure_digest",
            "fingerprint_recipe",
            "last_build_status",
            "last_built_at",
            "max_lag",
            "schema_fingerprint",
            "semantics_version",
            "watermark",
            "withheld_cells"
        ]
    );
    // A required key holding no value is present as null; an absent optional key is omitted.
    assert!(serde_json::to_value(&s).unwrap()["max_lag"].is_null());
    s.partitions_failed = Some(vec!["day=2030-01-01".into()]);
    s.semantics_version = None;
    s.fingerprint_recipe = None;
    let k = keys(&s);
    assert!(k.contains(&"partitions_failed".to_string()) && !k.contains(&"semantics_version".to_string()));
    // The section rides the snapshot manifest and round-trips through it.
    let m = snapshot("2030-01-01T00:00:00Z", None, Some(s.clone()));
    let back: SnapshotManifest = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
    assert_eq!(back.publish, m.publish);
    let bare = snapshot("2030-01-01T00:00:00Z", None, None);
    assert!(!serde_json::to_string(&bare).unwrap().contains("publish"));
}

/// `semantics_version` advances when the engine adds an injected column, and `fingerprint_recipe` names the fingerprint's inputs, that column included.
// spec: run.publish.semantics-version@a952eb95
/// Adding an injected column advances the semantics version, whose fingerprint recipe names the column.
// spec: store.reconcile.reserved-set-versioned@3ef4472c
#[test]
fn the_recipe_names_every_injected_column_the_semantics_version_counts() {
    assert_eq!(SEMANTICS_VERSION, 2);
    for injected in contextful_core::store::reserve::ALWAYS_INJECTED {
        assert!(FINGERPRINT_RECIPE.contains(injected), "{injected}");
    }
    for input in ["name", "type", "nullable", "grain"] {
        assert!(FINGERPRINT_RECIPE.contains(input), "{input}");
    }
}

/// A build entry carries build id, start and completion instants, a status of published, refused, partial or failed, the contract identity, and the partition values it left unfilled.
// spec: run.publish.build-entry@a96fa84e
#[test]
fn a_build_entry_is_read_off_each_committed_section() {
    let mut first = section("", "1.0.0", "fa", "2030-01-01T00:00:00Z");
    first.build_started_at = at("2030-01-01T00:00:00Z");
    first.last_built_at = at("2030-01-01T00:00:09Z");
    let a = snapshot("2030-01-01T00:00:00Z", None, Some(first));
    let mut second = section("", "1.0.0", "fa", "2030-01-02T00:00:00Z");
    second.last_build_status = BuildStatus::Partial;
    second.partitions_failed = Some(vec!["day=2030-01-02".into()]);
    let b = snapshot("2030-01-02T00:00:00Z", Some(&a.snapshot_id), Some(second));
    let unpublished = snapshot("2030-01-03T00:00:00Z", Some(&b.snapshot_id), None);
    let entries = build_entries(&[unpublished, b.clone(), a.clone()]);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].build_id, a.snapshot_id.to_string());
    assert_eq!((entries[0].started_at, entries[0].completed_at), (at("2030-01-01T00:00:00Z"), at("2030-01-01T00:00:09Z")));
    assert_eq!(entries[0].status, BuildStatus::Published);
    assert_eq!((entries[0].contract_version.as_str(), entries[0].schema_fingerprint.as_str()), ("1.0.0", "fa"));
    assert!(entries[0].partitions_unfilled.is_empty());
    assert_eq!(entries[1].status, BuildStatus::Partial);
    assert_eq!(entries[1].partitions_unfilled, ["day=2030-01-02"]);
    let v = serde_json::to_value(&entries[1]).unwrap();
    assert_eq!(v["status"], "partial");
    assert_eq!(serde_json::to_value(BuildStatus::Refused).unwrap(), "refused");
    assert_eq!(serde_json::to_value(BuildStatus::Failed).unwrap(), "failed");
}

/// `contract-history.jsonl`, `builds.jsonl` and `holds.jsonl` are append-only history derived from committed manifests and build-attempt records; a log disagreeing with a source record is regenerated from it.
// spec: run.publish.history-logs@11c001f0
#[test]
fn a_log_is_regenerated_from_committed_manifests_and_keeps_collected_history() {
    let a = snapshot("2030-01-01T00:00:00Z", None, Some(section("", "1.0.0", "fa", "2030-01-01T00:00:00Z")));
    let b = snapshot("2030-01-02T00:00:00Z", Some(&a.snapshot_id), Some(section("", "1.0.0", "fa", "2030-01-02T00:00:00Z")));
    let c = snapshot("2030-01-03T00:00:00Z", Some(&b.snapshot_id), Some(section("", "2.0.0", "fb", "2030-01-03T00:00:00Z")));
    let history = contract_history(&[a.clone(), b.clone(), c.clone()]);
    assert_eq!(history.iter().map(|h| h.contract_version.as_str()).collect::<Vec<_>>(), ["1.0.0", "2.0.0"]);
    assert_eq!(history[1].build_id, c.snapshot_id.to_string());

    // `a` is collected: its entry stays as logged, and a tampered entry for `b` is rewritten.
    let derived = build_entries(&[b.clone(), c.clone()]);
    let mut logged = build_entries(&[a.clone(), b.clone()]);
    logged[1].status = BuildStatus::Refused;
    let out = regenerate(&logged, &derived, |e| e.build_id.clone(), |e| e.completed_at);
    assert_eq!(out.iter().map(|e| e.build_id.clone()).collect::<Vec<_>>(), [a, b, c].map(|m| m.snapshot_id.to_string()));
    assert!(out.iter().all(|e| e.status == BuildStatus::Published));
    // With its parent collected, a build left out of the log adds no history entry.
    assert!(contract_history(&[snapshot("2030-01-05T00:00:00Z", Some(&SnapshotId::next(at("2030-01-04T00:00:00Z"), None)), Some(section("", "3.0.0", "fc", "2030-01-05T00:00:00Z")))]).is_empty());

    let hold = |placed: &str| HoldRecord {
        build_id: "snapshot-1".into(),
        principal: "ops".into(),
        placed_at: at(placed),
        expires_at: at("2030-02-01T00:00:00Z"),
    };
    let key = |h: &HoldRecord| format!("{}@{}", h.build_id, h.placed_at);
    let out = regenerate(&[hold("2030-01-01T00:00:00Z")], &[hold("2030-01-02T00:00:00Z")], key, |h| h.placed_at);
    assert_eq!(out.len(), 2, "a renewal appends; the earlier placement stays in the history");
}

/// A hold records build id, placing principal and expiry; collection skips a held build, and a hold confers no other authority.
#[test]
fn a_hold_is_active_until_its_expiry() {
    let h = HoldRecord { build_id: "b".into(), principal: "ops".into(), placed_at: at("2030-01-01T00:00:00Z"), expires_at: at("2030-01-08T00:00:00Z") };
    assert!(h.active(at("2030-01-07T23:59:59Z")));
    assert!(!h.active(at("2030-01-08T00:00:00Z")));
}

/// The disclosure digest reads the table's `class`, `policy` and `visibility` alone, with a
/// set-valued field's order ignored and a list of objects kept in order.
#[test]
fn the_disclosure_digest_sorts_set_valued_fields() {
    use contextful_core::pipeline::model::{disclosure_digest, withholds_cells};
    use contextful_core::store::declare::TableDecl;
    use serde_json::json;
    let with = |policy: serde_json::Value| TableDecl { policy: Some(policy), ..TableDecl::named("t") };
    let open = TableDecl::named("t");
    assert_eq!(disclosure_digest(&open), disclosure_digest(&TableDecl { retain_runs: Some("30d".into()), ..TableDecl::named("u") }));
    assert!(!withholds_cells(&open));
    let ab = with(json!({"zone": {"allow": ["a", "b"]}}));
    let ba = with(json!({"zone": {"allow": ["b", "a"]}}));
    assert_eq!(disclosure_digest(&ab), disclosure_digest(&ba));
    assert_ne!(disclosure_digest(&ab), disclosure_digest(&open));
    assert!(withholds_cells(&ab));
    let first = with(json!({"rows": {"predicate": "p", "exception": [{"when": "x", "predicate": "1"}, {"when": "y", "predicate": "2"}]}}));
    let swapped = with(json!({"rows": {"predicate": "p", "exception": [{"when": "y", "predicate": "2"}, {"when": "x", "predicate": "1"}]}}));
    assert_ne!(disclosure_digest(&first), disclosure_digest(&swapped), "exceptions apply first-match, so their order counts");
    assert!(withholds_cells(&TableDecl { class: Some(json!("email")), ..TableDecl::named("t") }));
}
