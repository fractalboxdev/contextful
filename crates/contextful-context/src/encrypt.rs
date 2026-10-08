//! AES-256-GCM per-file sealing under a wrapped data key.

use crate::error::{ContextError, Result as ContextResult};
use contextful_core::store::StoreError;
use contextful_core::store::encrypt::{FileCipher, SealError};
use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey};
use sha2::{Digest, Sha256};
use std::path::Path;
use crate::error::IoPath;

const MAGIC: &[u8; 8] = b"CFSEAL01";
pub(crate) const PARQUET_KEY_NAME: &str = "contextful_project";

/// Files whose canonical plaintext is JSON or a UTF-8 counter. A bound store keeps
/// those bytes inside the versioned file envelope and authenticates them on read.
pub enum MetadataFiles<'a> {
    Plaintext,
    Sealed(&'a dyn FileCipher),
}

impl<'a> MetadataFiles<'a> {
    pub fn plaintext() -> Self { Self::Plaintext }
    pub fn sealed(cipher: &'a dyn FileCipher) -> Self { Self::Sealed(cipher) }

    pub fn seal_bytes(&self, path: &Path, bytes: &[u8]) -> ContextResult<Vec<u8>> {
        match self {
            Self::Plaintext => Ok(bytes.to_vec()),
            Self::Sealed(cipher) => cipher.seal(bytes).map_err(|e| ContextError::Invalid(format!("{}: sealing metadata: {e}", path.display()))),
        }
    }

    pub fn open_bytes(&self, path: &Path, bytes: &[u8]) -> ContextResult<Vec<u8>> {
        match self {
            Self::Plaintext => Ok(bytes.to_vec()),
            Self::Sealed(cipher) => cipher.open(bytes).map_err(|e| ContextError::Invalid(format!("{}: opening metadata: {e}", path.display()))),
        }
    }

    pub fn read(&self, path: &Path) -> ContextResult<Vec<u8>> {
        let bytes = std::fs::read(path).at(path)?;
        self.open_bytes(path, &bytes)
    }

    pub fn read_optional(&self, path: &Path) -> ContextResult<Option<Vec<u8>>> {
        match std::fs::read(path) {
            Ok(bytes) => self.open_bytes(path, &bytes).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ContextError::Io { path: path.to_path_buf(), source: e }),
        }
    }

    pub fn replace(&self, path: &Path, bytes: &[u8]) -> ContextResult<()> {
        crate::store::replace_file(path, &self.seal_bytes(path, bytes)?)
    }

    pub fn create_new(&self, path: &Path, bytes: &[u8]) -> ContextResult<bool> {
        crate::store::create_new_file(path, &self.seal_bytes(path, bytes)?)
    }

    pub fn write(&self, path: &Path, bytes: &[u8]) -> ContextResult<()> {
        std::fs::write(path, self.seal_bytes(path, bytes)?).at(path)
    }
}
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;
const TAG_LEN: usize = 16;
const HEADER_LEN: usize = MAGIC.len() + 4;
const WRAPPED_LEN: usize = KEY_LEN + TAG_LEN;
const PREFIX_LEN: usize = HEADER_LEN + NONCE_LEN + WRAPPED_LEN + NONCE_LEN;

/// One project key and its version. The project key wraps a fresh random data key for each file.
pub struct AesGcmFileCipher {
    project_key: [u8; KEY_LEN],
    version: u32,
}

/// Keys held by one encrypted store. No formatter exposes either key.
pub(crate) struct ProjectEncryption {
    files: AesGcmFileCipher,
    parquet_key: [u8; 32],
}

impl std::fmt::Debug for ProjectEncryption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProjectEncryption { keys: [redacted] }")
    }
}

impl ProjectEncryption {
    fn from_key(key: [u8; 32]) -> Self {
        let mut hash = Sha256::new();
        hash.update(b"contextful/parquet/footer/v1\0");
        hash.update(key);
        let digest = hash.finalize();
        // The 44-character encoding of an AES-256 key is unambiguous to DuckDB's key parser.
        let parquet_key = digest.into();
        Self { files: AesGcmFileCipher::new(key, 1), parquet_key }
    }

    pub(crate) fn files(&self) -> &AesGcmFileCipher {
        &self.files
    }

    pub(crate) fn parquet_key(&self) -> &[u8; 32] {
        &self.parquet_key
    }
}

impl FileCipher for ProjectEncryption {
    fn key_version(&self) -> u32 { self.files.key_version() }
    fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, SealError> { self.files.seal(plaintext) }
    fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, SealError> { self.files.open(sealed) }
}

