//! `run.backfill`: a planned range, chunk leases held by node and attempt, the fenced commit that
//! refuses a stale holder's land, and the sweep that collects paths no done row commits.

use crate::support::{at, Rig, T0};
use contextful_core::run::advance::CursorKind;
use contextful_core::run::backfill::{BackfillBlock, Bound, ChunkLease, ChunkRow, ChunkStatus, Window, CHUNK_LEASE_RENEWAL_SECS, CHUNK_LEASE_TTL_SECS};
use contextful_core::run::own::OwnerScope;
use contextful_engine::backfill::ChunkHold;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const PIPELINE: &str = "feed";
const TABLE: &str = "filings";
const RUN: &str = "bf-1";

fn block(max_chunks: u32, chunk_size: i64, start_from: i64) -> BackfillBlock {
    BackfillBlock { max_chunks: Some(max_chunks), chunk_size: Some(chunk_size), start_from }
}

fn window(start: i64, end: i64) -> Window {
    Window::new(Bound::Int(start), Bound::Int(end)).unwrap()
}

fn row(rig: &Rig, table: &str, name: &str) -> ChunkRow {
    rig.catalog().chunk_at(&OwnerScope::chunk(PIPELINE, table, name)).unwrap().unwrap_or_else(|| panic!("no chunk `{name}`"))
}

fn lease(rig: &Rig, table: &str, node: &str) -> Option<ChunkHold> {
    rig.engine.backfill().lease(PIPELINE, table, RUN, node).unwrap()
}

/// Write one part under `hold`'s attempt path below `root`.
fn land(root: &Path, hold: &ChunkHold) -> PathBuf {
    let dir = root.join(hold.path());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("part-0.parquet"), hold.lease.node.as_bytes()).unwrap();
    dir
}

/// Plan shape follows {{type:contextful_core::run::advance::CursorKind}}: `monotonic` plans independent time-window
/// or id-range chunks, `opaque-token` one sequential chunk walked in order, `snapshot-id` version ranges sequential
/// per stream and parallel across streams.
// spec: run.backfill.chunk-plan@cad84790
#[test]
fn a_plans_shape_follows_its_cursor_kind_and_a_sequential_plan_leases_one_chunk_at_a_time() {
    let rig = Rig::new();
    let backfill = rig.engine.backfill();
    let declared = block(8, 10, 0);

    // Monotonic chunks are independent: three workers lease three chunks at once.
    let orders = backfill.plan_range(PIPELINE, "orders", CursorKind::Monotonic, &declared, 30).unwrap();
    assert_eq!(orders.iter().map(|c| (c.predicate.clone(), c.sequential)).collect::<Vec<_>>(), [(window(0, 10), false), (window(10, 20), false), (window(20, 30), false)]);
    let held: Vec<String> = ["node-a", "node-b", "node-c"].iter().map(|n| lease(&rig, "orders", n).unwrap().chunk).collect();
    assert_eq!(held, ["c0", "c1", "c2"]);

    // A continuation token walks the whole range as one sequential chunk.
    let events = backfill.plan_range(PIPELINE, "events", CursorKind::OpaqueToken, &declared, 30).unwrap();
    assert_eq!(events.iter().map(|c| (c.chunk.as_str(), c.predicate.clone(), c.sequential)).collect::<Vec<_>>(), [("c0", window(0, 30), true)]);

    // Version ranges run in order within a stream and in parallel across streams.
    backfill.plan_range(PIPELINE, TABLE, CursorKind::SnapshotId, &declared, 20).unwrap();
    backfill.plan_range(PIPELINE, "notes", CursorKind::SnapshotId, &declared, 20).unwrap();
    let first = lease(&rig, TABLE, "node-a").unwrap();
    assert_eq!(first.chunk, "c0");
    assert!(lease(&rig, TABLE, "node-b").is_none(), "c1 waits on c0 in its stream");
    assert_eq!(lease(&rig, "notes", "node-b").unwrap().chunk, "c0", "another stream runs beside it");
    assert!(backfill.commit(&first).unwrap());
    assert_eq!(lease(&rig, TABLE, "node-b").unwrap().chunk, "c1");
}

