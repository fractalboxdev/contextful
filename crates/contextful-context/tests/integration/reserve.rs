//! `store.reserve`: the injected columns as a reader sees them, and batch validation.

use crate::support::{at, decl, query, s, Fixture};
use contextful_context::fold::fold;
use contextful_context::land::{land, Batch, RunContext};
use contextful_core::connector::infer::Provenance;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reserve::Injection;
use contextful_core::store::StoreError;
use serde_json::{json, Value};
use std::collections::HashMap;

/// The engine injects `_ingested_at` as a non-null Parquet `TIMESTAMP(UTC, NANOS)`, `_run_id`, `_batch_seq` as int32 where a batch scope exists, `_site_id`, and `_authored_by` where an authenticated subject authorized the write, replacing any producer value.
// spec: store.reserve.injected@a7ade4f8
#[test]
fn the_engine_injects_provenance_and_replaces_producer_values() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    f.land(
        &d,
        "run-1",
        json!([{"id": "a", "_run_id": "forged", "_ingested_at": "1999-01-01T00:00:00Z", "_site_id": "elsewhere"}]),
        "2030-01-01T00:00:00.123456789Z",
    )
    .unwrap();
    let file = f.store.root().join(&f.scan(&d, Bounds::default()).unwrap().files[0]);
    let described = query(&format!(
        "SELECT name, type, repetition_type, logical_type FROM parquet_schema('{}') WHERE name LIKE '\\_%' ESCAPE '\\' ORDER BY name",
        file.display()
    ));
    let ingested = described.iter().find(|r| r[0] == s("_ingested_at")).unwrap();
    assert_eq!(ingested[1], s("INT64"));
    assert_eq!(ingested[2], s("REQUIRED"));
    let logical = ingested[3].clone().unwrap();
    assert!(logical.contains("NANOS") && logical.contains("isAdjustedToUTC=1"), "{logical}");
    let seq = described.iter().find(|r| r[0] == s("_batch_seq")).unwrap();
    assert_eq!(seq[1], s("INT32"));

    let row = f.query(&d, Bounds::default(), "SELECT _run_id, _site_id, _batch_seq, epoch_ns(_ingested_at) FROM t");
    assert_eq!(row, [[s("run-1"), s("site-a"), s("0"), s("1893456000123456000")]]);
}

/// The engine injects `_taint`, a label under {{connector.infer.provenance-order}}, on each row a model's output lands as, replacing any producer value; a row no model produced omits it.
// spec: store.reserve.taint@bdb810ae
#[test]
fn a_model_output_row_carries_the_engine_taint_and_no_other_row_does() {
    let f = Fixture::new();
    let d = decl("name = \"claims\"");
    let context = |run: &str, taint: Option<Provenance>| RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: run.into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, taint },
        committed_at: at("2030-01-01T00:00:00Z"),
    };
    let batch = |rows: Value| Batch { rows: rows.as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect(), types: HashMap::new() };
    land(&f.store, &d, &batch(json!([{"id": "a", "_taint": "operator"}])), &context("run-1", Some(Provenance::ThirdParty))).unwrap();
    land(&f.store, &d, &batch(json!([{"id": "b", "_taint": "operator"}])), &context("run-2", None)).unwrap();
    assert_eq!(
        f.query(&d, Bounds::default(), "SELECT id, _taint FROM t ORDER BY id"),
        [[s("a"), s("ingested:third-party")], [s("b"), None]],
        "the engine's label replaces the producer's, and a row no model produced carries none"
    );
}

/// A producer column inside the `_` namespace and outside the optional set raises `StoreReservedColumnName` at reconciliation, before any Parquet.
#[test]
fn a_reserved_producer_column_refuses_before_any_parquet() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    let err = f.land(&d, "run-1", json!([{"id": "a", "_score": 1}]), "2030-01-01T00:00:00Z").unwrap_err();
    assert!(matches!(err.store(), Some(StoreError::StoreReservedColumnName(_))), "{err}");
    assert!(!f.table_dir("filings").exists());
}

/// A producer sets any of `_modality`, `_lang`, `_provenance` and `_prompt_hash`, and each surfaces in the provenance envelope where present.
#[test]
fn a_producer_sets_the_optional_columns_and_modality_is_checked() {
    let f = Fixture::new();
    let d = decl("name = \"notes\"");
    let hash = format!("sha256:{}", "0".repeat(64));
    f.land(&d, "run-1", json!([{"id": "a", "_modality": "text", "_lang": "en-GB", "_prompt_hash": hash}]), "2030-01-01T00:00:00Z").unwrap();
    assert_eq!(f.query(&d, Bounds::default(), "SELECT _modality, _lang FROM t"), [[s("text"), s("en-GB")]]);
    let err = f.land(&d, "run-2", json!([{"id": "b", "_modality": "video"}]), "2030-01-01T00:01:00Z").unwrap_err();
    assert!(err.to_string().contains("video"), "{err}");
    assert_eq!(f.scan(&d, Bounds::default()).unwrap().files.len(), 1, "the invalid batch landed");
}

