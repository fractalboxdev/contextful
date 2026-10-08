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

/// A key source holding fewer than 32 key bytes refuses before any store write.
#[test]
fn a_short_bound_key_source_refuses_rather_than_write_cleartext() {
    let (_d, opened) = store_with("[encryption]\nkey_source = \"env:PATH\"\n");
    let err = opened.unwrap_err();
    assert!(err.store().is_none() && err.to_string().contains("32 key bytes"), "{err}");
}

#[test]
fn an_unencrypted_store_exposes_no_catalog_cipher_binding() {
    let (_dir, opened) = store_with("[node]\nid = \"ingest-a\"\n");
    assert!(!opened.unwrap().encrypted());
}

/// A sidecar file carries a fresh wrapped data key and decrypts only under its project key.
#[test]
fn an_aes_gcm_file_round_trips_without_plaintext_or_key_reuse() {
    use contextful_context::encrypt::AesGcmFileCipher;
    use contextful_core::store::encrypt::FileCipher;

    let cipher = AesGcmFileCipher::new([0x37; 32], 3);
    let canary = b"ledger-canary-5f1e unique plaintext marker";
    let first = cipher.seal(canary).unwrap();
    let second = cipher.seal(canary).unwrap();
    assert_ne!(first, second);
    assert!(!first.windows(canary.len()).any(|part| part == canary));
    assert_eq!(cipher.key_version(), 3);
    assert_eq!(cipher.open(&first).unwrap(), canary);
    assert_eq!(cipher.open(&second).unwrap(), canary);
    assert!(AesGcmFileCipher::new([0x42; 32], 3).open(&first).is_err());
    let mut tampered = first;
    *tampered.last_mut().unwrap() ^= 1;
    assert!(cipher.open(&tampered).is_err());
}

#[test]
fn sealed_metadata_files_round_trip_without_plaintext_or_fallback() {
    use contextful_context::encrypt::{AesGcmFileCipher, MetadataFiles};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("_manifest.json");
    let canary = b"metadata-canary-5f1e";
    let value = format!("{{\"marker\":\"{}\"}}", std::str::from_utf8(canary).unwrap());
    let key = AesGcmFileCipher::new([0x37; 32], 1);
    let files = MetadataFiles::sealed(&key);
    assert!(files.create_new(&path, value.as_bytes()).unwrap());
    assert!(!files.create_new(&path, b"other").unwrap());
    assert_eq!(files.read(&path).unwrap(), value.as_bytes());
    let disk = std::fs::read(&path).unwrap();
    assert!(!disk.windows(canary.len()).any(|part| part == canary));
    assert!(MetadataFiles::sealed(&AesGcmFileCipher::new([0x42; 32], 1)).read(&path).is_err());
    std::fs::write(&path, b"CFSEAL01").unwrap();
    assert!(files.read(&path).is_err());
    std::fs::write(&path, &disk).unwrap();
    assert!(MetadataFiles::plaintext().read(&path).is_ok_and(|bytes| bytes == disk));
    files.replace(&path, b"{\"marker\":\"next\"}").unwrap();
    assert_eq!(files.read(&path).unwrap(), b"{\"marker\":\"next\"}");
}