/// A chunk row is {{type:contextful_core::run::backfill::ChunkRow}}; an interrupted plan resumes at the first chunk
/// not done.
// spec: run.backfill.chunk@a050c54e
#[test]
fn a_chunk_row_records_its_completion_and_a_replanned_backfill_resumes_past_it() {
    let rig = Rig::new();
    let backfill = rig.engine.backfill();
    let plan = backfill.plan_range(PIPELINE, TABLE, CursorKind::Monotonic, &block(8, 10, 0), 30).unwrap();
    assert_eq!(
        plan.iter().map(|c| (c.chunk.as_str(), c.status, c.attempts, c.cursor_committed)).collect::<Vec<_>>(),
        [("c0", ChunkStatus::Pending, 0, false), ("c1", ChunkStatus::Pending, 0, false), ("c2", ChunkStatus::Pending, 0, false)]
    );

    let hold = lease(&rig, TABLE, "node-a").unwrap();
    rig.clock.advance(5);
    assert!(rig.engine.backfill().commit(&hold).unwrap());
    let done = row(&rig, TABLE, "c0");
    assert_eq!((done.pipeline_id.as_str(), done.chunk.as_str(), done.predicate.clone()), (PIPELINE, "c0", window(0, 10)));
    assert_eq!((done.status, done.attempts, done.cursor_committed), (ChunkStatus::Done, 1, true));
    assert_eq!((done.started_at, done.completed_at), (Some(at(T0)), Some(at(T0).plus_secs(5))));

    // The interrupted backfill replans the same range: c0 keeps its done row and the next lease takes c1.
    let replanned = backfill.plan_range(PIPELINE, TABLE, CursorKind::Monotonic, &block(8, 10, 0), 30).unwrap();
    assert_eq!(replanned[0], done);
    assert_eq!(lease(&rig, TABLE, "node-b").unwrap().chunk, "c1");
}

/// A worker leases a chunk row, recording node id and attempt, for a 300 s time-to-live renewed every 60 s, and
/// writes under a path segmented by run, node, chunk and attempt.
// spec: run.backfill.chunk-lease@c7477be7
#[test]
fn a_chunk_lease_records_its_node_and_attempt_lives_300_s_and_renews_every_60_s() {
    assert_eq!((CHUNK_LEASE_TTL_SECS, CHUNK_LEASE_RENEWAL_SECS), (300, 60));
    let rig = Rig::new();
    let backfill = rig.engine.backfill();
    backfill.plan_range(PIPELINE, TABLE, CursorKind::Monotonic, &block(8, 10, 0), 10).unwrap();

    let mut hold = lease(&rig, TABLE, "node-a").unwrap();
    let recorded = ChunkLease { node: "node-a".into(), attempt: 1, expires_at: at(T0).plus_secs(300) };
    assert_eq!(hold.lease, recorded);
    assert_eq!(row(&rig, TABLE, "c0").lease, Some(recorded));
    assert_eq!(hold.path(), PathBuf::from("bf-1/node-a/c0/1"));

    // A renewal falls due 60 s after the lease was taken or last renewed.
    assert!(!hold.lease.renew_due(at(T0).plus_secs(59)));
    assert!(hold.lease.renew_due(at(T0).plus_secs(60)));

    // Renewed whenever due, the lease outlives its first 300 s.
    for _ in 0..6 {
        rig.clock.advance(59);
        assert!(!hold.lease.renew_due(rig.catalog().now().unwrap()));
        rig.clock.advance(1);
        assert!(hold.lease.renew_due(rig.catalog().now().unwrap()));
        assert!(backfill.renew(&mut hold).unwrap());
        assert!(lease(&rig, TABLE, "node-b").is_none(), "a renewed lease admits no second holder");
    }
    assert_eq!(row(&rig, TABLE, "c0").lease.unwrap().expires_at, at(T0).plus_secs(360 + 300));

    // Unrenewed, it lapses 300 s after its last renewal.
    rig.clock.advance(299);
    assert!(lease(&rig, TABLE, "node-b").is_none());
    rig.clock.advance(1);
    assert_eq!(lease(&rig, TABLE, "node-b").unwrap().lease.attempt, 2);
}