/// An optional column's value is held to its vocabulary whatever JSON type it arrives as:
/// a number is read as its text and refused like the same text would be, and a null is a
/// value the producer did not set.
#[test]
fn an_optional_column_is_checked_whatever_json_type_it_arrives_as() {
    let f = Fixture::new();
    let d = decl("name = \"notes\"");
    for (col, value) in [("_modality", json!(7)), ("_prompt_hash", json!(12345)), ("_modality", json!(true))] {
        let rows = json!([{"id": "a", col.to_string(): value}]);
        let err = f.land(&d, "run-1", rows, "2030-01-01T00:00:00Z").unwrap_err();
        assert!(err.to_string().contains(col), "a non-string `{col}` landed unchecked: {err}");
    }
    // A null is absence, not a bad value.
    f.land(&d, "run-1", json!([{"id": "a", "_modality": null}]), "2030-01-01T00:00:00Z").unwrap();
}

/// A pipeline declaring a table inside a reserved namespace raises `StoreReservedTableName` when its manifest is assembled, naming the reservation.
#[test]
fn a_reserved_table_name_refuses_the_landing() {
    let f = Fixture::new();
    let err = f.land(&decl("name = \"_runs\""), "run-1", json!([{"id": "a"}]), "2030-01-01T00:00:00Z").unwrap_err();
    assert!(matches!(err.store(), Some(StoreError::StoreReservedTableName(_))), "{err}");
}

/// The injected columns every landing carries stay non-null in `schema.json`, whichever
/// batch reached the table first and whatever columns later batches add.
#[test]
fn injected_columns_stay_non_null_in_the_merged_schema() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    let nullability = |f: &Fixture| -> Vec<(String, bool)> {
        let doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(f.table_dir("filings").join("schema.json")).unwrap()).unwrap();
        doc["fields"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| ["_ingested_at", "_run_id", "_site_id"].contains(&c["name"].as_str().unwrap()))
            .map(|c| (c["name"].as_str().unwrap().to_string(), c["nullable"].as_bool().unwrap()))
            .collect()
    };
    let expected = [("_ingested_at".to_string(), false), ("_run_id".to_string(), false), ("_site_id".to_string(), false)];

    f.land(&d, "run-1", json!([{"id": "a"}]), "2030-01-01T00:00:00Z").unwrap();
    assert_eq!(nullability(&f), expected, "after the first landing");
    f.land(&d, "run-2", json!([{"id": "b", "title": "t"}]), "2030-01-01T00:01:00Z").unwrap();
    assert_eq!(nullability(&f), expected, "after a landing adding a column");

    // A table whose first run landed zero rows merges the injected columns into an empty schema.
    let g = Fixture::new();
    g.land(&d, "run-1", json!([]), "2030-01-01T00:00:00Z").unwrap();
    g.land(&d, "run-2", json!([{"id": "a"}]), "2030-01-01T00:01:00Z").unwrap();
    assert_eq!(nullability(&g), expected, "after an empty first run");
}

/// The engine injects `_commit_seq`, a non-null int64 its commit assigns above every value the table holds; a run
/// committing after a read carries a value above every row that read returned, whatever its `_ingested_at`.
// spec: store.reserve.commit-seq@4bd29584
#[test]
fn a_run_committing_after_a_read_carries_a_higher_commit_seq_whatever_its_stamp() {
    let f = Fixture::new();
    let d = decl("name = \"spans\"");
    f.land(&d, "run-late-stamp", json!([{"id": "a"}, {"id": "b"}]), "2030-01-01T00:10:00Z").unwrap();
    let read = f.query(&d, Bounds::default(), "SELECT max(_commit_seq) FROM t");
    let seen: i64 = read[0][0].clone().unwrap().parse().unwrap();

    // A second writer commits later under an earlier transaction-time stamp.
    f.land(&d, "run-early-stamp", json!([{"id": "c", "_commit_seq": 0}]), "2030-01-01T00:00:00Z").unwrap();
    let rows = f.query(&d, Bounds::default(), "SELECT id, _commit_seq FROM t WHERE _commit_seq > (SELECT max(_commit_seq) FROM t WHERE _run_id = 'run-late-stamp') ORDER BY id");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0][0], s("c"));
    let late: i64 = rows[0][1].clone().unwrap().parse().unwrap();
    assert!(late > seen, "the later commit carries {late}, not above the {seen} the read returned");
    let stamps = f.query(&d, Bounds::default(), "SELECT id FROM t ORDER BY _ingested_at, id");
    assert_eq!(stamps[0][0], s("c"), "the later commit sorts first by `_ingested_at`");

    // The column is a required int64 in every part, and one run's rows share one value.
    for file in f.scan(&d, Bounds::default()).unwrap().files {
        let described = query(&format!(
            "SELECT type, repetition_type FROM parquet_schema('{}') WHERE name = '_commit_seq'",
            f.store.root().join(&file).display()
        ));
        assert_eq!(described, [[s("INT64"), s("REQUIRED")]], "{file}");
    }
    assert_eq!(f.query(&d, Bounds::default(), "SELECT count(DISTINCT _commit_seq) FROM t WHERE _run_id = 'run-late-stamp'"), [[s("1")]]);
}