#[cfg(feature = "read")]
#[test]
fn a_ledger_row_registers_in_duckdb_memory_without_a_plaintext_file() {
    use contextful_context::ledger::register_memory;
    use contextful_core::store::ledger::RequestRecord;
    use contextful_core::time::Instant;

    let dir = tempfile::tempdir().unwrap();
    let spill_dir = dir.path().join("spill");
    let config = duckdb::Config::default().with("temp_directory", spill_dir.to_str().unwrap()).unwrap();
    let db = duckdb::Connection::open_in_memory_with_flags(config).unwrap();
    let canary = "ledger-sql-canary-5f1e";
    let record = RequestRecord {
        request_id: canary.into(), vendor_request_id: None, connector: "remote".into(),
        method: "POST".into(), url_host: "example.test".into(), status_code: Some(201),
        started_at: Instant::parse("2030-01-01T00:00:00Z").unwrap(), duration_ms: 7, batch_seq: Some(2),
    };
    register_memory(&db, "ledger_rows", &[("run-1".into(), record)]).unwrap();
    let spill: String = db.query_row("SELECT current_setting('temp_directory')", [], |row| row.get(0)).unwrap();
    assert!(spill.is_empty(), "decoded ledger rows can spill into {spill}");
    let id: String = db.query_row("SELECT request_id FROM ledger_rows WHERE batch_seq = 2", [], |r| r.get(0)).unwrap();
    assert_eq!(id, canary);
    db.execute_batch("SET lock_configuration = true").unwrap();
    register_memory(&db, "empty_rows", &[]).unwrap();
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
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

/// A cipher for the sealed-sidecar path: a tagged, key-dependent byte transform that
/// refuses a file another key sealed.
struct TestCipher {
    key: u8,
}

impl contextful_core::store::encrypt::FileCipher for TestCipher {
    fn key_version(&self) -> u32 {
        7
    }

    fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, contextful_core::store::encrypt::SealError> {
        let mut out = b"SEALED".to_vec();
        out.push(self.key);
        out.extend(plaintext.iter().enumerate().map(|(i, b)| b ^ self.key ^ (i as u8).wrapping_mul(31)));
        Ok(out)
    }

    fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, contextful_core::store::encrypt::SealError> {
        match sealed.strip_prefix(b"SEALED".as_slice()) {
            Some([key, body @ ..]) if *key == self.key => Ok(body.iter().enumerate().map(|(i, b)| b ^ self.key ^ (i as u8).wrapping_mul(31)).collect()),
            _ => Err(contextful_core::store::encrypt::SealError("sealed under another key".into())),
        }
    }
}

/// Every file under `dir`, with its bytes.
fn tree(dir: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            out.extend(tree(&p));
        } else {
            let bytes = std::fs::read(&p).unwrap();
            out.push((p, bytes));
        }
    }
    out.sort();
    out
}

