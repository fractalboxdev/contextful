//! What every credentialed subcommand resolves before its first effect: the one
//! credential, admitted against pinned keys, and the read face over the project.

use anyhow::{Context, Result};
use contextful_context::read::Face;
use contextful_context::Store;
use crate::run::SystemClock;
use contextful_core::ports::Clock;
use contextful_core::revoke::KeySetLedger;
use contextful_core::time::Instant;
use contextful_core::AuthorityError;
use contextful_policy::enforce::mask::Pepper;
use contextful_policy::keyset::{KeySet, KeySource, StaticPins};
use contextful_policy::revoke::{parse_denylist, RevocationState};
use contextful_core::issue::AuthoringPosture;
use contextful_policy::verify::{effect_boundary, verify_inherited_pipe, Admission, AdmittedAuthority};
use crate::project::Located;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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
    /// (`authority.revoke.ledger-unavailable`)
    #[error("KeySetLedgerUnavailable: {0}")]
    KeySetLedgerUnavailable(String),
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

/// The static pins less every key the ledger retires at `now`
/// (`authority.revoke.immediate-retire`).
pub fn live_pins(flag: Option<&str>, ledger: &KeySetLedger, now: Instant) -> Result<StaticPins, AuthorityError> {
    static_pins(flag)?.retaining(|k| ledger.verifies(k, now))
}

/// The key version a denylist entry records for credentials verified under static pins.
const STATIC_KEY_VERSION: &str = "static";

/// The project's key-set ledger file (`authority.revoke.epoch-store`): `--keyset`, else
/// [`KeySetLedger::PATH`] under the project root. A file `--keyset` names, or one this
/// value has read, is required from then on: its absence raises `KeySetLedgerUnavailable`
/// rather than reading as an empty ledger that lifts every retirement and epoch bump
/// (`authority.revoke.ledger-unavailable`).
#[derive(Debug)]
pub struct LedgerFile {
    path: PathBuf,
    required: AtomicBool,
}

impl LedgerFile {
    /// The ledger `flag` names, else the one under `root`.
    pub fn at(root: &Path, flag: Option<&Path>) -> LedgerFile {
        match flag {
            Some(p) => LedgerFile { path: p.to_path_buf(), required: AtomicBool::new(true) },
            None => LedgerFile { path: root.join(KeySetLedger::PATH), required: AtomicBool::new(false) },
        }
    }

    /// Where the ledger is written.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The ledger now. An absent default file that no earlier read found is empty: no key
    /// retired and no epoch bumped.
    pub fn read(&self) -> Result<KeySetLedger, AdmitError> {
        let unavailable = |why: String| AdmitError::KeySetLedgerUnavailable(format!("{}: {why}", self.path.display()));
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !self.required.load(Ordering::SeqCst) => {
                return Ok(KeySetLedger::default());
            }
            Err(e) => return Err(unavailable(e.to_string())),
        };
        self.required.store(true, Ordering::SeqCst);
        KeySetLedger::parse(&text, &self.path.display().to_string()).map_err(unavailable)
    }
}

/// Static pins filtered through the ledger at every read, so a key the ledger retires, at
/// once or when its grace window lapses, drops from a running checkpoint at its next
/// admission (`authority.revoke.immediate-retire`).
pub struct LivePins<C> {
    pins: StaticPins,
    ledger: Arc<LedgerFile>,
    clock: C,
}

impl<C: Clock + Send + Sync> LivePins<C> {
    pub fn new(pins: StaticPins, ledger: Arc<LedgerFile>, clock: C) -> LivePins<C> {
        LivePins { pins, ledger, clock }
    }
}

impl<C: Clock + Send + Sync> KeySource for LivePins<C> {
    fn keys(&self) -> Result<Arc<KeySet>, AuthorityError> {
        let ledger = self.ledger.read().map_err(|e| AuthorityError::KeySetUnavailable(e.to_string()))?;
        let now = self.clock.now();
        self.pins.retaining(|k| ledger.verifies(k, now))?.keys()
    }

