//! `store.lay-out.folded-index`: the folded-runs index a pass writes, which a read trusts in
//! place of the chain walk, answering exactly as the walk does.

use crate::support::{at, decl, Fixture};
use contextful_context::fold::fold;
use contextful_context::Store;
use contextful_core::store::bound_time::Bound;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::{FoldedIndex, FormatVersion, SnapshotManifest};
use serde_json::json;
use std::path::Path;

const TABLE: &str = "events";

fn table() -> TableDecl {
    decl("name = \"events\"\nretain_runs = \"2d\"")
}

/// The instant `hours` hours after 2030-01-01T00:00:00Z.
fn hour(hours: u64) -> String {
    let base = at("2030-01-01T00:00:00Z");
    base.plus_secs(hours * 3600).to_rfc3339_nanos()
}

/// Runs each pass folds: 13 passes flush the first segment past [`FoldedIndex::INLINE_MAX`] rows.
const RUNS_PER_PASS: u64 = 5;

/// Land the runs of pass `i` and fold them, six hours apart, so retention collects as the chain grows.
fn fold_run(f: &Fixture, d: &TableDecl, i: u64) {
    for j in 0..RUNS_PER_PASS {
        f.land(d, &format!("run-{i:03}-{j}"), json!([{"e": i * 10 + j}]), &hour(i * 6)).unwrap();
    }
    fold(&f.store, d, at(&hour(i * 6 + 1))).unwrap();
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let dest = to.join(path.file_name().unwrap());
        if path.is_dir() { copy_tree(&path, &dest) } else { std::fs::copy(&path, &dest).map(|_| ()).unwrap() }
    }
}

/// A copy of `f`'s store with every snapshot manifest's `folded_runs` removed: the walk alone.
fn walked(f: &Fixture) -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".contextful/context/research");
    copy_tree(f.store.root(), &root);
    let snapshots = root.join("tables").join(TABLE).join("data/snapshots");
    for entry in std::fs::read_dir(&snapshots).unwrap() {
        let path = entry.unwrap().path().join("_manifest.json");
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let mut m: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        m.as_object_mut().unwrap().remove("folded_runs");
        std::fs::write(&path, serde_json::to_vec_pretty(&m).unwrap()).unwrap();
    }
    let store = Store::open(dir.path(), "research").unwrap();
    (dir, store)
}

/// What a read of `store` under `bound` resolves to: its files, or the refusal's identifier.
fn answer(store: &Store, d: &TableDecl, bound: Option<Bound>) -> String {
    let state = match store.state_at(d, bound) {
        Ok(s) => s,
        Err(e) => return format!("err {}", e.to_string().split(':').next().unwrap_or_default()),
    };
    match state.resolve(bound) {
        Ok(r) => format!("ok {:?}", r.files()),
        Err(e) => format!("err {}", e.to_string().split(':').next().unwrap_or_default()),
    }
}

/// Every bound worth asking: unbounded, and at and just before each snapshot of the whole chain.
fn bounds(store: &Store) -> Vec<Option<Bound>> {
    let (chain, _) = store.chain(TABLE).unwrap();
    let mut out = vec![None];
    for s in &chain {
        let created = s.created_at.to_rfc3339_nanos();
        out.push(Some(Bound::parse(&created).unwrap()));
        out.push(Some(Bound::parse(&s.created_at.minus_secs(1).to_rfc3339_nanos()).unwrap()));
    }
    out
}

fn assert_same_as_walk(f: &Fixture, d: &TableDecl) {
    let (_dir, walk) = walked(f);
    let asked = bounds(&walk);
    assert!(asked.len() > 1);
    for bound in asked {
        assert_eq!(answer(&f.store, d, bound), answer(&walk, d, bound), "bound {bound:?}");
    }
}

fn head(f: &Fixture) -> SnapshotManifest {
    f.store.chain(TABLE).unwrap().0.remove(0)
}