/// In an unencrypted project a reader memory-maps a sidecar read-only; in an encrypted one it decrypts each sealed sidecar file into anonymous process memory and writes no cleartext to disk.
// spec: store.encrypt.sidecar-reader@70509f80
#[test]
fn a_plaintext_sidecar_is_mapped_and_a_sealed_one_opens_into_memory_alone() {
    use arrow_array::builder::{FixedSizeListBuilder, Float32Builder};
    use arrow_array::{ArrayRef, RecordBatch, StringArray};
    use contextful_context::vector::{build, Fallback, Sealing, VectorSidecar, GRAPH_FILE};
    use contextful_core::store::declare::TableDecl;
    use contextful_core::store::lay_out::SnapshotId;
    use contextful_core::time::Instant;
    use std::sync::Arc;

    let canary = "passage-canary-5f1e";
    let ids: Vec<String> = (0..40).map(|i| if i == 17 { canary.to_string() } else { format!("passage-{i}") }).collect();
    let mut vectors = FixedSizeListBuilder::new(Float32Builder::new(), 4);
    for i in 0..40 {
        let v = if i == 17 { [0.0, 0.0, 0.0, 1.0] } else { [1.0, i as f32 / 40.0, 0.5, 0.0] };
        vectors.values().append_slice(&v);
        vectors.append(true);
    }
    let rows = RecordBatch::try_from_iter([
        ("passage_id", Arc::new(StringArray::from(ids.clone())) as ArrayRef),
        ("embedding", Arc::new(vectors.finish()) as ArrayRef),
    ])
    .unwrap();
    let decl = TableDecl::parse_pipeline(
        "[[pipeline.tables]]\nname = \"passages\"\nprimary_key = [\"passage_id\"]\n[[pipeline.tables.indexes]]\nkind = \"vector\"\ncolumn = \"embedding\"\nmodel = \"e5\"\ndim = 4\n",
    )
    .unwrap()
    .remove(0);
    let id = SnapshotId::next(Instant::parse("2030-01-01T00:00:00Z").unwrap(), None);
    let query = [0.0, 0.1, 0.0, 1.0];

    // Unencrypted: plaintext files, mapped read-only.
    let plain = tempfile::tempdir().unwrap();
    let entry = build(plain.path(), &id, &rows, &decl, &decl.indexes()[0], &Sealing::Plaintext).unwrap();
    assert_eq!(entry.key_version, 0);
    let entry = contextful_core::store::index::IndexEntry::from(entry);
    let mapped = VectorSidecar::open(plain.path(), "passages", &entry, &Sealing::Plaintext).unwrap();
    assert!(mapped.is_mapped());
    assert_eq!(mapped.probe(&query, 1).unwrap()[0].id, canary);
    let graph = tree(plain.path()).into_iter().find(|(p, _)| p.ends_with(GRAPH_FILE)).unwrap().1;
    assert!(graph.windows(canary.len()).any(|w| w == canary.as_bytes()), "the plaintext graph carries its identifiers");

    // Encrypted: every sidecar file sealed, opened into memory, nothing written.
    let cipher = TestCipher { key: 0x5a };
    let sealed = tempfile::tempdir().unwrap();
    let entry = build(sealed.path(), &id, &rows, &decl, &decl.indexes()[0], &Sealing::Sealed(&cipher)).unwrap();
    assert_eq!(entry.key_version, 7);
    let entry = contextful_core::store::index::IndexEntry::from(entry);
    let files = tree(sealed.path());
    assert_eq!(files.len(), 2, "{files:?}");
    for (path, bytes) in &files {
        assert!(bytes.starts_with(b"SEALED"), "{} is not sealed", path.display());
        for cleartext in [canary.as_bytes(), b"CFHNSW".as_slice(), b"passage_id".as_slice(), &1.0f32.to_le_bytes()] {
            assert!(!bytes.windows(cleartext.len()).any(|w| w == cleartext), "{} carries cleartext {cleartext:?}", path.display());
        }
    }
    let opened = VectorSidecar::open(sealed.path(), "passages", &entry, &Sealing::Sealed(&cipher)).unwrap();
    assert!(!opened.is_mapped());
    assert_eq!(opened.probe(&query, 1).unwrap()[0].id, canary);
    assert_eq!(tree(sealed.path()), files, "opening wrote to disk");

    // A sealed sidecar never opens as plaintext, nor under another key, nor a plaintext one
    // under the cipher.
    assert_eq!(VectorSidecar::open(sealed.path(), "passages", &entry, &Sealing::Plaintext).err(), Some(Fallback::Unreadable));
    let other = TestCipher { key: 0x11 };
    assert_eq!(VectorSidecar::open(sealed.path(), "passages", &entry, &Sealing::Sealed(&other)).err(), Some(Fallback::Unreadable));
    let plain_entry = contextful_core::store::index::IndexEntry::from(build(plain.path(), &id, &rows, &decl, &decl.indexes()[0], &Sealing::Plaintext).unwrap());
    assert_eq!(VectorSidecar::open(plain.path(), "passages", &plain_entry, &Sealing::Sealed(&cipher)).err(), Some(Fallback::Unreadable));
}

/// Staged rows and a declaration for a full-text sidecar whose one `canary` row alone holds
/// the term `zephyrine`.
fn fulltext_rows(canary: &str) -> (arrow_array::RecordBatch, contextful_core::store::declare::TableDecl) {
    use arrow_array::{ArrayRef, RecordBatch, StringArray};
    use std::sync::Arc;
    let ids: Vec<String> = (0..40).map(|i| if i == 17 { canary.to_string() } else { format!("passage-{i}") }).collect();
    let bodies: Vec<String> = (0..40).map(|i| if i == 17 { "the zephyrine ledger".to_string() } else { format!("routine entry {i}") }).collect();
    let rows = RecordBatch::try_from_iter([
        ("passage_id", Arc::new(StringArray::from(ids)) as ArrayRef),
        ("body", Arc::new(StringArray::from(bodies)) as ArrayRef),
    ])
    .unwrap();
    let decl = contextful_core::store::declare::TableDecl::parse_pipeline(
        "[[pipeline.tables]]\nname = \"passages\"\nprimary_key = [\"passage_id\"]\n[[pipeline.tables.indexes]]\nkind = \"fulltext\"\ncolumn = \"body\"\n",
    )
    .unwrap()
    .remove(0);
    (rows, decl)
}

