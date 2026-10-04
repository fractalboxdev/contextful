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
