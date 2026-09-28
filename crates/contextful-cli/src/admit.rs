//! What every credentialed subcommand resolves before its first effect: the one
//! credential, admitted against pinned keys, and the read face over the project.

use anyhow::{bail, Context, Result};
use contextful_context::read::Face;
use contextful_context::Store;
use crate::run::SystemClock;
use contextful_core::ports::Clock;
use contextful_policy::enforce::mask::Pepper;
use contextful_policy::keyset::{KeySource, StaticPins};
use contextful_policy::revoke::{parse_denylist, RevocationState};
use contextful_policy::verify::{verify_local_bearer, Admission, AdmittedAuthority};
use std::path::{Path, PathBuf};

/// The environment variable carrying the credential, kept out of the process arguments.
pub const TOKEN_VAR: &str = "CONTEXTFUL_TOKEN";

/// The key version a denylist entry records for credentials verified under static pins.
const STATIC_KEY_VERSION: &str = "static";

/// How a credential is admitted.
#[derive(clap::Args)]
pub struct AdmitArgs {
    /// Comma-separated issuer key pins.
    #[arg(long)]
    pub public_key: String,
    /// Expected audience; absent performs no audience check.
    #[arg(long)]
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
            bail!("{TOKEN_VAR} is unset: {what} admits one capability credential and acts on nothing without it");
        };
        let keys = StaticPins::parse(&self.public_key)?.keys()?;
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
pub fn face(project: &str, declaration: &Path) -> Result<Face> {
    let pepper = Pepper::resolve(|k| std::env::var(k).ok());
    let manifest =
        std::fs::read_to_string(declaration).with_context(|| format!("reading the declaration `{}`", declaration.display()))?;
    let store = Store::open(&std::env::current_dir()?, project)?;
    let face = Face::open(store, &manifest, pepper.clone())?;
    if let Some(signal) = pepper.signal() {
        eprintln!("{signal}");
    }
    Ok(face)
}
