//! `store.fold.carry-by-reference`, `store.fold.tiered-merge` and
//! `store.fold.reference-retention`: what a pass over an append table writes, what it
//! names at an earlier snapshot's directory, and what collection keeps for it.

use crate::support::{at, decl, Fixture};
use contextful_context::fold::fold;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::fold::{size_tier, FoldOutcome, FOLD_MERGE_FANOUT};
use contextful_core::store::lay_out::SnapshotManifest;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;

fn current(f: &Fixture, table: &str) -> SnapshotManifest {
    f.store.chain(table).unwrap().0.remove(0)
}

fn ids(f: &Fixture, d: &TableDecl, column: &str) -> BTreeSet<String> {
    contextful_context::rows::table_rows(&f.store, d, &[column])
        .unwrap()
        .into_iter()
        .map(|r| r[column].as_str().map(str::to_string).unwrap_or_else(|| r[column].to_string()))
        .collect()
}

fn events(prefix: &str, days: &[&str], n: usize) -> Value {
    Value::Array((0..n).map(|i| json!({ "id": format!("{prefix}-{i}"), "day": days[i % days.len()] })).collect())
}

/// An append table's pass writes its unfolded runs' rows as one delta part per partition they reach, rewrites only a part a delta row replaces or retention trims, and carries every other part by reference to its origin snapshot.
// spec: store.fold.carry-by-reference@f6fe20af
#[test]
fn a_pass_carries_every_part_its_runs_do_not_reach_by_reference() {
    let f = Fixture::new();
    let d = decl("name = \"events\"\npartition_by = [\"day\"]");
    f.land(&d, "run-1", events("a", &["2030-01-01", "2030-01-02"], 6), "2030-01-01T00:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let first = current(&f, "events");
    assert_eq!(first.parts.len(), 2);
    assert!(first.parts.iter().all(|p| p.origin.is_none() && p.rows == Some(3)), "{:?}", first.parts);

    // A run reaching one day: that day gains a delta part; both earlier parts stay where they are.
    f.land(&d, "run-2", events("b", &["2030-01-02"], 2), "2030-01-01T02:00:00Z").unwrap();
    assert!(matches!(fold(&f.store, &d, at("2030-01-01T03:00:00Z")).unwrap(), FoldOutcome::Folded { rows: 8, .. }));
    let second = current(&f, "events");
    let carried: Vec<_> = second.parts.iter().filter(|p| p.origin.is_some()).collect();
    assert_eq!(carried.len(), 2, "{:?}", second.parts);
    assert!(carried.iter().all(|p| p.origin.as_ref() == Some(&first.snapshot_id)));
    let delta: Vec<_> = second.parts.iter().filter(|p| p.origin.is_none()).collect();
    assert_eq!(delta.len(), 1);
    assert_eq!((delta[0].partition(), delta[0].rows), ("day=2030-01-02", Some(2)));
    // The new snapshot directory holds the delta and the manifest alone.
    let dir = f.store.snapshot_dir("events", &second.snapshot_id).unwrap();
    let mut written: Vec<String> = Vec::new();
    let mut stack = vec![dir.clone()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() { stack.push(p) } else { written.push(p.strip_prefix(&dir).unwrap().to_string_lossy().into_owned()) }
        }
    }
    written.sort();
    assert_eq!(written, ["_manifest.json".to_string(), format!("day=2030-01-02/{}", contextful_core::store::lay_out::part_name(0))]);
    // Every row reads through the resolved file list, carried parts at their origin.
    let files = f.scan(&d, Default::default()).unwrap().files;
    assert_eq!(files.iter().filter(|p| p.contains(&format!("/data/snapshots/{}/", first.snapshot_id))).count(), 2);
    assert_eq!(ids(&f, &d, "id").len(), 8);
}

