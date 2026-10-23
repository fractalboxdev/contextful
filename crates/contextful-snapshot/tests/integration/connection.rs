//! `surface.connection` records held in the control snapshot directory.

use contextful_snapshot::{ConnectionRecord, ControlError, SnapshotDir};
use serde_json::{json, Value};

fn active() -> Value {
    json!({ "tenant": "tenant-a", "state": "active" })
}

#[test]
fn a_connection_claim_takes_the_next_version_and_a_declined_change_claims_none() {
    let dir = tempfile::tempdir().unwrap();
    let snaps = SnapshotDir::open(dir.path());
    assert_eq!(snaps.connection("conn-a").unwrap(), None);

    let (first, claimed) = snaps.claim_connection_guarded::<ControlError>("conn-a", |standing| {
        assert!(standing.is_none());
        Ok(Some(active()))
    }, &|| Ok(())).unwrap();
    assert!(claimed);
    assert_eq!(first, ConnectionRecord { version: 1, body: active() });

    let (standing, claimed) = snaps.claim_connection_guarded::<ControlError>("conn-a", |_| Ok(None), &|| Ok(())).unwrap();
    assert!(!claimed, "a declined change claims nothing");
    assert_eq!(standing, first);

    let (paused, claimed) = snaps.claim_connection_guarded::<ControlError>("conn-a", |standing| {
        let mut body = standing.unwrap().body.clone();
        body["state"] = json!("paused");
        Ok(Some(body))
    }, &|| Ok(())).unwrap();
    assert!(claimed);
    assert_eq!(paused.version, 2);
    assert_eq!(snaps.connection("conn-a").unwrap(), Some(paused));
}

#[test]
fn a_connection_claim_rechecks_authority_under_the_lock_and_a_refusal_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let snaps = SnapshotDir::open(dir.path());
    let refuse = || {
        let lock = std::fs::OpenOptions::new().read(true).write(true).open(dir.path().join("manifest.lock")).unwrap();
        assert!(lock.try_lock().is_err(), "the effect boundary holds the claim lock");
        Err(ControlError::Storage("authority revoked at commit".into()))
    };
    assert!(snaps.claim_connection_guarded::<ControlError>("conn-a", |_| Ok(Some(active())), &refuse).is_err());
    assert_eq!(snaps.connection("conn-a").unwrap(), None);

    let refused = snaps.claim_connection_guarded("conn-a", |_| Err(ControlError::Storage("declared elsewhere".into())), &|| Ok(()));
    assert_eq!(refused.unwrap_err(), ControlError::Storage("declared elsewhere".into()));
    assert_eq!(snaps.connection("conn-a").unwrap(), None);
}

#[test]
fn a_connection_id_outside_the_record_grammar_reaches_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let snaps = SnapshotDir::open(dir.path());
    for id in ["", "../escape", "a/b", &"x".repeat(65)] {
        assert!(snaps.connection(id).is_err(), "`{id}`");
        assert!(snaps.claim_connection_guarded::<ControlError>(id, |_| Ok(Some(active())), &|| Ok(())).is_err(), "`{id}`");
    }
    assert!(!dir.path().join("escape").exists());
}