/// Across 60 folds, with tier merges and retention collecting superseded snapshots, every read answers from the
/// index exactly as the chain walk does, at every snapshot.
// spec: store.lay-out.folded-index@9b96e03e
#[test]
fn the_index_answers_every_bound_as_the_walk_does() {
    let f = Fixture::synced();
    let d = table();
    for i in 0..60 {
        fold_run(&f, &d, i);
        if i % 5 == 4 {
            assert_same_as_walk(&f, &d);
        }
    }
    let index = head(&f).folded_runs.expect("the head carries an index");
    assert!(index.segments.iter().any(|s| s.rows >= 4 * FoldedIndex::INLINE_MAX as u64), "four segments of one tier merged: {:?}", index.segments);
    assert_eq!(index.segments.iter().map(|s| s.rows).sum::<u64>() + index.inline.len() as u64, 60 * RUNS_PER_PASS);
}

/// A pass writes `folded_runs` over its parent's valid index: a few runs stay inline, a pass reaching the inline bound
/// flushes them into a segment, and the next pass carries that segment by reference. The snapshot declares `2.1`.
// spec: store.fold.folded-index@7d8c7f01
#[test]
fn a_pass_carries_its_parents_index() {
    let f = Fixture::synced();
    let d = table();
    fold_run(&f, &d, 0);
    let first = head(&f);
    assert_eq!(first.format_version.to_string(), "1.1", "a self-contained snapshot carrying an index declares 1.1");
    let index = first.folded_runs.clone().unwrap();
    assert_eq!(index.through, first.snapshot_id);
    assert!(index.segments.is_empty() && index.inline.len() == RUNS_PER_PASS as usize, "a few runs stay inline");
    for i in 1..13 {
        fold_run(&f, &d, i);
    }
    let flushed = head(&f).folded_runs.unwrap();
    assert_eq!(flushed.segments.len(), 1, "the inline rows flush into a segment");
    assert!(flushed.segments[0].origin.is_none() && flushed.segments[0].rows == 13 * RUNS_PER_PASS && flushed.inline.is_empty());
    fold_run(&f, &d, 13);
    let carrying = head(&f);
    assert_eq!(carrying.format_version, FormatVersion::of_snapshot(&carrying));
    assert_eq!(carrying.format_version.to_string(), "2.1", "a snapshot carrying parts by reference declares 2.1");
    let next = carrying.folded_runs.unwrap();
    assert!(next.segments.len() == 1 && next.segments[0].origin.is_some(), "the parent's segment is carried");
    assert_eq!(next.inline.len(), RUNS_PER_PASS as usize);
}

/// An unbounded read stops at the pointer's valid index: older manifests are never read.
// spec: store.lay-out.folded-index-walk@420ede02
#[test]
fn an_unbounded_read_reads_the_head_alone() {
    let f = Fixture::synced();
    let d = table();
    for i in 0..10 {
        fold_run(&f, &d, i);
    }
    let before = answer(&f.store, &d, None);
    let head_id = head(&f).snapshot_id.to_string();
    let snapshots = f.table_dir(TABLE).join("data/snapshots");
    for entry in std::fs::read_dir(&snapshots).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().unwrap().to_string_lossy() != head_id {
            let _ = std::fs::remove_file(path.join("_manifest.json"));
        }
    }
    let state = f.store.state(&d).unwrap();
    assert_eq!(state.chain.len(), 1, "the walk stops at the head's index");
    assert!(state.index.is_some());
    assert_eq!(answer(&f.store, &d, None), before);
}

/// An index whose `through` names another snapshot, whose segment's bytes moved, or whose segment is gone is not
/// trusted: the read walks, and answers as the walk does.
// spec: store.lay-out.folded-index-valid@ca09d1fa
#[test]
fn an_invalid_index_falls_back_to_the_walk() {
    for damage in ["through", "bytes", "gone"] {
        let f = Fixture::synced();
        let d = table();
        for i in 0..13 {
            fold_run(&f, &d, i);
        }
        let mut top = head(&f);
        let dir = f.store.snapshot_dir(TABLE, &top.snapshot_id).unwrap();
        let own = top.folded_runs.as_ref().unwrap().segments.iter().find(|s| s.origin.is_none()).cloned().expect("the head writes a segment");
        match damage {
            "through" => {
                top.folded_runs.as_mut().unwrap().through = top.parent.clone().unwrap();
                std::fs::write(dir.join("_manifest.json"), serde_json::to_vec_pretty(&top).unwrap()).unwrap();
            }
            "bytes" => std::fs::write(dir.join(&own.name), b"not a segment").unwrap(),
            _ => std::fs::remove_file(dir.join(&own.name)).unwrap(),
        }
        let state = f.store.state(&d).unwrap();
        assert!(state.index.as_ref().is_none_or(|ix| ix.at > 0), "{damage}: the head's index is not trusted");
        assert_same_as_walk(&f, &d);
    }
}