/// A full-text sidecar is mapped from plaintext and, sealed, opens into process memory alone.
#[test]
fn a_sealed_full_text_sidecar_opens_into_memory_alone() {
    use contextful_context::fulltext::{build, FulltextSidecar};
    use contextful_context::vector::{Fallback, Sealing};
    use contextful_core::store::lay_out::SnapshotId;
    use contextful_core::time::Instant;

    let canary = "passage-canary-5f1e";
    let (rows, decl) = fulltext_rows(canary);
    let id = SnapshotId::next(Instant::parse("2030-01-01T00:00:00Z").unwrap(), None);
    let query = vec!["zephyrine".to_string()];

    let plain = tempfile::tempdir().unwrap();
    let entry = contextful_core::store::index::IndexEntry::from(build(plain.path(), &id, &rows, &decl, &decl.indexes()[0], &Sealing::Plaintext).unwrap());
    let mapped = FulltextSidecar::open(plain.path(), "passages", &entry, &Sealing::Plaintext).unwrap();
    assert!(mapped.is_mapped());
    assert_eq!(mapped.probe(&query, 1).unwrap().candidates[0].id, canary);

    let cipher = TestCipher { key: 0x5a };
    let sealed = tempfile::tempdir().unwrap();
    let built = build(sealed.path(), &id, &rows, &decl, &decl.indexes()[0], &Sealing::Sealed(&cipher)).unwrap();
    assert_eq!(built.key_version, 7);
    let entry = contextful_core::store::index::IndexEntry::from(built);
    let files = tree(sealed.path());
    assert_eq!(files.len(), 2, "{files:?}");
    for (path, bytes) in &files {
        assert!(bytes.starts_with(b"SEALED"), "{} is not sealed", path.display());
        for cleartext in [canary.as_bytes(), b"CFPOST01".as_slice(), b"zephyrine".as_slice(), b"passage_id".as_slice()] {
            assert!(!bytes.windows(cleartext.len()).any(|w| w == cleartext), "{} carries cleartext {cleartext:?}", path.display());
        }
    }
    let opened = FulltextSidecar::open(sealed.path(), "passages", &entry, &Sealing::Sealed(&cipher)).unwrap();
    assert!(!opened.is_mapped());
    assert_eq!(opened.probe(&query, 1).unwrap().candidates[0].id, canary);
    assert_eq!(tree(sealed.path()), files, "opening wrote to disk");
    assert_eq!(FulltextSidecar::open(sealed.path(), "passages", &entry, &Sealing::Plaintext).err(), Some(Fallback::Unreadable));
    assert_eq!(FulltextSidecar::open(sealed.path(), "passages", &entry, &Sealing::Sealed(&TestCipher { key: 0x11 })).err(), Some(Fallback::Unreadable));
}

/// A real file cipher seals each full-text sidecar file and opens its postings in memory.
#[test]
fn an_aes_gcm_sidecar_has_zero_plaintext_canary_hits() {
    use contextful_context::encrypt::AesGcmFileCipher;
    use contextful_context::fulltext::{build, FulltextSidecar};
    use contextful_context::vector::Sealing;
    use contextful_core::store::lay_out::SnapshotId;
    use contextful_core::time::Instant;

    let canary = "passage-canary-5f1e";
    let (rows, decl) = fulltext_rows(canary);
    let id = SnapshotId::next(Instant::parse("2030-01-01T00:00:00Z").unwrap(), None);
    let cipher = AesGcmFileCipher::new([0x37; 32], 3);
    let store = tempfile::tempdir().unwrap();
    let entry = build(store.path(), &id, &rows, &decl, &decl.indexes()[0], &Sealing::Sealed(&cipher)).unwrap();
    assert_eq!(entry.key_version, 3);
    let files = tree(store.path());
    assert_eq!(files.len(), 2);
    let hits: usize = files
        .iter()
        .map(|(_, bytes)| bytes.windows(canary.len()).filter(|part| *part == canary.as_bytes()).count())
        .sum();
    assert_eq!(hits, 0);
    let entry = contextful_core::store::index::IndexEntry::from(entry);
    let opened = FulltextSidecar::open(store.path(), "passages", &entry, &Sealing::Sealed(&cipher)).unwrap();
    assert_eq!(opened.probe(&["zephyrine".into()], 1).unwrap().candidates[0].id, canary);
    assert_eq!(tree(store.path()), files);
}

