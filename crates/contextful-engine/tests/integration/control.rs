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

#[test]
fn an_attested_import_resumes_v1_only_after_receipt_validation() {
    let dir = tempfile::tempdir().unwrap();
    let snaps = SnapshotDir::open(dir.path());
    std::fs::write(dir.path().join("manifest@v1.toml"), "first").unwrap();
    let sign = |version, parent: Option<&str>| {
        assert_eq!((version, parent), (1, None));
        Ok("valid receipt".to_string())
    };
    let verify = |receipt: &str| {
        if receipt == "valid receipt" { Ok(()) } else { Err(ControlError::Storage("invalid receipt".into())) }
    };
    assert_eq!(snaps.import_attested("first", sign, verify).unwrap(), 1);
    std::fs::remove_file(dir.path().join("manifest@current")).unwrap();
    std::fs::write(dir.path().join("receipt@v1.json"), "forged receipt").unwrap();
    assert!(snaps.import_attested("first", sign, verify).is_err());
    assert_eq!(snaps.current().unwrap(), None);
    assert_eq!(std::fs::read_to_string(dir.path().join("receipt@v1.json")).unwrap(), "forged receipt");
    std::fs::write(dir.path().join("receipt@v1.json"), "valid receipt").unwrap();
    assert_eq!(snaps.import_attested("first", sign, verify).unwrap(), 1);
    assert!(snaps.import_attested("first", sign, verify).is_err());
    assert!(!dir.path().join("manifest@v2.toml").exists());
}
