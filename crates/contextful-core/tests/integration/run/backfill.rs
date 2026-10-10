//! `run.backfill`: chunk rows, their windows and the transitions a claim, a stop, a commit and
//! a rewind take.

use super::at;
use contextful_core::run::backfill::{Bound, ChunkRow, ChunkStatus, Window};
use contextful_core::run::own::OwnerScope;
use contextful_core::run::record::RunStatus;
use contextful_core::run::RunError;

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