    fn keys_after_signature_failure(&self) -> Result<Arc<KeySet>, AuthorityError> {
        self.keys()
    }
}

/// The revocation state a checkpoint reads now: the denylist file, and the ledger's
/// current scoped epochs.
pub fn revocation_state(denylist: Option<&Path>, ledger: &KeySetLedger) -> Result<RevocationState> {
    let mut revocation = RevocationState { epochs: ledger.current_epochs(), ..RevocationState::default() };
    if let Some(path) = denylist {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        revocation.denylist = parse_denylist(&text, STATIC_KEY_VERSION);
    }
    Ok(revocation)
}

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
    /// The key-set ledger holding retired keys and scoped epochs; absent,
    /// `.contextful/keyset.toml` under the project root.
    #[arg(long)]
    pub keyset: Option<PathBuf>,
}

impl AdmitArgs {
    /// Whether a credential accompanies the command in [`TOKEN_VAR`].
    pub fn presented() -> bool {
        token().is_some()
    }

    /// Admit the credential in [`TOKEN_VAR`] now, as presented over the stdio pipe the
    /// process inherited (`authority.verify.local-peer-fallback`), returning it with the
    /// revocation state later effect boundaries re-read.
    /// The ledger is the one under the root `--project` resolves to
    /// (`authority.issue.project-root`).
    pub fn admit(&self, project: Option<&str>, what: &str) -> Result<(AdmittedAuthority, RevocationState)> {
        let Some(token) = token() else {
            return Err(AdmitError::StdioCredentialMissing(format!(
                "{TOKEN_VAR} is unset: {what} admits one capability credential and acts on nothing without it"
            ))
            .into());
        };
        let now = SystemClock.now();
        let ledger = LedgerFile::at(&crate::project::root(project)?, self.keyset.as_deref()).read()?;
        let keys: Arc<KeySet> = live_pins(self.public_key.as_deref(), &ledger, now)?.keys()?;
        let revocation = revocation_state(self.denylist.as_deref(), &ledger)?;
        let authority = {
            let mut admission = Admission::new(now, &revocation);
            if let Some(aud) = self.audience.as_deref() {
                admission = admission.expecting(aud);
            }
            verify_inherited_pipe(&token, &keys, &admission)?
        };
        Ok((authority, revocation))
    }

    /// Who authors a table write verb's rows (`authority.verify.write-verbs`): under the
    /// manifest's posture, the credential in [`TOKEN_VAR`] when one accompanies the write,
    /// admitted and holding `write` over every table in `tables`; `None` lands unauthored.
    pub fn author(&self, project: Option<&str>, manifest: &str, tables: &[&str], what: &str) -> Result<Option<Author>> {
        let posture = AuthoringPosture::from_manifest(manifest)?;
        if token().is_none() {
            posture.unaccompanied(what)?;
            return Ok(None);
        }
        let (authority, _) = self.admit(project, what)?;
        for table in tables {
            authority.require_write(table)?;
        }
        Ok(Some(Author { authority, denylist: self.denylist.clone(), ledger: LedgerFile::at(&crate::project::root(project)?, self.keyset.as_deref()) }))
    }
}

/// The credential in [`TOKEN_VAR`], when one is set.
fn token() -> Option<String> {
    std::env::var(TOKEN_VAR).ok().filter(|t| !t.trim().is_empty())
}

/// The admitted credential a table write lands under.
pub struct Author {
    authority: AdmittedAuthority,
    denylist: Option<PathBuf>,
    ledger: LedgerFile,
}

impl Author {
    /// The principal every row the write lands carries as `_authored_by`.
    pub fn on_behalf_of(&self) -> Option<String> {
        self.authority.subject().on_behalf_of().map(str::to_string)
    }

    /// The commit boundary (`authority.verify.write-commit`): the denylist read afresh,
    /// and expiry, against the system clock.
    pub fn boundary(&self) -> Result<()> {
        let revocation = revocation_state(self.denylist.as_deref(), &self.ledger.read()?)?;
        effect_boundary(&self.authority, &Admission::new(SystemClock.now(), &revocation))?;
        Ok(())
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