/// A chunk commits in one catalog update conditioned on `attempt = :mine AND status != 'done'`; an update matching no
/// row leaves that worker's files uncommitted.
// spec: run.backfill.fenced-commit@f0c18fbd
#[test]
fn two_workers_racing_one_chunk_commit_exactly_one_attempts_files() {
    let rig = Rig::new();
    let backfill = rig.engine.backfill();
    let root = rig.dir.path().join("landing");
    backfill.plan_range(PIPELINE, TABLE, CursorKind::Monotonic, &block(8, 10, 0), 10).unwrap();

    // Worker A pauses past its lease; worker B claims the chunk and both land files.
    let a = lease(&rig, TABLE, "node-a").unwrap();
    land(&root, &a);
    rig.clock.advance(CHUNK_LEASE_TTL_SECS as i64);
    let b = lease(&rig, TABLE, "node-b").unwrap();
    land(&root, &b);

    // A's commit matches no row: the chunk stays running under B and commits nothing.
    assert!(!backfill.commit(&a).unwrap());
    let running = row(&rig, TABLE, "c0");
    assert_eq!((running.status, running.attempts, running.cursor_committed), (ChunkStatus::Running, 2, false));
    assert_eq!(running.committed_path(RUN), None);

    // B's commit lands; a repeated commit meets a done row and matches nothing.
    assert!(backfill.commit(&b).unwrap());
    assert!(!backfill.commit(&b).unwrap());
    let done = row(&rig, TABLE, "c0");
    assert_eq!((done.status, done.attempts), (ChunkStatus::Done, 2));
    assert_eq!(done.committed_path(RUN), Some(b.path()), "only B's attempt is committed");
    assert_ne!(a.path(), b.path());
}

/// An expired lease lets a second worker claim the chunk, increment the attempt and write under its own path.
// spec: run.backfill.attempt-isolation@46ad85ba
#[test]
fn an_expired_lease_lets_a_second_worker_claim_the_next_attempt_under_its_own_path() {
    let rig = Rig::new();
    let backfill = rig.engine.backfill();
    backfill.plan_range(PIPELINE, TABLE, CursorKind::Monotonic, &block(8, 10, 0), 10).unwrap();

    let mut a = lease(&rig, TABLE, "node-a").unwrap();
    rig.clock.advance(CHUNK_LEASE_TTL_SECS as i64 - 1);
    assert!(lease(&rig, TABLE, "node-b").is_none(), "a live lease holds the chunk");
    rig.clock.advance(1);
    let b = lease(&rig, TABLE, "node-b").unwrap();
    assert_eq!((b.chunk.as_str(), b.lease.attempt), ("c0", 2));
    assert_eq!(b.path(), PathBuf::from("bf-1/node-b/c0/2"));
    assert_eq!(row(&rig, TABLE, "c0").attempts, 2);
    assert!(!backfill.renew(&mut a).unwrap(), "the superseded holder renews nothing");

    // The same node claiming again after a lapse still writes under a fresh attempt.
    rig.clock.advance(CHUNK_LEASE_TTL_SECS as i64);
    let again = lease(&rig, TABLE, "node-a").unwrap();
    assert_eq!(again.lease.attempt, 3);
    assert!(again.path() != a.path() && again.path() != b.path());
}

