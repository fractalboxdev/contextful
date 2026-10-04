//! AES-256-GCM per-file sealing under a wrapped data key.

use contextful_core::store::encrypt::{FileCipher, SealError};
use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey};

const MAGIC: &[u8; 8] = b"CFSEAL01";
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
