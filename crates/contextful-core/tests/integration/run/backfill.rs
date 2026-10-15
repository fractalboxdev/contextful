//! `run.backfill`: chunk rows, their windows and the transitions a claim, a stop, a commit and
//! a rewind take.

use super::at;
use contextful_core::run::advance::CursorKind;
use contextful_core::run::backfill::{chunk_path, BackfillBlock, Bound, ChunkLease, ChunkPlan, ChunkRow, ChunkStatus, Window, CHUNK_LEASE_TTL_SECS};
use contextful_core::run::own::OwnerScope;
use contextful_core::run::record::RunStatus;
use contextful_core::run::RunError;
use serde_json::json;

fn window(start: i64, end: i64) -> Window {
    Window::new(Bound::Int(start), Bound::Int(end)).unwrap()
}

#[test]
fn a_half_open_window_overlaps_only_a_window_on_its_scale_sharing_a_position() {
    let w = window(10, 20);
    assert!(w.overlaps(&window(0, 11)));
    assert!(w.overlaps(&window(19, 30)));
    assert!(!w.overlaps(&window(0, 10)), "the end is exclusive");
    assert!(!w.overlaps(&window(20, 30)));
    let text = Window::new(Bound::Text("2030-01-01".into()), Bound::Text("2030-02-01".into())).unwrap();
    assert!(!w.comparable(&text));
    assert!(!w.overlaps(&text));
    assert!(matches!(Window::new(Bound::Text("b".into()), Bound::Text("a".into())), Err(RunError::PipelineRewindWindowInvalid(_))));
}

#[test]
fn a_chunk_claims_settles_on_its_close_and_rewinds() {
    let mut c = ChunkRow::planned("feed", "filings", "c0", 0, window(0, 10));
    assert_eq!(c.scope(), OwnerScope::chunk("feed", "filings", "c0"));
    assert!(c.claimable());

    c.claim(at("2030-01-01T00:00:00Z"));
    assert_eq!((c.status, c.attempts), (ChunkStatus::Running, 1));
    assert!(!c.claimable());
    c.settle(RunStatus::Canceled);
    assert_eq!((c.status, c.attempts), (ChunkStatus::Pending, 1), "a stop leaves the attempt count");

    c.claim(at("2030-01-01T00:01:00Z"));
    c.settle(RunStatus::Failed);
    assert_eq!((c.status, c.attempts), (ChunkStatus::Failed, 2));
    assert!(c.claimable());

    c.claim(at("2030-01-01T00:02:00Z"));
    c.complete(at("2030-01-01T00:03:00Z"));
    c.settle(RunStatus::Success);
    assert_eq!((c.status, c.attempts, c.cursor_committed), (ChunkStatus::Done, 3, true));
    c.settle(RunStatus::Canceled);
    assert_eq!(c.status, ChunkStatus::Done, "a done chunk settles no further");

    c.rewind();
    assert_eq!((c.status, c.cursor_committed, c.completed_at), (ChunkStatus::Pending, false, None));
}

#[test]
fn a_seeding_scope_is_a_scope_of_its_own() {
    let seed = OwnerScope::seed("feed", "filings");
    assert_eq!(seed, OwnerScope::table("feed", "filings#seed"));
    assert_ne!(seed, OwnerScope::table("feed", "filings"));
}

fn block(value: serde_json::Value) -> BackfillBlock {
    BackfillBlock::from_value(&value).unwrap()
}

fn spans(plan: &ChunkPlan) -> Vec<(String, i64, i64)> {
    plan.chunks
        .iter()
        .map(|(name, w)| match (&w.start, &w.end) {
            (Bound::Int(s), Bound::Int(e)) => (name.clone(), *s, *e),
            other => panic!("an integer plan holds integer windows, not {other:?}"),
        })
        .collect()
}