/// Parquet modular encryption covers its columns and footer, and Arrow reads only with the key.
#[test]
fn encrypted_parquet_has_no_plaintext_canary_and_decrypts() {
    use arrow_array::{ArrayRef, RecordBatch, StringArray};
    use contextful_context::parquet_io;
    use std::sync::Arc;

    let canary = "parquet-canary-5f1e unique plaintext marker";
    let batch = RecordBatch::try_from_iter([("body", Arc::new(StringArray::from(vec![canary])) as ArrayRef)]).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("part.parquet");
    let key = [0x37; 16];
    parquet_io::write_encrypted(&path, &batch, &key).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert!(!bytes.windows(canary.len()).any(|part| part == canary.as_bytes()));
    assert!(!bytes.starts_with(b"PAR1"), "the footer is encrypted");
    assert!(parquet_io::read(&path).is_err());
    assert!(parquet_io::read_encrypted(&path, &[0x42; 16]).is_err());
    let opened = parquet_io::read_encrypted(&path, &key).unwrap();
    assert_eq!(opened.len(), 1);
    let body = opened[0].column_by_name("body").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
    assert_eq!(body.value(0), canary);

    #[cfg(feature = "read")]
    {
        let db = duckdb::Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA add_parquet_key('test', '7777777777777777')").unwrap();
        let sql = format!("SELECT body FROM read_parquet('{}', encryption_config = {{footer_key: 'test'}})", path.display());
        let read: String = db.query_row(&sql, [], |row| row.get(0)).unwrap();
        assert_eq!(read, canary);
    }
}

/// Persistent adapters share the opened store's cipher without receiving key material.
#[test]
fn a_bound_store_shares_one_opaque_cipher_with_persistent_adapters() {
    let key_var = "CONTEXTFUL_TEST_KEY_74_SHARED_BINDING";
    unsafe { std::env::set_var(key_var, "0123456789abcdef0123456789abcdef") };
    let (_dir, opened) = store_with(&format!("[encryption]\nkey_source = \"env:{key_var}\"\n"));
    let store = opened.unwrap();
    let cipher = store.file_cipher().unwrap();
    let second = store.file_cipher().unwrap();
    assert!(std::sync::Arc::ptr_eq(&cipher, &second));
    let sealed = cipher.seal(b"opaque-store-cipher-74").unwrap();
    assert_eq!(second.open(&sealed).unwrap(), b"opaque-store-cipher-74");
    assert!(store.encrypted());
    let (_plain_dir, plain) = store_with("");
    assert!(plain.unwrap().file_cipher().is_none());
}

