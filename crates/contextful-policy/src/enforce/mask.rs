//! `authority.mask`: the class registry, column policies and their manifest checks, the
//! pepper, the keyed digest and token, and the masked projection a relation carries.

use super::PolicyError;
use contextful_core::enforce::EnforceError;
use contextful_core::store::declare::DeclarationMalformed;
use contextful_core::store::reconcile::ColumnType;
use contextful_core::store::relation::ident;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Output width of a keyed hash, in chars (`authority.mask.hash-width`).
pub const HASH_OUTPUT_WIDTH: u32 = 32;

/// Output width of a tokenization, in chars (`authority.mask.token-width`).
pub const TOKEN_OUTPUT_WIDTH: u32 = 20;

/// Fewest inputs one masked output covers (`authority.mask.crowd`).
pub const MASK_CROWD_FLOOR: u64 = 1000;

/// Column masks one table declares (`authority.mask.masks-per-table`).
pub const MASKS_PER_TABLE: usize = 128;

/// The one environment variable holding the pepper (`authority.mask.pepper`).
pub const PEPPER_VAR: &str = "CONTEXTFUL_PEPPER";

/// The pepper an unset variable resolves to (`authority.mask.development-pepper`).
const DEVELOPMENT_PEPPER: &str = "contextful-development-pepper-not-for-production";

/// The scalar functions the query layer calls, each holding the pepper in process memory
/// (`authority.mask.native-digest`).
pub const HASH_FUNCTION: &str = "contextful_mask_hash";
pub const TOKEN_FUNCTION: &str = "contextful_mask_token";

/// Lowercase letters and digits a token is drawn from.
const TOKEN_ALPHABET: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";

/// One entry of the class registry (`authority.mask.class-registry`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Class {
    pub name: &'static str,
    /// The registered domain size of an exhaustible class.
    pub domain: Option<f64>,
}

impl Class {
    pub fn exhaustible(&self) -> bool {
        self.domain.is_some()
    }

    /// Whether the class marks protected health data.
    pub fn protected(&self) -> bool {
        self.name == "phi"
    }
}

/// The class registry.
pub const CLASSES: [Class; 5] = [
    Class { name: "phi", domain: None },
    Class { name: "ssn", domain: Some(1e9) },
    Class { name: "phone", domain: Some(1e10) },
    Class { name: "email", domain: Some(1e10) },
    Class { name: "mrn", domain: Some(1e8) },
];

/// Look up a class; one outside the registry refuses (`authority.mask.unknown-class`).
pub fn class(name: &str) -> Result<Class, EnforceError> {
    CLASSES
        .iter()
        .find(|c| c.name == name)
        .copied()
        .ok_or_else(|| EnforceError::UnknownClass(format!("class `{name}` is not in the class registry")))
}

/// A masking strategy (`authority.mask.strategies`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    Drop,
    Hash,
    Tokenize,
    Truncate(u32),
    Bucket(u64),
    Range(u64),
}

impl Strategy {
    fn parse(s: &str) -> Result<Strategy, DeclarationMalformed> {
        let bad = || DeclarationMalformed(format!("strategy `{s}` is not drop, hash, tokenize, truncate:<n>, bucket:<n> or range:<n>"));
        let (name, arg) = s.split_once(':').map_or((s, None), |(n, a)| (n, Some(a)));
        let positive = |a: Option<&str>| a.and_then(|a| a.parse::<u64>().ok()).filter(|n| *n > 0).ok_or_else(bad);
        Ok(match (name, arg) {
            ("drop", None) => Strategy::Drop,
            ("hash", None) => Strategy::Hash,
            ("tokenize", None) => Strategy::Tokenize,
            ("truncate", a) => Strategy::Truncate(u32::try_from(positive(a)?).map_err(|_| bad())?),
            ("bucket", a) => Strategy::Bucket(positive(a)?),
            ("range", a) => Strategy::Range(positive(a)?),
            _ => return Err(bad()),
        })
    }

    /// The alphabet size of a keyed digest's output: hexadecimal for `hash`, lowercase
    /// letters and digits for `tokenize`.
    pub fn digest_base(self) -> u32 {
        match self {
            Strategy::Tokenize => TOKEN_ALPHABET.len() as u32,
            _ => 16,
        }
    }

    /// The output width of a digest primary.
    fn digest_width(self) -> Option<u32> {
        match self {
            Strategy::Hash => Some(HASH_OUTPUT_WIDTH),
            Strategy::Tokenize => Some(TOKEN_OUTPUT_WIDTH),
            _ => None,
        }
    }
}

