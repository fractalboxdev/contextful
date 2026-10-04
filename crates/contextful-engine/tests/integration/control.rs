//! `surface.apply` and `surface.reconcile` over the local snapshot directory.

use contextful_core::surface::SurfaceError;
use contextful_engine::control::{ControlError, SnapshotDir};

#[test]
fn adoption_publishes_a_verified_chain_with_the_pointer_last() {
    let dir = tempfile::tempdir().unwrap();
    let snaps = SnapshotDir::open(dir.path());
    let chain = [(1, b"first".to_vec(), b"receipt one".to_vec()), (3, b"third".to_vec(), b"receipt three".to_vec())];
    snaps.adopt(None, &chain, 3).unwrap();
    assert_eq!(snaps.current().unwrap(), Some(3));
    assert_eq!(snaps.read(1).unwrap(), "first");
    assert_eq!(snaps.read(3).unwrap(), "third");
    assert_eq!(std::fs::read(dir.path().join("receipt@v3.json")).unwrap(), b"receipt three");
    assert!(!dir.path().join("manifest@v2.toml").exists());
}

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

/// A signed claim writes its receipt before the pointer and passes the predecessor receipt
/// to the next claim under the same lock.
// spec: surface.apply.receipt-file@4fca09c6
#[test]
fn an_attested_claim_commits_the_snapshot_and_receipt_together() {
    let dir = tempfile::tempdir().unwrap();
    let snaps = SnapshotDir::open(dir.path());
    let failed = snaps.claim_attested(None, "first", |_, _| Err(ControlError::Storage("signing failed".into())));
    assert!(failed.unwrap_err().to_string().contains("signing failed"));
    assert_eq!(snaps.current().unwrap(), None);
    assert!(!dir.path().join("manifest@v1.toml").exists());

    assert_eq!(snaps.claim_attested(None, "first", |version, parent| {
        assert_eq!(version, 1);
        assert_eq!(parent, None);
        Ok("first receipt".into())
    }).unwrap(), 1);
    assert_eq!(std::fs::read_to_string(dir.path().join("receipt@v1.json")).unwrap(), "first receipt");
    assert_eq!(snaps.current().unwrap(), Some(1));

    assert_eq!(snaps.claim_attested(Some(1), "second", |version, parent| {
        assert_eq!(version, 2);
        assert_eq!(parent, Some("first receipt"));
        Ok("second receipt".into())
    }).unwrap(), 2);
    assert_eq!(snaps.read(2).unwrap(), "second");
    assert_eq!(std::fs::read_to_string(dir.path().join("receipt@v2.json")).unwrap(), "second receipt");
}

/// A receipt publication error leaves no snapshot file for the version it refused.
#[test]
fn a_failed_receipt_write_removes_its_unpublished_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let snaps = SnapshotDir::open(dir.path());
    let refused = snaps.claim_attested(None, "first", |version, _| {
        std::fs::create_dir(dir.path().join(format!("receipt@v{version}.json"))).unwrap();
        Ok("receipt".into())
    });
    assert!(refused.is_err());
    assert_eq!(snaps.current().unwrap(), None);
    assert!(!dir.path().join("manifest@v1.toml").exists());
}