/// A bound project key opens a store, and a landed row stays encrypted through a store read.
#[test]
fn a_bound_store_lands_ciphertext_and_reads_its_row() {
    use contextful_context::land::{Batch, RunContext};
    use contextful_context::rows::table_rows;
    use contextful_core::store::declare::TableDecl;
    use contextful_core::store::lay_out::NodeId;
    use contextful_core::store::reserve::Injection;
    use contextful_core::time::Instant;
    use serde_json::json;

    let key_var = "CONTEXTFUL_TEST_KEY_74_BOUND";
    // This unique variable is read only by this test's store; no other test changes it.
    unsafe { std::env::set_var(key_var, "0123456789abcdef0123456789abcdef") };
    let (dir, opened) = store_with(&format!("[encryption]\nkey_source = \"env:{key_var}\"\n"));
    let store = opened.unwrap();
    let manifest = "[[pipeline.tables]]\nname = \"documents\"\nprimary_key = [\"doc_id\"]\n";
    let decl = TableDecl::parse_pipeline(manifest).unwrap().remove(0);
    let canary = "row-canary-5f1e unique plaintext marker";
    let batch = Batch { rows: vec![json!({"doc_id": "d1", "body": canary}).as_object().unwrap().clone()], types: Default::default() };
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: "run-1".into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, taint: None },
        committed_at: Instant::parse("2030-01-01T00:00:00Z").unwrap(),
    };
    contextful_context::land::land_run(&store, &decl, std::slice::from_ref(&batch), &ctx, &Default::default(), &|| Ok(())).unwrap();
    let next = Batch { rows: vec![json!({"doc_id": "d2", "body": canary, "extra": 99}).as_object().unwrap().clone()], types: [("extra".into(), contextful_core::store::reconcile::ColumnType::Float64)].into() };
    let next_ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: "run-2".into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, taint: None },
        committed_at: Instant::parse("2030-01-02T00:00:00Z").unwrap(),
    };
    contextful_context::land::land_run(&store, &decl, std::slice::from_ref(&next), &next_ctx, &Default::default(), &|| Ok(())).unwrap();
    let files = written(&dir.path().join(".contextful/context/research"));
    assert!(!files.is_empty());
    for path in &files {
        let bytes = std::fs::read(path).unwrap();
        assert!(!bytes.windows(canary.len()).any(|part| part == canary.as_bytes()), "{} holds plaintext", path.display());
    }
    let rows = table_rows(&store, &decl, &["doc_id", "body"]).unwrap();
    assert_eq!(rows[0]["body"], canary);
    #[cfg(feature = "read")]
    {
        use contextful_context::read::{Face, ReadOptions};
        use contextful_core::store::bound_time::Bounds;
        use contextful_policy::enforce::mask::Pepper;
        use contextful_policy::enforce::session::Request;

        let credentials = crate::read::Reads::new();
        let authority = credentials.authority(crate::read::loop_subject("agent://encrypted-reader"), vec![crate::read::read(&["documents"], None)]);
        let face = Face::open(store.clone(), "", Pepper::resolve(|_| Some("encrypted-reader-pepper".into()))).unwrap();
        assert_eq!(face.operator_query("SELECT * FROM documents ORDER BY doc_id LIMIT 500 OFFSET 0", ReadOptions::default()).unwrap().rows.len(), 2);
        let model = r#"
[[model]]
id = "encrypted_copy"
sql = "SELECT doc_id, body, extra FROM documents"
unique_key = ["doc_id"]
[model.contract]
version = "1.0.0"
columns = [{ name = "doc_id", type = "utf8", nullable = false }, { name = "body", type = "utf8", nullable = false }, { name = "extra", type = "float64", nullable = true }]
[model.freshness]
max_lag = "1d"
"#;
        crate::read::build_model(&face, model, "encrypted_copy", "2030-01-03T00:00:00Z");
        let manifests = contextful_context::build::manifests(&store, "encrypted_copy").unwrap();
        assert_eq!(manifests.len(), 1);
        assert!(manifests[0].parts.iter().all(|part| part.key_version == 1));
        let id = manifests[0].publish.as_ref().unwrap().build_id.clone();
        let now = Instant::parse("2030-01-03T00:00:00Z").unwrap();
        contextful_context::build::hold(&store, "encrypted_copy", &id, canary, 60, now).unwrap();
        assert_eq!(contextful_context::build::holds(&store, "encrypted_copy").unwrap()[0].principal, canary);
        contextful_context::build::write_logs(&store, "encrypted_copy").unwrap();
        assert_eq!(face.operator_query("SELECT * FROM encrypted_copy", ReadOptions::default()).unwrap().rows.len(), 2);
        let session = face.session(&authority, &Request::default(), Bounds::default()).unwrap();
        assert_eq!(face.query(&session, "SELECT * FROM documents ORDER BY doc_id LIMIT 500 OFFSET 0", ReadOptions::default()).unwrap().rows.len(), 2);
        for _ in 0..2 {
            let response = face.query(&session, "SELECT body, extra FROM documents ORDER BY doc_id", ReadOptions::default()).unwrap();
            assert_eq!(crate::read::column(&response, "body"), vec![json!(canary), json!(canary)]);
            assert_eq!(crate::read::column(&response, "extra"), vec![json!(null), json!(99.0)]);
        }
    }
    for path in written(&dir.path().join(".contextful/context/research")) {
        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.windows(canary.len()).any(|part| part == canary.as_bytes()), "{} holds plaintext after model publication", path.display());
    }
}

