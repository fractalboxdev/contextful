//! `store.bound-time`: bounds, their resolution and their echo.

use super::{at, run, snapshot};
use contextful_core::store::bound_time::{Bound, Bounds};
use contextful_core::store::declare::{TableDecl, ValidTime};
use contextful_core::store::reconcile::{Column, ColumnType, Schema};
use contextful_core::store::relation::relation;
use contextful_core::store::resolve::TableState;
use contextful_core::store::StoreError;

/// A snapshot's carried part resolves to its origin's directory beside its own delta, and
/// the file list stays sorted (`store.fold.carry-by-reference`).
#[test]
fn a_carried_part_resolves_under_its_origin_snapshot() {
    use contextful_core::store::lay_out::PartEntry;
    let s0 = snapshot("2030-01-01T02:00:00Z", None, &["run-1"]);
    let mut s1 = snapshot("2030-01-01T04:00:00Z", Some(&s0.snapshot_id), &["run-2"]);
    s1.parts = vec![PartEntry::new("part-00000.parquet", 0), PartEntry { origin: Some(s0.snapshot_id.clone()), ..PartEntry::new("part-00000.parquet", 0) }];
    let runs = vec![run("run-1", "2030-01-01T01:00:00Z", 1), run("run-2", "2030-01-01T03:00:00Z", 1)];
    let state = TableState { table: "filings".into(), runs, chain: vec![s1.clone(), s0.clone()], ..TableState::default() };
    let files = state.resolve(None).unwrap().files();
    let expected = [s0.snapshot_id.to_string(), s1.snapshot_id.to_string()].map(|id| format!("data/snapshots/{id}/part-00000.parquet"));
    assert_eq!(files, expected);
}

fn state() -> TableState {
    let s0 = snapshot("2030-01-01T02:00:00Z", None, &["run-1", "run-2"]);
    let s1 = snapshot("2030-01-01T04:00:00Z", Some(&s0.snapshot_id), &["run-3"]);
    TableState {
        table: "filings".into(),
        write_mode: Default::default(),
        runs: vec![
            run("run-1", "2030-01-01T00:00:00Z", 1),
            run("run-2", "2030-01-01T01:00:00Z", 1),
            run("run-3", "2030-01-01T03:00:00Z", 1),
            run("run-4", "2030-01-01T05:00:00Z", 1),
        ],
        chain: vec![s1, s0],
        history_collected: false,
        collected: Default::default(),
    }
}

fn names(files: Vec<String>) -> Vec<String> {
    files.into_iter().map(|f| f.split('/').take(3).collect::<Vec<_>>().join("/")).collect()
}

/// A read carrying neither bound resolves each table to its current snapshot plus the committed runs it omits.
// spec: store.bound-time.unbounded-latest@dfc0eeb4
#[test]
fn an_unbounded_read_is_the_current_snapshot_plus_omitted_runs() {
    let st = state();
    let r = st.resolve(None).unwrap();
    assert_eq!(r.snapshot.unwrap().snapshot_id, st.chain[0].snapshot_id);
    assert_eq!(r.runs.iter().map(|r| r.run_id.as_str()).collect::<Vec<_>>(), ["run-4"]);
    let files = r.files();
    let mut sorted = files.clone();
    sorted.sort();
    assert_eq!(files, sorted, "the file list is sorted");
}

/// Inside each table's FROM-source, `as_of` selects the newest reachable snapshot created at or before the bound plus
/// the committed runs at or before it that the snapshot omits.
// spec: store.bound-time.as-of@9f994b50
#[test]
fn as_of_resolves_to_the_newest_snapshot_at_or_before_it_plus_omitted_runs() {
    let st = state();
    let b = |s: &str| Some(Bound::parse(s).unwrap());

    // Between the two snapshots: the older one, plus run-3 committed before the bound.
    let r = st.resolve(b("2030-01-01T03:30:00Z")).unwrap();
    assert_eq!(r.snapshot.unwrap().snapshot_id, st.chain[1].snapshot_id);
    assert_eq!(r.runs.iter().map(|r| r.run_id.as_str()).collect::<Vec<_>>(), ["run-3"]);

    // Exactly at a snapshot's creation: that snapshot, inclusive.
    let r = st.resolve(b("2030-01-01T04:00:00Z")).unwrap();
    assert_eq!(r.snapshot.unwrap().snapshot_id, st.chain[0].snapshot_id);
    assert!(r.runs.is_empty());

    // Before every snapshot: the runs committed by then.
    let r = st.resolve(b("2030-01-01T00:30:00Z")).unwrap();
    assert!(r.snapshot.is_none());
    assert_eq!(names(r.files()), ["data/runs/run-1"]);
}

