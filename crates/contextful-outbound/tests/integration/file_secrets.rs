//! The `file` adapter: answers by file, versions by digest, and the one conditional write half.

use crate::support::{name, SetClock, T0};
use contextful_core::connector::record::{Observation, Observe};
use contextful_core::connector::reference::Hydrated;
use contextful_core::connector::resolve::Provider;
use contextful_core::connector::rotate::{PutError, VersionedStore};
use contextful_core::run::journal::sha256_hex;
use contextful_outbound::file_secrets::FileSecrets;
use contextful_outbound::secrets::{assemble_all, Rotator};
use std::collections::BTreeMap;
use std::sync::Arc;

fn vars(dir: &std::path::Path) -> BTreeMap<String, String> {
    [("CONTEXTFUL_SECRETS_BACKEND", "file"), ("CONTEXTFUL_SECRETS_DIR", dir.to_str().unwrap())].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// The `file` backend answers a name with its file's bytes less one trailing newline,
/// versioned by their SHA-256; an absent file is a miss.
// spec: connector.resolve.file-backend@bc0740f8
#[test]
fn the_file_backend_answers_by_file_and_versions_by_digest() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("vendor-refresh"), "refresh-0\n").unwrap();
    let files = FileSecrets::new(dir.path());
    let answer = files.answer(&name("vendor-refresh")).unwrap().expect("the file answers");
    assert_eq!(answer.value.reveal(), "refresh-0");
    assert_eq!(answer.version.as_deref(), Some(sha256_hex(b"refresh-0\n").as_str()));
    assert!(files.answer(&name("vendor-other")).unwrap().is_none(), "an absent file is a miss");
    match files.observe(&name("vendor-refresh"), crate::support::at(T0)) {
        Observation::Observed { present, version, .. } => assert!(present && version == answer.version),
        other => panic!("{other:?}"),
    }
    let a = assemble_all(&vars(dir.path()), Arc::new(SetClock::new())).unwrap();
    assert_eq!(a.resolver.hydrate(&name("vendor-refresh")).unwrap().reveal(), "refresh-0");
    assert_eq!(a.resolver.attribution().get("vendor-refresh").map(String::as_str), Some("file"));
    let unset = assemble_all(&[("CONTEXTFUL_SECRETS_BACKEND".to_string(), "file".to_string())].into(), Arc::new(SetClock::new())).err().unwrap();
    assert!(unset.message.contains("CONTEXTFUL_SECRETS_DIR"), "{unset}");
}

/// A put compares the file's digest with the hydrated version under a lock, replaces it whole
/// with owner-only permissions, refuses a read-only file, and loses to a newer version.
// spec: connector.rotate.file-put@1dc0acce
// spec: connector.principal-secrets.adapter-port@4180127c
#[test]
fn a_file_put_replaces_the_hydrated_version_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vendor-refresh");
    std::fs::write(&path, "refresh-0").unwrap();
    let files = FileSecrets::new(dir.path());
    let v0 = files.answer(&name("vendor-refresh")).unwrap().unwrap().version.unwrap();
    let v1 = files.put(&name("vendor-refresh"), &Hydrated::new("refresh-1"), Some(&v0), None).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "refresh-1");
    assert_eq!(v1, sha256_hex(b"refresh-1"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    // A put conditioned on the superseded version loses and names the stored one.
    assert_eq!(files.put(&name("vendor-refresh"), &Hydrated::new("late"), Some(&v0), None), Err(PutError::Conflict(v1.clone())));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "refresh-1");
    // A file without write permission refuses the put, whatever the process may write.
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&path, perms).unwrap();
    match files.put(&name("vendor-refresh"), &Hydrated::new("refresh-2"), Some(&v1), None) {
        Err(PutError::Denied(why)) => assert!(!why.contains("refresh-2"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "refresh-1");
    let leftovers: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| n.ends_with(".tmp")).collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

/// Of the linked adapters only `file` carries a write method.
// spec: connector.rotate.write-halves@5da5b48d
#[test]
fn only_the_file_adapter_carries_a_write_half() {
    let dir = tempfile::tempdir().unwrap();
    let mut v = vars(dir.path());
    v.insert("CONTEXTFUL_SECRETS_BACKEND".into(), "env,file".into());
    let a = assemble_all(&v, Arc::new(SetClock::new())).unwrap();
    assert_eq!(a.writers.keys().map(String::as_str).collect::<Vec<_>>(), ["file"]);
    assert!(assemble_all(&BTreeMap::new(), Arc::new(SetClock::new())).unwrap().writers.is_empty());
}

/// Two resolvers refreshing one name serialize through the stored version: one put wins, and
/// the other rehydrates the winner without raising.
// spec: connector.principal-secrets.write-serialization@ff9573e4
#[test]
fn two_resolvers_refreshing_one_file_resolve_one_version() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("vendor-refresh"), "refresh-0").unwrap();
    let rig = || {
        let a = assemble_all(&vars(dir.path()), Arc::new(SetClock::new())).unwrap();
        let resolver = Arc::new(a.resolver);
        resolver.hydrate(&name("vendor-refresh")).unwrap();
        (resolver.clone(), Rotator::new(resolver, a.writers))
    };
    let ((first, a), (second, b)) = (rig(), rig());
    let racers = [(a, "refresh-a"), (b, "refresh-b")];
    let outcomes: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = racers.iter().map(|(r, v)| s.spawn(move || r.rotate(&name("vendor-refresh"), |_| Ok(Some((Hydrated::new(*v), None)))))).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(outcomes.len(), 2);
    assert!(outcomes.iter().all(Result::is_ok), "{outcomes:?}");
    let stored = std::fs::read_to_string(dir.path().join("vendor-refresh")).unwrap();
    assert!(stored == "refresh-a" || stored == "refresh-b", "{stored}");
    for resolver in [&first, &second] {
        assert_eq!(resolver.hydrate(&name("vendor-refresh")).unwrap().reveal(), stored, "each resolver reads the one winning version");
    }
}
