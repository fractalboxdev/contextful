//! Public benchmark corpora (`assurance.baseline.public-corpora`): each enters through a
//! fetch-manifest entry naming its URL, its dataset's terms and the SHA-256 digest of its
//! bytes. A fetched corpus lives under [`PUBLIC_CORPUS_DIR`], which version control
//! ignores, and runs as held-out comparison alone (`assurance.baseline.native-gate`).

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::EvalError;

/// Where fetched public corpora land, relative to the repository root.
pub const PUBLIC_CORPUS_DIR: &str = "evals/public";

/// A fetch manifest: one `[[corpus]]` table per public corpus.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchManifest {
    #[serde(default)]
    pub corpus: Vec<FetchEntry>,
}

/// One public corpus.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchEntry {
    pub name: String,
    /// An http or https URL.
    pub url: String,
    /// The dataset's licence or terms the fetch runs under.
    pub terms: String,
    /// Lowercase hex SHA-256 of the fetched bytes.
    pub sha256: String,
}

impl FetchManifest {
    /// Parse a manifest. An entry with a blank name or terms, a URL other than http or
    /// https, a malformed digest or a repeated name refuses the manifest.
    pub fn parse(text: &str) -> Result<FetchManifest, String> {
        let manifest: FetchManifest = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut names = std::collections::BTreeSet::new();
        for e in &manifest.corpus {
            if e.name.trim().is_empty() {
                return Err("a corpus entry carries a blank name".into());
            }
            if !names.insert(e.name.as_str()) {
                return Err(format!("`{}` appears twice", e.name));
            }
            if e.terms.trim().is_empty() {
                return Err(format!("`{}` names no terms to fetch under", e.name));
            }
            if !(e.url.starts_with("https://") || e.url.starts_with("http://")) {
                return Err(format!("`{}` names `{}`; a corpus URL is http or https", e.name, e.url));
            }
            if e.sha256.len() != 64 || !e.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
                return Err(format!("`{}` pins `{}`; a digest is 64 lowercase hex characters", e.name, e.sha256));
            }
        }
        Ok(manifest)
    }

    pub fn entry(&self, name: &str) -> Option<&FetchEntry> {
        self.corpus.iter().find(|e| e.name == name)
    }
}

impl FetchEntry {
    /// Hold fetched `bytes` to the pinned digest before any case converts.
    pub fn verify(&self, bytes: &[u8]) -> Result<(), EvalError> {
        let actual = sha256_hex(bytes);
        if actual == self.sha256 {
            Ok(())
        } else {
            Err(EvalError::PublicCorpusDigestMismatch { corpus: self.name.clone(), expected: self.sha256.clone(), actual })
        }
    }
}

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}