/// An `as_of` before the oldest snapshot retention kept raises `StoreAsOfUnretained`, naming the oldest
/// answerable instant.
// spec: store.bound-time.as-of-unretained@3a23145e
#[test]
fn an_as_of_before_collected_history_is_unretained() {
    let mut st = state();
    st.chain.truncate(1);
    st.runs.retain(|r| r.run_id != "run-1" && r.run_id != "run-2");
    st.history_collected = true;
    match st.resolve(Some(Bound::parse("2030-01-01T01:00:00Z").unwrap())) {
        Err(StoreError::StoreAsOfUnretained(m)) => assert!(m.contains("2030-01-01T04:00:00.000000000Z"), "{m}"),
        other => panic!("expected StoreAsOfUnretained, got {other:?}"),
    }
    // At or after the oldest retained snapshot, the read answers.
    st.resolve(Some(Bound::parse("2030-01-01T04:00:00Z").unwrap())).unwrap();
    // Uncollected history answers from the runs.
    let st = state();
    st.resolve(Some(Bound::parse("2030-01-01T00:30:00Z").unwrap())).unwrap();
}

/// An `as_of` resolving to a snapshot whose successors fold a run absent from the store raises `StoreAsOfUnretained`,
/// naming the oldest such successor's instant, rather than answering without the run.
#[test]
fn an_as_of_missing_a_run_a_later_snapshot_folds_is_unretained() {
    let mut st = state();
    // run-3, which s1 folds, is gone; a bound inside s0's span would read s0 without it.
    st.runs.retain(|r| r.run_id != "run-3");
    match st.resolve(Some(Bound::parse("2030-01-01T03:30:00Z").unwrap())) {
        Err(StoreError::StoreAsOfUnretained(m)) => assert!(m.contains("2030-01-01T04:00:00.000000000Z"), "{m}"),
        other => panic!("expected StoreAsOfUnretained, got {other:?}"),
    }
    st.resolve(Some(Bound::parse("2030-01-01T04:30:00Z").unwrap())).unwrap();
    st.resolve(None).unwrap();
}

/// An `as_of` resolving to a snapshot a collection reduced to its `_manifest.json` raises `StoreAsOfUnretained`,
/// while an unbounded read still folds the runs that snapshot names.
// spec: store.bound-time.as-of-unretained@3a23145e
#[test]
fn an_as_of_resolving_to_a_collected_snapshot_is_unretained() {
    let mut st = state();
    let oldest = st.chain[1].snapshot_id.to_string();
    st.collected.insert(oldest);
    st.runs.retain(|r| r.run_id != "run-1" && r.run_id != "run-2");
    st.history_collected = true;
    match st.resolve(Some(Bound::parse("2030-01-01T03:00:00Z").unwrap())) {
        Err(StoreError::StoreAsOfUnretained(m)) => assert!(m.contains("2030-01-01T04:00:00.000000000Z"), "{m}"),
        other => panic!("expected StoreAsOfUnretained, got {other:?}"),
    }
    st.resolve(Some(Bound::parse("2030-01-01T04:30:00Z").unwrap())).unwrap();
    // The collected snapshot's runs stay folded: re-listing run-1 reads nothing twice.
    st.runs.push(run("run-1", "2030-01-01T00:00:00Z", 1));
    let r = st.resolve(None).unwrap();
    assert!(!r.runs.is_empty(), "the unbounded read covers run-4");
    assert!(r.runs.iter().all(|r| r.run_id == "run-4"), "{:?}", r.runs.iter().map(|r| &r.run_id).collect::<Vec<_>>());
}

/// Every bound compares instants as timestamps, not strings; a date-only literal resolves to the start of the next
/// day, exclusive.
// spec: store.bound-time.instant-comparison@f1d28442
#[test]
fn bounds_compare_instants_and_a_date_resolves_to_the_next_day_exclusive() {
    // One instant at two offsets is one bound; string order would disagree.
    let offset = Bound::parse("2030-01-01T05:00:00+02:00").unwrap();
    assert_eq!(offset.at, at("2030-01-01T03:00:00Z"));
    assert!(offset.admits(at("2030-01-01T03:00:00Z")));
    assert!(!offset.admits(at("2030-01-01T04:00:00Z")));

    let day = Bound::parse("2030-01-01").unwrap();
    assert_eq!(day.at, at("2030-01-02T00:00:00Z"));
    assert!(!day.inclusive);
    assert!(day.admits(at("2030-01-01T23:59:59.999999999Z")));
    assert!(!day.admits(at("2030-01-02T00:00:00Z")));
    assert!(Bound::parse("2030-13-01").is_err());
}

/// A bounded read returns `contextful.bounds` as `{as_of?, valid_as_of?, inclusive}`, each instant RFC 3339 UTC with
/// nine fractional digits and a `Z`; an unbounded read omits it.
// spec: store.bound-time.echo@6e0cc0a7
#[test]
fn a_bounded_read_echoes_its_bounds() {
    assert_eq!(Bounds::default().echo(), None);
    let b = Bounds { as_of: Some(Bound::parse("2030-01-01T01:00:00+01:00").unwrap()), valid_as_of: None, available_as_of: None, ..Bounds::default() };
    assert_eq!(b.echo().unwrap(), serde_json::json!({"as_of": "2030-01-01T00:00:00.000000000Z", "inclusive": true}));
    let b = Bounds { as_of: None, valid_as_of: Some(Bound::parse("2030-01-01").unwrap()), available_as_of: None, ..Bounds::default() };
    assert_eq!(b.echo().unwrap(), serde_json::json!({"valid_as_of": "2030-01-02T00:00:00.000000000Z", "inclusive": false}));
}