#[test]
fn a_fixed_seed_store_has_zero_plaintext_hits_and_decrypts_every_payload() {
    use contextful_context::land::{land, Batch, RunContext};
    use contextful_context::ledger;
    use contextful_context::rows::table_rows;
    use contextful_core::store::declare::TableDecl;
    use contextful_core::store::lay_out::NodeId;
    use contextful_core::store::ledger::RequestRecord;
    use contextful_core::store::reserve::Injection;
    use contextful_core::time::Instant;
    use serde_json::json;
    use sha2::{Digest, Sha256};

    const CANARY: &str = "encrypt-fixed-seed-74-5f1e";
    let key_var = "CONTEXTFUL_TEST_KEY_74_WHOLE_STORE";
    unsafe { std::env::set_var(key_var, "0123456789abcdef0123456789abcdef") };
    let (dir, opened) = store_with(&format!("[encryption]\nkey_source = \"env:{key_var}\"\n"));
    let store = opened.unwrap();
    let root = dir.path().join(".contextful/context/research");
    let decl = TableDecl::parse_pipeline("[[pipeline.tables]]\nname = \"documents\"\n").unwrap().remove(0);
    let node = NodeId::parse("ingest-a").unwrap();
    let at = Instant::parse("2030-01-01T00:00:00Z").unwrap();
    let batch = Batch { rows: vec![json!({"doc_id": "d1", "body": CANARY}).as_object().unwrap().clone()], types: Default::default() };
    let ctx = RunContext {
        node: node.clone(),
        injection: Injection { run_id: CANARY.into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, taint: None },
        committed_at: at,
    };
    land(&store, &decl, &batch, &ctx).unwrap();
    let record = RequestRecord {
        request_id: CANARY.into(), vendor_request_id: None, connector: "remote".into(),
        method: "POST".into(), url_host: "example.test".into(), status_code: Some(201),
        started_at: at, duration_ms: 7, batch_seq: Some(0),
    };
    ledger::append(&store, "documents", CANARY, &node, &[record]).unwrap();
    let digest = format!("{:x}", Sha256::digest(CANARY.as_bytes()));
    store.land_blob(&digest, CANARY.as_bytes()).unwrap();

    let files = tree(&root);
    let hits: usize = files.iter().map(|(_, bytes)| bytes.windows(CANARY.len()).filter(|window| *window == CANARY.as_bytes()).count()).sum();
    contextful_eval::record::emit("encrypt-no-plaintext", hits as f64, files.len() as u64, 0);
    assert_eq!(hits, 0, "{files:?}");
    assert_eq!(table_rows(&store, &decl, &["body"]).unwrap()[0]["body"], CANARY);
    let ledger_path = ledger::files(&store, "documents").unwrap().pop().unwrap();
    assert_eq!(ledger::read_for_store(&store, &ledger_path).unwrap()[0].1.request_id, CANARY);
    assert_eq!(store.blob(&digest).unwrap().unwrap(), CANARY.as_bytes());
}

#[test]
fn a_bound_store_seals_its_request_ledger() {
    use contextful_context::ledger;
    use contextful_core::store::lay_out::NodeId;
    use contextful_core::store::ledger::RequestRecord;
    use contextful_core::time::Instant;

    let key_var = "CONTEXTFUL_TEST_KEY_74_LEDGER";
    // This unique variable is read only by this test's store; no other test changes it.
    unsafe { std::env::set_var(key_var, "0123456789abcdef0123456789abcdef") };
    let (dir, opened) = store_with(&format!("[encryption]\nkey_source = \"env:{key_var}\"\n"));
    let store = opened.unwrap();
    let canary = "ledger-canary-5f1e unique plaintext marker";
    let record = RequestRecord {
        request_id: canary.into(),
        vendor_request_id: None,
        connector: "source".into(),
        method: "GET".into(),
        url_host: "example.invalid".into(),
        status_code: Some(200),
        started_at: Instant::parse("2030-01-01T00:00:00Z").unwrap(),
        duration_ms: 1,
        batch_seq: Some(0),
    };
    let node = NodeId::parse("ingest-a").unwrap();
    ledger::append(&store, "documents", "run-1", &node, &[record.clone()]).unwrap();
    let path = ledger::files(&store, "documents").unwrap().remove(0);
    let bytes = std::fs::read(path.clone()).unwrap();
    assert!(!bytes.windows(canary.len()).any(|part| part == canary.as_bytes()));
    let rows = ledger::read_for_store(&store, &path).unwrap();
    assert_eq!(rows, [("run-1".into(), record)]);
    assert!(written(&dir.path().join(".contextful/context/research")).iter().all(|p| !std::fs::read(p).unwrap().windows(canary.len()).any(|part| part == canary.as_bytes())));
}