/// A `folded_runs` field this build cannot decode, a corrupt row id or a future shape, reads as absent: the read walks
/// and answers as the walk does, refusing nothing.
#[test]
fn a_malformed_index_field_walks() {
    for shape in ["row", "future"] {
        let f = Fixture::synced();
        let d = table();
        for i in 0..4 {
            fold_run(&f, &d, i);
        }
        let top = head(&f);
        let path = f.store.snapshot_dir(TABLE, &top.snapshot_id).unwrap().join("_manifest.json");
        let mut doc: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        match shape {
            "row" => doc["folded_runs"]["inline"][0]["snapshot"] = json!("not-a-snapshot-id"),
            _ => doc["folded_runs"] = json!({"through": top.snapshot_id.to_string(), "layout": "a later shape"}),
        }
        std::fs::write(&path, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
        let state = f.store.state(&d).unwrap();
        assert!(state.index.as_ref().is_none_or(|ix| ix.at > 0), "{shape}: the head's field reads as absent");
        assert_same_as_walk(&f, &d);
    }
}

/// A collected snapshot manifest the record names as skipped ends a walk below a valid index, as a collected parent
/// does; a walk meeting no valid index fetches it, and a store without a part source refuses naming it. The chain's
/// folded keys from the head are the index's.
// spec: store.pull.husks-fetched@6ea68a8e
#[test]
fn a_skipped_husk_ends_a_walk_below_a_valid_index_and_a_walk_without_one_fetches_it() {
    use contextful_core::store::lay_out::{SkippedRuns, SKIPPED_RUNS_FILE};
    let f = Fixture::synced();
    let d = table();
    for i in 0..6 {
        fold_run(&f, &d, i);
    }
    let (chain, _) = f.store.chain(TABLE).unwrap();
    let state = f.store.state(&d).unwrap();
    let keys: std::collections::BTreeSet<String> = state.folded(0).into_iter().map(str::to_string).collect();
    assert!(!keys.is_empty());
    assert_eq!(f.store.folded_keys_from(TABLE, chain[0].snapshot_id.clone()).unwrap(), keys);
    assert_eq!(f.store.current_snapshot(TABLE).unwrap().map(|s| s.snapshot_id), Some(chain[0].snapshot_id.clone()));

    // The oldest snapshot's manifest leaves, recorded as a husk a lazy pull skipped.
    let oldest = chain.last().unwrap();
    let path = f.store.snapshot_dir(TABLE, &oldest.snapshot_id).unwrap().join("_manifest.json");
    std::fs::remove_file(&path).unwrap();
    let rel = path.strip_prefix(f.store.root()).unwrap().to_string_lossy().into_owned();
    std::fs::write(f.store.root().join(SKIPPED_RUNS_FILE), serde_json::to_vec(&SkippedRuns { manifests: vec![rel.clone()] }).unwrap()).unwrap();
    let bound = Some(Bound::parse(&oldest.created_at.to_rfc3339_nanos()).unwrap());
    assert!(f.store.state_at(&d, bound).is_ok(), "a walk below the head's index ends at the husk");

    // With no index anywhere, the walk needs the husk, and this store reads through no part source.
    for s in chain.iter().take(chain.len() - 1) {
        let p = f.store.snapshot_dir(TABLE, &s.snapshot_id).unwrap().join("_manifest.json");
        let mut m: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        m.as_object_mut().unwrap().remove("folded_runs");
        std::fs::write(&p, serde_json::to_vec_pretty(&m).unwrap()).unwrap();
    }
    let e = f.store.state(&d).unwrap_err().to_string();
    assert!(e.starts_with("SyncFoldedRunsUnfetched") && e.contains(&rel), "{e}");
}