/// A declared valid-time column whose type is not a timestamp raises `StoreValidTimeNotTimestamp` at declaration.
// spec: store.bound-time.valid-time-type@013162fb
#[test]
fn a_valid_time_column_not_timestamp_typed_is_refused() {
    let mut t = TableDecl::named("filings");
    t.valid_time = Some(ValidTime { from: "effective_from".into(), to: Some("effective_to".into()) });
    let schema = |to: ColumnType| Schema {
        columns: vec![Column::new("effective_from", ColumnType::Timestamp, true), Column::new("effective_to", to, true)],
        ..Default::default()
    };
    t.validate(&schema(ColumnType::Timestamp)).unwrap();
    match t.validate(&schema(ColumnType::Utf8)) {
        Err(StoreError::StoreValidTimeNotTimestamp(m)) => assert!(m.contains("effective_to") && m.contains("Utf8"), "{m}"),
        other => panic!("expected StoreValidTimeNotTimestamp, got {other:?}"),
    }
}

/// A `valid_as_of` read against a table declaring no pair raises `StoreValidTimeUndeclared`, naming the table.
// spec: store.bound-time.valid-time-undeclared@7f17964f
#[test]
fn valid_as_of_on_a_table_declaring_no_pair_is_refused() {
    let t = TableDecl::named("filings");
    let b = Bound::parse("2030-01-01T00:00:00Z").unwrap();
    match relation(&t, &["/s/a.parquet".into()], &[], &[], Some(b)) {
        Err(StoreError::StoreValidTimeUndeclared(m)) => assert!(m.contains("filings"), "{m}"),
        other => panic!("expected StoreValidTimeUndeclared, got {other:?}"),
    }
}

/// Transaction time is `_ingested_at`; valid time is a declared pair of table columns, which the engine never
/// infers, and it stamps no second transaction timestamp.
// spec: store.bound-time.two-clocks@6f41b729
#[test]
fn transaction_time_is_ingested_at_and_valid_time_is_declared() {
    // A table with timestamp columns named like a pair still declares none.
    let t = TableDecl::parse_pipeline("[[pipeline.tables]]\nname = \"filings\"\n").unwrap().remove(0);
    assert!(t.valid_time.is_none());
    let b = Bound::parse("2030-01-01T00:00:00Z").unwrap();
    assert!(relation(&t, &["/s/a.parquet".into()], &[], &[], Some(b)).is_err());
    let cols: Vec<String> = contextful_core::store::reserve::Injection {
        run_id: "r".into(),
        site_id: "s".into(),
        batch_seq: None,
        authored_by: None, authored_agent: None,
        taint: None,
    }
    .columns()
    .into_iter()
    .filter(|c| c.ty == ColumnType::Timestamp)
    .map(|c| c.name)
    .collect();
    assert_eq!(cols, ["_ingested_at"]);
}

#[test]
fn an_exclusive_valid_bound_admits_rows_ending_at_it() {
    let mut t = TableDecl::named("rates");
    t.valid_time = Some(ValidTime { from: "f".into(), to: Some("u".into()) });
    let rel = relation(&t, &["/s/a.parquet".into()], &[], &[], Some(Bound::parse("2030-01-15").unwrap())).unwrap();
    assert!(rel.contains("\"f\" < TIMESTAMPTZ '2030-01-16T00:00:00.000000000Z' AND (\"u\" IS NULL OR \"u\" >= TIMESTAMPTZ '2030-01-16T00:00:00.000000000Z')"), "{rel}");
}

#[test]
fn a_parquet_relation_names_its_in_memory_encryption_key() {
    let decl = TableDecl::named("documents");
    let sql = contextful_core::store::relation::relation_with_encryption(
        &decl,
        &["/s/part.parquet".into()],
        &[],
        &[],
        None,
        Some("contextful_project"),
    )
    .unwrap();
    assert!(sql.contains("encryption_config = {footer_key: 'contextful_project'}"), "{sql}");
}
#[test]
fn encrypted_relations_union_each_file_without_reusing_parquet_metadata() {
    use contextful_core::store::declare::TableDecl;
    use contextful_core::store::relation::relation_with_encryption;

    let decl = TableDecl::named("documents");
    let sql = relation_with_encryption(&decl, &["a.parquet".into(), "b.parquet".into()], &[], &[], None, Some("project")).unwrap();
    assert!(!sql.contains("union_by_name"), "{sql}");
    assert_eq!(sql.matches("read_parquet(").count(), 2, "{sql}");
    assert!(sql.contains("UNION ALL BY NAME"), "{sql}");
    assert_eq!(sql.matches("footer_key: 'project'").count(), 2, "{sql}");
}
