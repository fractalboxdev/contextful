//! `store.encrypt`: the per-file cipher port an encrypted project seals sidecar files with.

/// A cipher failing to seal or open a file's bytes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SealError(pub String);

/// Seals and opens one file's bytes under a per-file data key the project key wraps
/// (`store.encrypt.cipher`). An adapter holding the key implements it; the store never
/// sees key material.
pub trait FileCipher: Send + Sync {
    /// The key version new files seal under.
    fn key_version(&self) -> u32;

    /// The sealed form of `plaintext`, self-contained: it carries its own wrapped data key
    /// and nonce.
    fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, SealError>;

    /// The plaintext of a sealed file, in process memory; a tampered or foreign file
    /// refuses.
    fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, SealError>;
}
