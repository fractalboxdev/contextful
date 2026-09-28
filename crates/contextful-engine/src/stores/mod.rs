//! Adapters behind the journal, blob and awakeable store ports of
//! `contextful_core::run::ports`: the machine-local file tree, the default, and an
//! in-process memory store. Every adapter passes [`crate::conformance`].

pub mod file;
pub mod memory;

pub use file::{FileAwakeableStore, FileBlobStore, FileJournalStore};
pub use memory::{MemoryAwakeableStore, MemoryBlobStore, MemoryJournalStore};

/// Whether `token` has the shape a minted token has: 1 to 128 ASCII letters, digits, `-`
/// or `_`. A token outside it names no row in any store.
pub fn is_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= 128 && token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