/// The catalog row signals commit; directory presence does not. Retention collects files under paths without a done
/// row according to a retention window.
// spec: run.backfill.commit-signal@7f2c019d
#[test]
fn only_a_done_row_commits_a_path_and_the_sweep_collects_paths_without_one() {
    let rig = Rig::new();
    let backfill = rig.engine.backfill();
    let root = rig.dir.path().join("landing");
    backfill.plan_range(PIPELINE, TABLE, CursorKind::Monotonic, &block(8, 10, 0), 20).unwrap();

    // c0: A's attempt lapses, B's commits. c1: C holds a live lease and has landed.
    let a = lease(&rig, TABLE, "node-a").unwrap();
    let stale = land(&root, &a);
    rig.clock.advance(CHUNK_LEASE_TTL_SECS as i64);
    let b = lease(&rig, TABLE, "node-b").unwrap();
    assert_eq!(b.chunk, "c0");
    let committed = land(&root, &b);
    assert!(backfill.commit(&b).unwrap());
    let c = lease(&rig, TABLE, "node-c").unwrap();
    assert_eq!(c.chunk, "c1");
    let live = land(&root, &c);

    // Inside the retention window nothing is collected.
    assert!(backfill.collect(PIPELINE, TABLE, RUN, &root, SystemTime::UNIX_EPOCH).unwrap().is_empty());
    assert!(stale.exists());

    // Past it, A's path goes: it exists on disk but no done row names it. B's done path and C's live one stay.
    let cutoff = SystemTime::now() + Duration::from_secs(60);
    assert_eq!(backfill.collect(PIPELINE, TABLE, RUN, &root, cutoff).unwrap(), std::slice::from_ref(&stale));
    assert!(!stale.exists());
    assert!(committed.join("part-0.parquet").exists());
    assert!(live.join("part-0.parquet").exists());

    // C's lease lapses, but no successor holds c1: C may still commit, so its path stays.
    rig.clock.advance(CHUNK_LEASE_TTL_SECS as i64);
    assert!(backfill.collect(PIPELINE, TABLE, RUN, &root, cutoff).unwrap().is_empty());
    assert!(live.join("part-0.parquet").exists());

    // Once node-d re-leases c1, C's attempt is superseded: its path is collected and its commit refused.
    let d = lease(&rig, TABLE, "node-d").unwrap();
    assert_eq!((d.chunk.as_str(), d.lease.attempt), ("c1", 2));
    assert_eq!(backfill.collect(PIPELINE, TABLE, RUN, &root, cutoff).unwrap(), std::slice::from_ref(&live));
    assert!(!backfill.commit(&c).unwrap());
    assert!(committed.exists());
}

/// A rewind clears the chunk's lease, so a commit from the attempt it rewound applies nothing.
#[test]
fn a_rewound_chunk_refuses_the_commit_of_the_attempt_it_rewound() {
    let rig = Rig::new();
    let backfill = rig.engine.backfill();
    backfill.plan_range(PIPELINE, TABLE, CursorKind::Monotonic, &block(8, 10, 0), 20).unwrap();

    // A holds c0 when an operator rewinds it: A's late commit applies nothing.
    let a = lease(&rig, TABLE, "node-a").unwrap();
    assert_eq!(a.chunk, "c0");
    backfill.rewind(PIPELINE, TABLE, &window(0, 10)).unwrap();
    assert!(!backfill.commit(&a).unwrap());
    let pending = row(&rig, TABLE, "c0");
    assert_eq!((pending.status, pending.lease), (ChunkStatus::Pending, None));

    // B commits c0, it is rewound, and B's retried commit applies nothing.
    let b = lease(&rig, TABLE, "node-b").unwrap();
    assert_eq!((b.chunk.as_str(), b.lease.attempt), ("c0", 2));
    assert!(backfill.commit(&b).unwrap());
    backfill.rewind(PIPELINE, TABLE, &window(0, 10)).unwrap();
    assert!(!backfill.commit(&b).unwrap());
    let rewound = row(&rig, TABLE, "c0");
    assert_eq!((rewound.status, rewound.committed_path(RUN)), (ChunkStatus::Pending, None));
}
