//! `store.encrypt`: a declared key source resolves at startup or the store refuses to open.

use contextful_context::Store;
use contextful_core::store::StoreError;

fn store_with(config: &str) -> (tempfile::TempDir, contextful_context::Result<Store>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".contextful/context/research");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("config.toml"), config).unwrap();
    let opened = Store::open(dir.path(), "research");
    (dir, opened)
}

/// A `key_source` naming a binding the process lacks raises `StoreEncryptionKeyUnbound` at startup, with no cleartext fallback.
// spec: store.encrypt.key-unbound@417fe0f8
#[test]
fn an_unbound_key_source_refuses_to_open_the_store() {
    let var = "CONTEXTFUL_TEST_KEY_NEVER_SET_7F3A";
    assert!(std::env::var_os(var).is_none());
    let (_d, opened) = store_with(&format!("[encryption]\nkey_source = \"env:{var}\"\n"));
    match opened.unwrap_err().store() {
        Some(StoreError::StoreEncryptionKeyUnbound(m)) => assert!(m.contains(var), "{m}"),
        other => panic!("expected StoreEncryptionKeyUnbound, got {other:?}"),
    }
    let (_d, opened) = store_with("[encryption]\nkey_source = \"kms:projects/p/keys/k\"\n");
    assert!(matches!(opened.unwrap_err().store(), Some(StoreError::StoreEncryptionKeyUnbound(_))));

    // Encryption is off unless declared, and an unknown configuration key refuses.
    let (_d, opened) = store_with("[node]\nid = \"ingest-a\"\n");
    assert_eq!(opened.unwrap().configured_node_id().as_deref(), Some("ingest-a"));
    let (_d, opened) = store_with("[encryption]\nkey = \"literal\"\n");
    assert!(opened.is_err());
}

/// A bound key source refuses too: this build links no at-rest cipher and writes no cleartext in its place.
#[test]
fn a_bound_key_source_refuses_rather_than_write_cleartext() {
    let (_d, opened) = store_with("[encryption]\nkey_source = \"env:PATH\"\n");
    let err = opened.unwrap_err();
    assert!(err.store().is_none() && err.to_string().contains("no at-rest cipher"), "{err}");
}

/// Files under `dir` other than the store's `config.toml`, at any depth.
fn written(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().map(Result::unwrap) {
        let p = e.path();
        if p.is_dir() {
            out.extend(written(&p));
        } else if p.file_name().is_some_and(|n| n != "config.toml") {
            out.push(p);
        }
    }
    out
}

#[test]
fn a_declared_encryption_opens_no_store_and_writes_no_file() {
    // The walk sees what a store writes: an undeclared store opens and leaves files.
    let (d, opened) = store_with("[node]\nid = \"ingest-a\"\n");
    drop(opened.unwrap());
    std::fs::write(d.path().join(".contextful/context/research/probe.parquet"), b"PAR1").unwrap();
    assert!(!written(d.path()).is_empty());

    let configs = [
        "[encryption]\nkey_source = \"env:CONTEXTFUL_TEST_KEY_NEVER_SET_7F3A\"\n",
        "[encryption]\nkey_source = \"kms:projects/p/keys/k\"\n",
        "[encryption]\nkey_source = \"env:PATH\"\n",
        "[encryption]\nkey = \"literal\"\n",
    ];
    let (mut opens, mut files) = (0u64, 0u64);
    for config in configs {
        let (d, opened) = store_with(config);
        opens += u64::from(opened.is_ok());
        drop(opened);
        files += written(d.path()).len() as u64;
    }
    contextful_eval::record::emit("encrypt-declared-refuses", (opens + files) as f64, configs.len() as u64, 0);
    assert_eq!((opens, files), (0, 0));
}
