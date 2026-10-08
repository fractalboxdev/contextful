//! `surface.apply` and `surface.reconcile` over the local snapshot directory.

use contextful_core::surface::SurfaceError;
use contextful_engine::control::{ControlError, Draft, SnapshotDir};

#[test]
fn control_mutations_check_authority_under_the_lock_and_refuse_without_writes() {
    let dir = tempfile::tempdir().unwrap();
    let snaps = SnapshotDir::open(dir.path());
    snaps.claim_attested(None, "original", |_, _| Ok("first receipt".into())).unwrap();
    let draft = Draft::new(1, "edited".into(), "alice".into()).unwrap();
    let refuse = || {
        let lock = std::fs::OpenOptions::new().read(true).write(true).open(dir.path().join("manifest.lock")).unwrap();
        assert!(lock.try_lock().is_err(), "the effect boundary holds the mutation lock");
        Err(ControlError::Storage("authority revoked at commit".into()))
    };
    assert!(snaps.claim_attestation_nonce_guarded(&"a".repeat(32), 100, 100, &refuse).is_err());
    assert!(!dir.path().join("attestation-nonces").exists());
    assert!(snaps.save_draft_guarded(&draft, &refuse).is_err());
    assert!(!dir.path().join("manifest@draft.json").exists());
    snaps.save_draft(&draft).unwrap();
    assert!(snaps.claim_draft_guarded(&draft, &refuse).is_err());
    assert_eq!(snaps.current().unwrap(), Some(1));
    assert!(!dir.path().join("manifest@v2.toml").exists());
    assert_eq!(snaps.read_draft().unwrap(), draft);
    assert!(snaps.claim_draft_attested_guarded(&draft, |_, _| Ok("receipt".into()), &refuse).is_err());
    assert!(!dir.path().join("receipt@v2.json").exists());
    assert_eq!(snaps.current().unwrap(), Some(1));
}

#[test]
fn a_saved_draft_is_bound_to_its_editor_and_exact_nonce() {
    let dir = tempfile::tempdir().unwrap();
    let snaps = SnapshotDir::open(dir.path());
    snaps.import("# original\n").unwrap();
    let alice = Draft::new(1, "# alice\n".into(), "alice".into()).unwrap();
    snaps.save_draft(&alice).unwrap();
    let bob = Draft::new(1, "# bob\n".into(), "bob".into()).unwrap();
    snaps.save_draft(&bob).unwrap();
    assert!(matches!(snaps.claim_draft(&alice), Err(ControlError::Surface(SurfaceError::ManifestVersionConflict(_)))));
    assert_eq!(snaps.current().unwrap(), Some(1));
    assert_eq!(snaps.claim_draft(&bob).unwrap(), 2);
    assert_eq!(snaps.read(2).unwrap(), "# bob\n");
}

#[test]
fn an_attestation_nonce_survives_reopen_and_expires_after_its_signed_window() {
    let dir = tempfile::tempdir().unwrap();
    let first = SnapshotDir::open(dir.path());
    assert!(first.claim_attestation_nonce(&"a".repeat(32), 100, 100).unwrap());
    let reopened = SnapshotDir::open(dir.path());
    assert!(!reopened.claim_attestation_nonce(&"a".repeat(32), 100, 160).unwrap());
    assert!(reopened.claim_attestation_nonce(&"b".repeat(32), 161, 161).unwrap());
    assert!(!dir.path().join("attestation-nonces/100").exists());
}

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

#[test]
fn an_attested_draft_keeps_its_receipt_chain_and_refuses_failed_signing() {
    let dir = tempfile::tempdir().unwrap();
    let snaps = SnapshotDir::open(dir.path());
    snaps.import_attested("first", |_, _| Ok("receipt one".into()), |_| Ok(())).unwrap();
    let draft = Draft::new(1, "second".into(), "alice".into()).unwrap();
    snaps.save_draft(&draft).unwrap();
    assert!(snaps.claim_draft_attested(&draft, |_, _| Err(ControlError::Storage("signer absent".into()))).is_err());
    assert_eq!(snaps.current().unwrap(), Some(1));
    assert_eq!(snaps.read_draft().unwrap(), draft);
    assert_eq!(snaps.claim_draft_attested(&draft, |version, prior| {
        assert_eq!((version, prior), (2, Some("receipt one")));
        Ok("receipt two".into())
    }).unwrap(), 2);
    assert_eq!(std::fs::read_to_string(dir.path().join("receipt@v2.json")).unwrap(), "receipt two");
    assert_eq!(snaps.current().unwrap(), Some(2));
}