/// A column mask: one value whose primary and combine halves are private, driving both
/// layers; no consumer applies the primary alone (`authority.mask.one-value`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mask {
    primary: Strategy,
    combine: Option<u32>,
}

/// One column's declared policy (`authority.mask.column-policy`).
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnPolicy {
    pub class: Option<Class>,
    pub mask: Option<Mask>,
    pub crowd: u64,
    pub summarize_only: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawColumnPolicy {
    #[serde(default)]
    class: Option<String>,
    #[serde(default)]
    strategy: Option<String>,
    #[serde(default)]
    combine: Option<String>,
    #[serde(default)]
    crowd: Option<u64>,
    #[serde(default)]
    summarize_only: Option<bool>,
}

impl ColumnPolicy {
    /// Check one column's declaration at manifest load.
    pub fn parse(column: &str, raw: RawColumnPolicy) -> Result<ColumnPolicy, PolicyError> {
        let at = |why: String| format!("column `{column}`: {why}");
        let class = raw.class.as_deref().map(class).transpose()?;
        let crowd = raw.crowd.unwrap_or(MASK_CROWD_FLOOR);
        if crowd < MASK_CROWD_FLOOR {
            return Err(DeclarationMalformed(at(format!("crowd {crowd} is below the floor of {MASK_CROWD_FLOOR} values"))).into());
        }
        let primary = raw.strategy.as_deref().map(Strategy::parse).transpose()?;
        let combine = match (&raw.combine, primary) {
            (None, _) => None,
            (Some(c), None) => return Err(DeclarationMalformed(at(format!("combine `{c}` has no strategy to combine with"))).into()),
            (Some(c), Some(p)) => {
                let combine = Strategy::parse(c)?;
                let Some(width) = p.digest_width() else {
                    return Err(DeclarationMalformed(at(format!("combine `{c}` behind a strategy that is no digest"))).into());
                };
                match combine {
                    Strategy::Truncate(n) if n >= width => {
                        return Err(EnforceError::TruncationCutsNothing(at(format!(
                            "truncate:{n} is at or past the primary's {width}-char output"
                        )))
                        .into())
                    }
                    Strategy::Truncate(n) => Some(n),
                    _ => {
                        return Err(EnforceError::CombineWithoutGeneralization(at(format!(
                            "`{c}` behind a digest leaves its output unchanged; truncate is the combine that generalizes one"
                        )))
                        .into())
                    }
                }
            }
        };
        if let (Some(p @ (Strategy::Hash | Strategy::Tokenize)), Some(cls)) = (primary, class) {
            if let Some(domain) = cls.domain {
                let Some(n) = combine else {
                    return Err(EnforceError::DigestAloneOnExhaustibleClass(at(format!(
                        "a bare keyed digest over exhaustible class `{}` is recoverable by enumeration; add combine = \"truncate:<n>\"",
                        cls.name
                    )))
                    .into());
                };
                let ceiling = truncation_ceiling(domain, crowd, p.digest_base());
                if n > ceiling {
                    return Err(EnforceError::TruncationTooWide(at(format!(
                        "truncate:{n} over class `{}` exceeds the ceiling of {ceiling} for crowd {crowd}",
                        cls.name
                    )))
                    .into());
                }
            }
        }
        Ok(ColumnPolicy {
            class,
            mask: primary.map(|primary| Mask { primary, combine }),
            crowd,
            summarize_only: raw.summarize_only.unwrap_or(false),
        })
    }
}

/// The widest truncation a keyed digest over an exhaustible class admits:
/// floor(log_b(domain ÷ crowd)), `b` the digest's alphabet size
/// (`authority.mask.truncation-ceiling`).
pub fn truncation_ceiling(domain: f64, crowd: u64, base: u32) -> u32 {
    let ratio = domain / crowd as f64;
    if ratio < 1.0 {
        0
    } else {
        // The epsilon keeps an exact power, such as 36^4, from flooring one short.
        (ratio.ln() / f64::from(base).ln() + 1e-9).floor() as u32
    }
}

/// The pepper: the key of `hash` and `tokenize`, resolved once and read by both layers.
#[derive(Clone)]
pub struct Pepper {
    key: Vec<u8>,
    development: bool,
}

impl std::fmt::Debug for Pepper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pepper").field("development", &self.development).finish_non_exhaustive()
    }
}

impl Pepper {
    /// Resolve the pepper from [`PEPPER_VAR`] (`authority.mask.pepper`). Left unset or
    /// empty, it resolves to a constant in the source tree
    /// (`authority.mask.development-pepper`).
    pub fn resolve(env: impl Fn(&str) -> Option<String>) -> Pepper {
        match env(PEPPER_VAR).filter(|v| !v.is_empty()) {
            Some(v) => Pepper { key: v.into_bytes(), development: false },
            None => Pepper { key: DEVELOPMENT_PEPPER.as_bytes().to_vec(), development: true },
        }
    }

