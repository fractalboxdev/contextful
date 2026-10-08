//! `run.journal`: entry keys, the idempotency key and the inline cutoff.

use contextful_core::run::journal::{awakeable_key, sha256_hex, EntryKey, Stored, INLINE_CUTOFF_BYTES};

#[test]
fn shared_dynamic_stores_satisfy_the_generic_run_ports() {
    use contextful_core::run::ports::{BlobStore, JournalStore};
    use std::sync::Arc;
    fn journal_port<T: JournalStore>() {}
    fn blob_port<T: BlobStore>() {}
    journal_port::<Arc<dyn JournalStore>>();
    blob_port::<Arc<dyn BlobStore>>();
}

/// Every outbound request a step makes carries an idempotency key derived from its entry key, identical on every
/// re-entry of that effect.
// spec: run.journal.idempotency-key@cc89bbef
#[test]
fn the_idempotency_key_derives_from_the_entry_key_alone() {
    let k = EntryKey::new("x-1", "pull-1", b"\"p2\"");
    assert_eq!(k.input_hash, sha256_hex(b"\"p2\""));
    assert_eq!(k.idempotency_key(), EntryKey::new("x-1", "pull-1", b"\"p2\"").idempotency_key());
    assert_eq!(k.idempotency_key().len(), 32);
    for other in [EntryKey::new("x-2", "pull-1", b"\"p2\""), EntryKey::new("x-1", "pull-2", b"\"p2\""), EntryKey::new("x-1", "pull-1", b"\"p3\"")] {
        assert_ne!(k.idempotency_key(), other.idempotency_key(), "{other:?}");
    }
    assert_eq!(awakeable_key("tok"), sha256_hex(b"awakeable:tok"));
}

/// A value of 1 MiB or smaller is stored inline in its row as bytes; a larger value lands in a content-addressed
/// blob named by its sha256, and the row holds the reference.
// spec: run.journal.inline-cutoff@eccb9c26
#[test]
fn values_up_to_1_mib_are_inline_and_larger_ones_are_blobs() {
    assert_eq!(INLINE_CUTOFF_BYTES, 1024 * 1024);
    let at_cutoff = vec![b'a'; INLINE_CUTOFF_BYTES];
    let s = Stored::place(&at_cutoff);
    assert!(s.blob().is_none());
    assert_eq!(s.inline_bytes().unwrap(), at_cutoff);
    let binary = vec![0u8, 159, 255];
    assert_eq!(Stored::place(&binary).inline_bytes().unwrap(), binary);
    let over = vec![b'a'; INLINE_CUTOFF_BYTES + 1];
    match Stored::place(&over) {
        Stored::Blob { sha256, bytes } => {
            assert_eq!(sha256, sha256_hex(&over));
            assert_eq!(bytes, over.len() as u64);
        }
        other => panic!("{other:?}"),
    }
}
