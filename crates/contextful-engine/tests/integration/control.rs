//! `surface.apply` and `surface.reconcile` over the local snapshot directory.

use contextful_core::surface::SurfaceError;
use contextful_engine::control::{ControlError, SnapshotDir};

/// An apply whose compare-and-swap loses raises `ManifestVersionConflict`, reloads the winning version and
/// reapplies its pending edits onto it, overwriting no applied version.
// spec: surface.apply.version-race@30fc0e6c
#[test]
fn a_lost_claim_conflicts_and_overwrites_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let snaps = SnapshotDir::open(dir.path());
    assert_eq!(snaps.current().unwrap(), None);
    // Two applies read the empty store; the first claims v1.
    assert_eq!(snaps.claim(None, "# winner\n").unwrap(), 1);
    let lost = snaps.claim(None, "# loser\n").unwrap_err();
    assert!(matches!(lost, ControlError::Surface(SurfaceError::ManifestVersionConflict(_))), "{lost}");
    assert!(lost.to_string().starts_with("ManifestVersionConflict"), "{lost}");
    assert_eq!(snaps.read(1).unwrap(), "# winner\n", "the applied version stays as claimed");
    assert_eq!(snaps.current().unwrap(), Some(1));
    // The loser reloads the winner and reapplies onto it as v2.
    let reloaded = snaps.read(snaps.current().unwrap().unwrap()).unwrap();
    assert_eq!(snaps.claim(Some(1), &format!("{reloaded}# loser\n")).unwrap(), 2);
    assert_eq!(snaps.read(2).unwrap(), "# winner\n# loser\n");
    assert_eq!(snaps.read(1).unwrap(), "# winner\n");
    // A version file left by an interrupted claim is never overwritten: the next claim skips past it.
    std::fs::write(dir.path().join("manifest@v3.toml"), "# orphan\n").unwrap();
    assert_eq!(snaps.claim(Some(2), "# next\n").unwrap(), 4);
    assert_eq!(std::fs::read_to_string(dir.path().join("manifest@v3.toml")).unwrap(), "# orphan\n");
}

/// A malformed pointer refuses the read rather than arming a version nobody applied.
#[test]
fn a_malformed_pointer_refuses_the_read() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("manifest@current"), "3 or so").unwrap();
    let e = SnapshotDir::open(dir.path()).current().unwrap_err();
    assert!(e.to_string().starts_with("ControlPointerMalformed"), "{e}");
}