/// Resolve an environment binding to a raw 32-byte project key.
pub(crate) fn bind_key_source(source: &str) -> ContextResult<ProjectEncryption> {
    let Some(var) = source.strip_prefix("env:") else {
        return Err(StoreError::StoreEncryptionKeyUnbound(format!(
            "`[encryption] key_source = \"{source}\"` names a key-management service this build has no client for"
        )).into());
    };
    let value = std::env::var(var).map_err(|_| StoreError::StoreEncryptionKeyUnbound(format!(
        "`[encryption] key_source = \"{source}\"` names `{var}`, which this process lacks"
    )))?;
    if value.is_empty() {
        return Err(StoreError::StoreEncryptionKeyUnbound(format!(
            "`[encryption] key_source = \"{source}\"` names `{var}`, which this process lacks"
        )).into());
    }
    let key: [u8; 32] = value.as_bytes().try_into().map_err(|_| ContextError::Invalid(format!(
        "`[encryption] key_source = \"{source}\"` must bind exactly 32 key bytes"
    )))?;
    Ok(ProjectEncryption::from_key(key))
}

impl AesGcmFileCipher {
    /// A 256-bit project key is supplied by a bound key source.
    pub fn new(project_key: [u8; KEY_LEN], version: u32) -> Self {
        Self {
            project_key,
            version,
        }
    }

    fn key(bytes: &[u8; KEY_LEN]) -> Result<LessSafeKey, SealError> {
        UnboundKey::new(&aead::AES_256_GCM, bytes)
            .map(LessSafeKey::new)
            .map_err(|_| SealError("AES-256-GCM key initialization failed".into()))
    }

    fn random<const N: usize>() -> Result<[u8; N], SealError> {
        let mut bytes = [0; N];
        getrandom::fill(&mut bytes).map_err(|_| SealError("file key generation failed".into()))?;
        Ok(bytes)
    }
}

impl FileCipher for AesGcmFileCipher {
    fn key_version(&self) -> u32 {
        self.version
    }

    fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, SealError> {
        let data_key = Self::random::<KEY_LEN>()?;
        let wrap_nonce = Self::random::<NONCE_LEN>()?;
        let data_nonce = Self::random::<NONCE_LEN>()?;
        let mut result = Vec::with_capacity(PREFIX_LEN + plaintext.len() + TAG_LEN);
        result.extend_from_slice(MAGIC);
        result.extend_from_slice(&self.version.to_le_bytes());
        let header = result.clone();
        result.extend_from_slice(&wrap_nonce);
        let mut wrapped = data_key.to_vec();
        Self::key(&self.project_key)?
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(wrap_nonce),
                Aad::from(header.as_slice()),
                &mut wrapped,
            )
            .map_err(|_| SealError("file key wrapping failed".into()))?;
        result.extend_from_slice(&wrapped);
        result.extend_from_slice(&data_nonce);
        let mut body = plaintext.to_vec();
        Self::key(&data_key)?
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(data_nonce),
                Aad::from(header.as_slice()),
                &mut body,
            )
            .map_err(|_| SealError("file sealing failed".into()))?;
        result.extend_from_slice(&body);
        Ok(result)
    }

    fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, SealError> {
        if sealed.len() < PREFIX_LEN + TAG_LEN || &sealed[..MAGIC.len()] != MAGIC {
            return Err(SealError("unrecognised sealed file".into()));
        }
        let version = u32::from_le_bytes(
            sealed[MAGIC.len()..HEADER_LEN]
                .try_into()
                .expect("fixed version width"),
        );
        if version != self.version {
            return Err(SealError("sealed file key version differs".into()));
        }
        let header = &sealed[..HEADER_LEN];
        let wrap_nonce: [u8; NONCE_LEN] = sealed[HEADER_LEN..HEADER_LEN + NONCE_LEN]
            .try_into()
            .expect("fixed nonce width");
        let mut wrapped =
            sealed[HEADER_LEN + NONCE_LEN..HEADER_LEN + NONCE_LEN + WRAPPED_LEN].to_vec();
        let data_key = Self::key(&self.project_key)?
            .open_in_place(
                Nonce::assume_unique_for_key(wrap_nonce),
                Aad::from(header),
                &mut wrapped,
            )
            .map_err(|_| SealError("file key authentication failed".into()))?;
        let data_key: [u8; KEY_LEN] = data_key
            .try_into()
            .map_err(|_| SealError("wrapped file key has invalid length".into()))?;
        let data_nonce: [u8; NONCE_LEN] = sealed[PREFIX_LEN - NONCE_LEN..PREFIX_LEN]
            .try_into()
            .expect("fixed nonce width");
        let mut body = sealed[PREFIX_LEN..].to_vec();
        let plaintext_len = Self::key(&data_key)?
            .open_in_place(
                Nonce::assume_unique_for_key(data_nonce),
                Aad::from(header),
                &mut body,
            )
            .map_err(|_| SealError("sealed file authentication failed".into()))?
            .len();
        body.truncate(plaintext_len);
        Ok(body)
    }
}