/// Commits of one table serialize from assigning `_commit_seq` to the step that makes the run readable, so the
/// readable runs of a table always hold a prefix of its commit sequence.
// spec: store.reserve.commit-order@0fa09147
#[test]
fn concurrent_commits_expose_a_prefix_of_the_commit_sequence_to_every_read() {
    let f = Fixture::new();
    let d = decl("name = \"spans\"");
    f.land(&d, "run-0", json!([{"id": "seed"}]), "2030-01-01T00:00:00Z").unwrap();
    let writers = 6;
    let done = std::sync::atomic::AtomicUsize::new(0);
    let prefixes = std::thread::scope(|scope| {
        for w in 0..writers {
            let (f, d, done) = (&f, &d, &done);
            scope.spawn(move || {
                let rows = json!((0..50).map(|i| json!({"id": format!("w{w}-{i}")})).collect::<Vec<_>>());
                // Stamps descend as writers ascend, so no stamp order matches the commit order.
                f.land(d, &format!("run-{}", w + 1), rows, &format!("2030-01-01T00:{:02}:00Z", 59 - w)).unwrap();
                done.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            });
        }
        let mut reads = 0;
        loop {
            let finished = done.load(std::sync::atomic::Ordering::SeqCst) == writers;
            let seqs: Vec<i64> = f
                .query(&d, Bounds::default(), "SELECT DISTINCT _commit_seq FROM t ORDER BY 1")
                .into_iter()
                .map(|r| r[0].clone().unwrap().parse().unwrap())
                .collect();
            let expected: Vec<i64> = (1..=seqs.len() as i64).collect();
            assert_eq!(seqs, expected, "a read observed a gap in the commit sequence");
            reads += 1;
            if finished {
                break reads;
            }
        }
    });
    assert!(prefixes >= 1);
    assert_eq!(f.query(&d, Bounds::default(), "SELECT count(DISTINCT _commit_seq), count(*) FROM t"), [[s("7"), s("301")]]);
}

/// A commit assigns one above the greatest of the node's counter, every run manifest's `commit_seq` and the current
/// snapshot's, so a store restored by a pull, or holding no counter, never reissues a value.
// spec: store.reserve.commit-seq-seed@00000000
#[test]
fn a_store_holding_no_counter_continues_above_the_values_its_manifests_record() {
    let f = Fixture::new();
    let d = decl("name = \"spans\"");
    let counter = f.table_dir("spans").join("data/.commit_seq");
    let seqs = |f: &Fixture| -> Vec<(Option<String>, i64)> {
        f.query(&d, Bounds::default(), "SELECT DISTINCT _run_id, _commit_seq FROM t ORDER BY 2")
            .into_iter()
            .map(|r| (r[0].clone(), r[1].clone().unwrap().parse().unwrap()))
            .collect()
    };
    f.land(&d, "run-1", json!([{"id": "a"}]), "2030-01-01T00:00:00Z").unwrap();
    f.land(&d, "run-2", json!([{"id": "b"}]), "2030-01-01T00:01:00Z").unwrap();

    // A pull onto a fresh machine restores the runs and not the node-local counter.
    std::fs::remove_file(&counter).unwrap();
    let m = f.land(&d, "run-3", json!([{"id": "c"}]), "2030-01-01T00:02:00Z").unwrap();
    assert_eq!(m.commit_seq, Some(3), "the run manifest records its commit sequence value");
    assert_eq!(seqs(&f), [(s("run-1"), 1), (s("run-2"), 2), (s("run-3"), 3)]);

    // Folded and collected: the current snapshot alone records the values its rows carry.
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    std::fs::remove_dir_all(f.table_dir("spans").join("data/runs")).unwrap();
    std::fs::remove_file(&counter).unwrap();
    f.land(&d, "run-4", json!([{"id": "d"}]), "2030-01-01T02:00:00Z").unwrap();
    assert_eq!(seqs(&f), [(s("run-1"), 1), (s("run-2"), 2), (s("run-3"), 3), (s("run-4"), 4)]);
}