/// `[pipeline.backfill]` declares `max_chunks`, the bound on a plan's chunk count; `chunk_size`, one clock-ranged
/// chunk's width in cursor units; and `start_from`, the floor, default zero.
// spec: run.backfill.block@3577d3c7
#[test]
fn a_backfill_block_declares_a_chunk_bound_a_width_and_a_floor_defaulting_to_zero() {
    let declared = block(json!({"max_chunks": 4, "chunk_size": 10, "start_from": 5}));
    assert_eq!(declared, BackfillBlock { max_chunks: Some(4), chunk_size: Some(10), start_from: 5 });
    assert_eq!(block(json!({"max_chunks": 4, "chunk_size": 10})).start_from, 0, "the floor defaults to zero");
    for malformed in [json!({"chunks": 4}), json!({"chunk_size": "ten"}), json!({"max_chunks": -1})] {
        assert!(matches!(BackfillBlock::from_value(&malformed), Err(RunError::PipelineSpecInvalid(_))), "{malformed}");
    }

    // The plan starts at the floor, steps by the width and holds at most `max_chunks` chunks.
    let plan = ChunkPlan::of(CursorKind::Monotonic, &declared, 40).unwrap();
    assert_eq!(spans(&plan), [("c0".into(), 5, 15), ("c1".into(), 15, 25), ("c2".into(), 25, 35), ("c3".into(), 35, 40)]);
    assert!(matches!(ChunkPlan::of(CursorKind::Monotonic, &declared, 46), Err(RunError::PipelineSpecInvalid(_))), "five chunks pass a bound of four");
    let widest = BackfillBlock { max_chunks: Some(4), chunk_size: Some(i64::MAX), start_from: 1 };
    assert_eq!(spans(&ChunkPlan::of(CursorKind::Monotonic, &widest, 10).unwrap()), [("c0".into(), 1, 10)], "a width past the range ends at the range");
    for partial in [json!({"max_chunks": 4}), json!({"chunk_size": 10}), json!({"max_chunks": 4, "chunk_size": 0})] {
        assert!(matches!(ChunkPlan::of(CursorKind::Monotonic, &block(partial.clone()), 40), Err(RunError::PipelineSpecInvalid(_))), "{partial}");
    }
}

#[test]
fn a_plan_takes_its_shape_from_the_cursor_kind() {
    let declared = block(json!({"max_chunks": 8, "chunk_size": 10}));
    let monotonic = ChunkPlan::of(CursorKind::Monotonic, &declared, 25).unwrap();
    assert!(!monotonic.sequential);
    assert_eq!(spans(&monotonic), [("c0".into(), 0, 10), ("c1".into(), 10, 20), ("c2".into(), 20, 25)]);

    let token = ChunkPlan::of(CursorKind::OpaqueToken, &BackfillBlock::default(), 25).unwrap();
    assert!(token.sequential);
    assert_eq!(spans(&token), [("c0".into(), 0, 25)], "a continuation token walks one chunk");

    let snapshot = ChunkPlan::of(CursorKind::SnapshotId, &declared, 25).unwrap();
    assert!(snapshot.sequential);
    assert_eq!(spans(&snapshot), spans(&monotonic));

    assert!(ChunkPlan::of(CursorKind::Monotonic, &declared, 0).unwrap().chunks.is_empty(), "an empty range plans nothing");
}

#[test]
fn a_chunk_lease_claims_an_attempt_renews_while_current_and_fences_a_stale_commit() {
    let t0 = at("2030-01-01T00:00:00Z");
    let mut c = ChunkRow::planned("feed", "filings", "c0", 0, window(0, 10));
    let a = c.lease("node-a", t0).expect("a pending chunk leases");
    assert_eq!(a, ChunkLease { node: "node-a".into(), attempt: 1, expires_at: t0.plus_secs(CHUNK_LEASE_TTL_SECS) });
    assert!(c.lease("node-b", t0.plus_secs(CHUNK_LEASE_TTL_SECS - 1)).is_none(), "a live lease admits no second holder");
    let b = c.lease("node-b", t0.plus_secs(CHUNK_LEASE_TTL_SECS)).expect("an expired lease admits a second holder");
    assert_eq!((b.attempt, c.attempts), (2, 2));
    assert!(c.renew(&a, t0.plus_secs(CHUNK_LEASE_TTL_SECS)).is_none(), "a superseded holder renews nothing");
    assert!(!c.commit_fenced(a.attempt, t0.plus_secs(CHUNK_LEASE_TTL_SECS)));
    assert_eq!(c.status, ChunkStatus::Running);
    assert!(c.commit_fenced(b.attempt, t0.plus_secs(CHUNK_LEASE_TTL_SECS + 1)));
    assert_eq!((c.status, c.cursor_committed), (ChunkStatus::Done, true));
    assert!(!c.commit_fenced(b.attempt, t0.plus_secs(CHUNK_LEASE_TTL_SECS + 2)), "a done row matches no second commit");
    assert_eq!(c.committed_path("bf-1"), Some(chunk_path("bf-1", "node-b", "c0", 2)));
}
