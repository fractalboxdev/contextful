//! What every credentialed subcommand resolves before its first effect: the one
//! credential, admitted against pinned keys, and the read face over the project.

use anyhow::{Context, Result};
use contextful_context::read::Face;
use contextful_context::Store;
use crate::run::SystemClock;
use contextful_core::ports::Clock;
use contextful_core::AuthorityError;
use contextful_policy::enforce::mask::Pepper;
use contextful_policy::keyset::{KeySource, StaticPins};
use contextful_policy::revoke::{parse_denylist, RevocationState};
use contextful_policy::verify::{verify_local_bearer, Admission, AdmittedAuthority};
use crate::project::Located;
use std::path::PathBuf;

/// The environment variable carrying the credential, kept out of the process arguments.
pub const TOKEN_VAR: &str = "CONTEXTFUL_TOKEN";

/// The variable `--public-key` falls back to (`authority.verify.pin-source`).
pub const PUBKEY_VAR: &str = "CONTEXTFUL_ISSUER_PUBKEY";

/// The variable `--audience` falls back to (`authority.verify.pin-source`).
pub const AUDIENCE_VAR: &str = "CONTEXTFUL_AUDIENCE";

/// The refusals of admission over the process transport. `Display` begins with the
/// identifier.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdmitError {
    /// (`surface.package.stdio-credential`)
    #[error("StdioCredentialMissing: {0}")]
    StdioCredentialMissing(String),
}

/// The static pins from `--public-key`, else [`PUBKEY_VAR`]; neither raises
/// `KeySetUnavailable` naming both (`authority.verify.pin-source`).
pub fn static_pins(flag: Option<&str>) -> Result<StaticPins, AuthorityError> {
    let Some(list) = flag else {
        return Err(AuthorityError::KeySetUnavailable(format!(
            "no issuer key pins: pass --public-key or set {PUBKEY_VAR}"
        )));
    };
    StaticPins::parse(list)
}

/// The key version a denylist entry records for credentials verified under static pins.
const STATIC_KEY_VERSION: &str = "static";

/// How a credential is admitted.
#[derive(clap::Args)]
pub struct AdmitArgs {
    /// Comma-separated issuer key pins.
    #[arg(long, env = PUBKEY_VAR)]
    pub public_key: Option<String>,
    /// Expected audience; absent performs no audience check.
    #[arg(long, env = AUDIENCE_VAR)]
    pub audience: Option<String>,
    /// A file of revocation identifiers, one per line.
    #[arg(long)]
    pub denylist: Option<PathBuf>,
}

impl AdmitArgs {
    /// Admit the credential in [`TOKEN_VAR`] now, returning it with the revocation state
    /// later effect boundaries re-read.
    pub fn admit(&self, what: &str) -> Result<(AdmittedAuthority, RevocationState)> {
        let Some(token) = std::env::var(TOKEN_VAR).ok().filter(|t| !t.trim().is_empty()) else {
            return Err(AdmitError::StdioCredentialMissing(format!(
                "{TOKEN_VAR} is unset: {what} admits one capability credential and acts on nothing without it"
            ))
            .into());
        };
        let keys = static_pins(self.public_key.as_deref())?.keys()?;
        let mut revocation = RevocationState::default();
        if let Some(path) = &self.denylist {
            let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
            revocation.denylist = parse_denylist(&text, STATIC_KEY_VERSION);
        }
        let authority = {
            let mut admission = Admission::new(SystemClock.now(), &revocation);
            if let Some(aud) = self.audience.as_deref() {
                admission = admission.expecting(aud);
            }
            verify_local_bearer(&token, &keys, &admission)?
        };
        Ok((authority, revocation))
    }
}

/// Open the read face over the project's store and manifest, signalling a development
/// pepper once.
pub fn face(located: &Located) -> Result<Face> {
    let pepper = Pepper::resolve(|k| std::env::var(k).ok());
    let declaration = &located.declaration;
    let manifest =
        std::fs::read_to_string(declaration).with_context(|| format!("reading the declaration `{}`", declaration.display()))?;
    let store = Store::open(&located.project.dir, &located.project.name)?;
    let face = Face::open(store, &manifest, pepper.clone())?;
    if let Some(signal) = pepper.signal() {
        eprintln!("{signal}");
    }
    Ok(face)
}