    pub fn is_development(&self) -> bool {
        self.development
    }

    /// The one signal a run under the development pepper emits.
    pub fn signal(&self) -> Option<String> {
        self.development.then(|| {
            format!("{PEPPER_VAR} is unset: masked values are keyed by the development pepper in the source tree")
        })
    }

    fn mac(&self, label: &str, value: &str) -> [u8; 32] {
        hmac_sha256(&self.key, &[label.as_bytes(), &[0], value.as_bytes()].concat())
    }

    /// The keyed digest: HMAC-SHA-256 under the pepper, 32 hex chars. Pseudonymous:
    /// anyone holding the pepper recomputes it (`authority.mask.pseudonymous`).
    pub fn digest(&self, value: &str) -> String {
        self.mac("hash", value).iter().take(HASH_OUTPUT_WIDTH as usize / 2).map(|b| format!("{b:02x}")).collect()
    }

    /// The token: 20 chars of lowercase letters and digits keyed by the pepper.
    pub fn token(&self, value: &str) -> String {
        self.mac("tokenize", value)
            .iter()
            .take(TOKEN_OUTPUT_WIDTH as usize)
            .map(|b| TOKEN_ALPHABET[usize::from(*b) % TOKEN_ALPHABET.len()] as char)
            .collect()
    }
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let pad = |byte: u8| k.iter().map(|b| b ^ byte).collect::<Vec<u8>>();
    let inner = Sha256::new().chain_update(pad(0x36)).chain_update(message).finalize();
    Sha256::new().chain_update(pad(0x5c)).chain_update(inner).finalize().into()
}

impl Mask {
    /// The masked value for one input of a column typed `ty`, as the write layer applies
    /// it; `None` is SQL NULL. Every strategy yields exactly the value [`Mask::sql`] does.
    pub fn apply(&self, pepper: &Pepper, value: Option<&str>, ty: ColumnType) -> Option<String> {
        let lower = |v: &str, n: u64| v.trim().parse::<f64>().ok().map(|v| ((v / n as f64).floor() * n as f64) as i64);
        let primary = match (self.primary, value) {
            (Strategy::Drop, _) => return (ty == ColumnType::Utf8).then(String::new),
            (_, None) => return None,
            (Strategy::Hash, Some(v)) => pepper.digest(v),
            (Strategy::Tokenize, Some(v)) => pepper.token(v),
            (Strategy::Truncate(n), Some(v)) => v.chars().take(n as usize).collect(),
            (Strategy::Bucket(n), Some(v)) => lower(v, n)?.to_string(),
            (Strategy::Range(n), Some(v)) => {
                let lo = lower(v, n)?;
                format!("{lo}-{}", lo + n as i64)
            }
        };
        Some(match self.combine {
            Some(n) => primary.chars().take(n as usize).collect(),
            None => primary,
        })
    }

    /// The projection expression replacing a column in place: the query layer calls the
    /// pepper-holding scalar functions, so no key material enters the relation text
    /// (`authority.mask.native-digest`). `drop` nulls the cell, or yields an empty string
    /// for a string column (`authority.mask.drop`).
    pub fn sql(&self, column: &str, ty: ColumnType) -> String {
        let c = ident(column);
        let text = format!("CAST({c} AS VARCHAR)");
        let primary = match self.primary {
            Strategy::Drop if ty == ColumnType::Utf8 => return "CAST('' AS VARCHAR)".to_string(),
            Strategy::Drop => return format!("CAST(NULL AS {})", ty.sql()),
            Strategy::Hash => format!("{HASH_FUNCTION}({text})"),
            Strategy::Tokenize => format!("{TOKEN_FUNCTION}({text})"),
            Strategy::Truncate(n) => format!("left({text}, {n})"),
            Strategy::Bucket(n) => format!("CAST(CAST(floor(TRY_CAST(trim({text}) AS DOUBLE) / {n}) * {n} AS BIGINT) AS VARCHAR)"),
            Strategy::Range(n) => {
                let lo = format!("CAST(floor(TRY_CAST(trim({text}) AS DOUBLE) / {n}) * {n} AS BIGINT)");
                format!("(CAST({lo} AS VARCHAR) || '-' || CAST({lo} + {n} AS VARCHAR))")
            }
        };
        match self.combine {
            Some(n) => format!("left({primary}, {n})"),
            None => primary,
        }
    }
}