#[test]
fn a_bound_store_seals_landed_blob_bytes() {
    use sha2::Digest;
    let key_var = "CONTEXTFUL_TEST_KEY_74_BLOB";
    // This unique variable is read only by this test's store; no other test changes it.
    unsafe { std::env::set_var(key_var, "0123456789abcdef0123456789abcdef") };
    let (dir, opened) = store_with(&format!("[encryption]\nkey_source = \"env:{key_var}\"\n"));
    let store = opened.unwrap();
    let canary = b"blob-canary-5f1e unique plaintext marker";
    let digest = format!("{:x}", sha2::Sha256::digest(canary));
    store.land_blob(&digest, canary).unwrap();
    assert_eq!(store.blob(&digest).unwrap().as_deref(), Some(canary.as_slice()));
    let files = written(&dir.path().join(".contextful/context/research"));
    assert!(files.iter().all(|p| !std::fs::read(p).unwrap().windows(canary.len()).any(|part| part == canary)));
}

/// A sealed full-text sidecar file larger than 256 MiB stays unopened, and its arm adds no candidates.
// spec: read.retrieve.fulltext-sealed-cap@5e433a1f
#[test]
fn a_sealed_full_text_sidecar_past_256_mib_stays_unopened() {
    use contextful_context::fulltext::{build, FulltextSidecar, POSTINGS_FILE};
    use contextful_context::vector::{Fallback, Sealing};
    use contextful_core::read::rank::{fulltext_sealed_over_cap, FULLTEXT_SEALED_CAP_BYTES};
    use contextful_core::store::lay_out::SnapshotId;
    use contextful_core::time::Instant;
    assert_eq!(FULLTEXT_SEALED_CAP_BYTES, 256 * 1024 * 1024);
    assert!(!fulltext_sealed_over_cap(FULLTEXT_SEALED_CAP_BYTES) && fulltext_sealed_over_cap(FULLTEXT_SEALED_CAP_BYTES + 1));

    let (rows, decl) = fulltext_rows("passage-canary");
    let id = SnapshotId::next(Instant::parse("2030-01-01T00:00:00Z").unwrap(), None);
    let cipher = TestCipher { key: 0x5a };
    let sealed = tempfile::tempdir().unwrap();
    let built = build(sealed.path(), &id, &rows, &decl, &decl.indexes()[0], &Sealing::Sealed(&cipher)).unwrap();
    let file = sealed.path().join(&built.path).join(POSTINGS_FILE);
    let entry = contextful_core::store::index::IndexEntry::from(built);
    assert!(FulltextSidecar::open(sealed.path(), "passages", &entry, &Sealing::Sealed(&cipher)).is_ok());
    // A sparse file one byte past the cap: its size alone decides, and no byte is read.
    std::fs::OpenOptions::new().write(true).open(&file).unwrap().set_len(FULLTEXT_SEALED_CAP_BYTES + 1).unwrap();
    assert_eq!(FulltextSidecar::open(sealed.path(), "passages", &entry, &Sealing::Sealed(&cipher)).err(), Some(Fallback::OverCap));
    // A plaintext sidecar is mapped, never decrypted, and takes no such cap.
    let plain = tempfile::tempdir().unwrap();
    let built = build(plain.path(), &id, &rows, &decl, &decl.indexes()[0], &Sealing::Plaintext).unwrap();
    let entry = contextful_core::store::index::IndexEntry::from(built);
    assert!(FulltextSidecar::open(plain.path(), "passages", &entry, &Sealing::Plaintext).is_ok());
}