/// A restated key rewrites only the part holding its stored row; a stored row ranking higher
/// keeps its place and the landed one leaves.
#[test]
fn a_restated_key_rewrites_only_the_part_holding_it() {
    let f = Fixture::new();
    let d = decl("name = \"bars\"\nprimary_key = [\"symbol\", \"day\"]\norder_by = \"rev\"\npartition_by = [\"year\"]");
    let rows = |rev: i64, days: &[&str]| {
        Value::Array(days.iter().flat_map(|day| ["S1", "S2"].map(|s| json!({ "symbol": s, "day": day, "year": &day[..4], "close": rev, "rev": rev }))).collect())
    };
    f.land(&d, "run-1", rows(1, &["2029-12-30", "2029-12-31", "2030-01-01"]), "2030-01-01T00:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let first = current(&f, "bars");
    // One day restated upward, another landed with a lower revision than the stored one.
    let mut restated = rows(2, &["2030-01-01"]).as_array().unwrap().clone();
    restated.push(json!({ "symbol": "S1", "day": "2029-12-31", "year": "2029", "close": 0, "rev": 0 }));
    f.land(&d, "run-2", Value::Array(restated), "2030-01-01T02:00:00Z").unwrap();
    assert!(matches!(fold(&f.store, &d, at("2030-01-01T03:00:00Z")).unwrap(), FoldOutcome::Folded { rows: 6, .. }));
    let second = current(&f, "bars");
    let carried: Vec<_> = second.parts.iter().filter(|p| p.origin.is_some()).map(|p| p.partition()).collect();
    assert_eq!(carried, ["year=2029"], "the 2029 part loses no row, so it stays carried");
    let held = contextful_context::rows::table_rows(&f.store, &d, &["symbol", "day", "rev"]).unwrap();
    assert_eq!(held.len(), 6, "one row per key");
    let rev = |s: &str, day: &str| held.iter().find(|r| r["symbol"] == s && r["day"] == day).unwrap()["rev"].as_i64().unwrap();
    assert_eq!((rev("S1", "2030-01-01"), rev("S2", "2030-01-01")), (2, 2));
    assert_eq!(rev("S1", "2029-12-31"), 1, "a stored row ranking higher keeps its place");
    assert_eq!(first.parts.len(), 2);
    assert!(first.parts.iter().all(|p| p.origin.is_none()));
}

/// Within each partition a pass writes into, 4 parts of one size tier, a tier holding row counts between consecutive powers of that number, merge into one part, repeating until no tier holds 4 parts.
// spec: store.fold.tiered-merge@f11d6692
#[test]
fn four_parts_of_one_tier_merge_into_one() {
    assert_eq!(FOLD_MERGE_FANOUT, 4);
    assert_eq!((size_tier(3), size_tier(4), size_tier(15), size_tier(16)), (0, 1, 1, 2));
    let f = Fixture::new();
    let d = decl("name = \"passages\"\nprimary_key = [\"id\"]");
    let land = |pass: usize| {
        let rows = Value::Array((0..5).map(|i| json!({ "id": format!("p{pass}-{i}") })).collect());
        f.land(&d, &format!("run-{pass}"), rows, &format!("2030-01-01T{:02}:00:00Z", 2 * pass)).unwrap();
        fold(&f.store, &d, at(&format!("2030-01-01T{:02}:30:00Z", 2 * pass))).unwrap();
        current(&f, "passages")
    };
    for pass in 0..3 {
        assert_eq!(land(pass).parts.len(), pass + 1, "three parts of one tier stay apart");
    }
    let merged = land(3);
    assert_eq!(merged.parts.len(), 1, "the fourth part of the tier merges all four: {:?}", merged.parts);
    assert_eq!((merged.parts[0].origin.as_ref(), merged.parts[0].rows), (None, Some(20)));
    // The merged part keeps the key order a whole snapshot has.
    let path = f.store.snapshot_dir("passages", &merged.snapshot_id).unwrap().join(&merged.parts[0].name);
    let held: Vec<String> = contextful_context::rows::table_rows(&f.store, &d, &["id"]).unwrap().into_iter().map(|r| r["id"].as_str().unwrap().to_string()).collect();
    let mut sorted = held.clone();
    sorted.sort();
    assert_eq!(held, sorted, "{}", path.display());
}

/// Collection keeps each part and sidecar segment a retained snapshot carries by reference, removing the rest of the collected snapshot's directory, manifest included, until no retained snapshot names them.
// spec: store.fold.reference-retention@7986e652
#[test]
fn collection_keeps_a_part_a_retained_snapshot_carries() {
    let f = Fixture::new();
    let d = decl("name = \"events\"\npartition_by = [\"day\"]");
    f.land(&d, "run-1", events("a", &["2030-01-01", "2030-01-02"], 4), "2030-01-01T00:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let first = current(&f, "events");
    let first_dir = f.store.snapshot_dir("events", &first.snapshot_id).unwrap();
    f.land(&d, "run-2", events("b", &["2030-01-02"], 1), "2030-01-02T00:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-02T01:00:00Z")).unwrap();
    // Past the window the first snapshot is collected, save the two parts the current one carries.
    f.land(&d, "run-3", events("c", &["2030-01-02"], 1), "2030-01-09T02:00:00Z").unwrap();
    let FoldOutcome::Folded { collected, .. } = fold(&f.store, &d, at("2030-01-09T02:00:00Z")).unwrap() else { panic!() };
    assert!(collected.contains(&format!("data/snapshots/{}", first.snapshot_id)), "{collected:?}");
    assert!(!first_dir.join("_manifest.json").exists());
    for p in &first.parts {
        assert!(first_dir.join(&p.name).is_file(), "{} is carried", p.name);
    }
    assert_eq!(ids(&f, &d, "id").len(), 6);
    let (chain, _) = f.store.chain("events").unwrap();
    assert!(!chain.is_empty());
    assert!(chain.iter().all(|s| s.snapshot_id != first.snapshot_id));

    // A merge that takes the carried part into a new one releases it; once no retained
    // snapshot names the directory, collection removes it.
    for (k, day) in (4..8).zip(["2030-01-01"; 4]) {
        f.land(&d, &format!("run-{k}"), events(&format!("d{k}"), &[day], 2), &format!("2030-01-1{k}T00:00:00Z")).unwrap();
        fold(&f.store, &d, at(&format!("2030-01-1{k}T01:00:00Z"))).unwrap();
    }
    let last = current(&f, "events");
    let day1: Vec<_> = last.parts.iter().filter(|p| p.partition() == "day=2030-01-01").collect();
    assert!(!day1.is_empty());
    assert!(day1.iter().all(|p| p.origin.as_ref() != Some(&first.snapshot_id)), "{day1:?}");
    f.land(&d, "run-9", events("e", &["2030-01-03"], 1), "2030-01-30T00:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-30T00:00:00Z")).unwrap();
    let still: Vec<_> = current(&f, "events").parts.iter().filter(|p| p.origin.as_ref() == Some(&first.snapshot_id)).map(|p| p.name.clone()).collect();
    for p in &first.parts {
        assert_eq!(first_dir.join(&p.name).is_file(), still.contains(&p.name), "{}", p.name);
    }
    assert_eq!(ids(&f, &d, "id").len(), 15);
}
