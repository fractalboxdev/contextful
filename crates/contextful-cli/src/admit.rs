//! What every credentialed subcommand resolves before its first effect: the one
//! credential, admitted against pinned keys, and the read face over the project.

use anyhow::{Context, Result};
use clap::ArgMatches;
use contextful_core::revoke::KeySetLedger;
use contextful_core::time::Instant;
use contextful_core::AuthorityError;
use contextful_policy::keyset::StaticPins;
#[cfg(feature = "read-plane")]
use contextful_policy::possession::NonceCache;
use contextful_policy::revoke::{parse_denylist, RevocationState};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
#[cfg(feature = "read-plane")]
pub use wired::*;

/// The variable `--public-key` falls back to (`authority.verify.pin-source`).
pub const PUBKEY_VAR: &str = "CONTEXTFUL_ISSUER_PUBKEY";

/// The variable `--audience` falls back to (`authority.verify.pin-source`).
pub const AUDIENCE_VAR: &str = "CONTEXTFUL_AUDIENCE";

/// The variable carrying a presented holder proof, kept out of the process arguments
/// (`authority.verify.local-proof-channel`).
#[cfg(feature = "read-plane")]
pub const DPOP_VAR: &str = "CONTEXTFUL_DPOP";

/// The holder seed path a verb signs with when neither `--holder-key` nor [`DPOP_VAR`]
/// supplies a proof (`authority.verify.local-proof-channel`).
#[cfg(feature = "read-plane")]
pub const HOLDER_KEY_VAR: &str = "CONTEXTFUL_HOLDER_KEY";

/// The command path the process parsed, such as `context land` or `mcp`: the target a
/// local holder proof covers (`authority.verify.local-proof-request`).
static COMMAND_PATH: OnceLock<String> = OnceLock::new();

/// Record the command path `matches` parsed, once per process.
pub fn record_command_path(matches: &ArgMatches) {
    let mut path = Vec::new();
    let mut at = matches;
    while let Some((name, sub)) = at.subcommand() {
        path.push(name);
        at = sub;
    }
    let _ = COMMAND_PATH.set(path.join(" "));
}

#[cfg(feature = "read-plane")]
fn command_path() -> Result<&'static str> {
    COMMAND_PATH.get().map(String::as_str).context("no command path is recorded, so no holder proof names a verb")
}

/// The value of the variable `var`, when set and not blank.
#[cfg(feature = "read-plane")]
fn env_value(var: &str) -> Option<String> {
    std::env::var(var).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// The refusals of admission over the process transport. `Display` begins with the
/// identifier.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdmitError {
    /// (`surface.package.stdio-credential`)
    #[cfg_attr(not(feature = "read-plane"), allow(dead_code))]
    #[error("StdioCredentialMissing: {0}")]
    StdioCredentialMissing(String),
    /// (`surface.package.owner-flag`)
    #[cfg(feature = "read-plane")]
    #[error("OwnerCredentialInvalid: {0}")]
    OwnerCredentialInvalid(String),
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

/// The project's holder-proof nonce store, relative to the project root: one JSON line per
/// retained nonce (`authority.verify.local-nonce-store`).
#[cfg(feature = "read-plane")]
pub const NONCE_STORE: &str = ".contextful/proof-nonces";

/// One retained nonce: the proof's `jti` and the Unix second its retention ends.
#[cfg(feature = "read-plane")]
#[derive(serde::Serialize, serde::Deserialize)]
struct NonceLine {
    nonce: String,
    until: i64,
}

/// The nonce store every invocation on one project checks its holder proof against, so a
/// proof admits once inside the replay window across processes
/// (`authority.verify.local-nonce-store`).
#[cfg(feature = "read-plane")]
struct NonceStore {
    path: PathBuf,
    lock: PathBuf,
}

#[cfg(feature = "read-plane")]
impl NonceStore {
    fn under(root: &Path) -> NonceStore {
        let path = root.join(NONCE_STORE);
        let lock = path.with_extension("lock");
        NonceStore { path, lock }
    }

    /// Run `check` over the cache the store holds at `now`, holding the store's lock file
    /// exclusively from read to write; the store is rewritten only when `check` admits. An
    /// unreadable or malformed store admits nothing.
    fn check(&self, now: Instant, check: impl FnOnce(&mut NonceCache) -> Result<()>) -> Result<()> {
        let unavailable = |why: String| anyhow::anyhow!("the proof nonce store {} is unavailable: {why}", self.path.display());
        let dir = self.path.parent().expect("the store path has a parent");
        std::fs::create_dir_all(dir).map_err(|e| unavailable(e.to_string()))?;
        let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&self.lock).map_err(|e| unavailable(e.to_string()))?;
        lock.lock().map_err(|e| unavailable(e.to_string()))?;
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(unavailable(e.to_string())),
        };
        let mut entries = Vec::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let NonceLine { nonce, until } = serde_json::from_str(line).map_err(|e| unavailable(e.to_string()))?;
            entries.push((nonce, Instant::from_unix_secs(until).map_err(|e| unavailable(e.to_string()))?));
        }
        let mut cache = NonceCache::restored(entries, now);
        check(&mut cache)?;
        let mut out = String::new();
        for (nonce, until) in cache.entries() {
            let line = NonceLine { nonce: nonce.to_owned(), until: until.unix_secs() };
            out.push_str(&serde_json::to_string(&line)?);
            out.push('\n');
        }
        let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|e| unavailable(e.to_string()))?;
        std::io::Write::write_all(&mut tmp, out.as_bytes()).map_err(|e| unavailable(e.to_string()))?;
        tmp.persist(&self.path).map_err(|e| unavailable(e.to_string()))?;
        Ok(())
    }
}

/// Admission and the read face, which reach the store and the run path.
#[cfg(feature = "read-plane")]
mod wired {
    use super::{command_path, env_value, live_pins, revocation_state, AdmitError, LedgerFile, NonceStore, AUDIENCE_VAR, DPOP_VAR, HOLDER_KEY_VAR, PUBKEY_VAR};
    use crate::clock::SystemClock;
    use crate::project::{open_face_with_pins, Located};
    use anyhow::{Context, Result};
    use contextful_context::read::Face;
    #[cfg(feature = "data-plane")]
    use contextful_core::issue::AuthoringPosture;
    use contextful_core::ports::{Clock, FixedClock};
    use contextful_core::time::Instant;
    use contextful_core::AuthorityError;
    use contextful_policy::enforce::mask::Pepper;
    use contextful_policy::keyset::{KeySet, KeySource, StaticPins};
    use contextful_policy::possession::{local_request, verify_proof, HolderKey};
    use contextful_policy::revoke::RevocationState;
    #[cfg(feature = "data-plane")]
    use contextful_policy::verify::effect_boundary;
    use contextful_policy::verify::{no_holder_proof, verify_local, Admission, AdmittedAuthority, LocalTransport};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    /// The environment variable carrying the credential, kept out of the process arguments.
    pub const TOKEN_VAR: &str = "CONTEXTFUL_TOKEN";

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
        /// An Ed25519 holder seed file the verb signs its own holder proof with, for a
        /// credential bound to a holder key. It beats a proof in `CONTEXTFUL_DPOP` and a seed in
        /// `CONTEXTFUL_HOLDER_KEY`.
        #[arg(long)]
        pub holder_key: Option<PathBuf>,
    }

    impl AdmitArgs {
        #[cfg(feature = "data-plane")]
        /// Whether a credential accompanies the command in [`TOKEN_VAR`].
        pub fn presented() -> bool {
            token().is_some()
        }

        /// Admit the credential in [`TOKEN_VAR`] now, as presented to the parsed command path
        /// over the stdio pipe the process inherited, returning it with the revocation state
        /// later effect boundaries re-read. A credential bound to a holder key admits only
        /// through the proof [`AdmitArgs::holder_proof`] resolves
        /// (`authority.verify.local-proof-channel`); one bound to none admits through the pipe
        /// (`authority.verify.local-peer-fallback`). The ledger is the one under the root
        /// `--project` resolves to (`authority.issue.project-root`).
        pub fn admit(&self, project: Option<&str>, what: &str) -> Result<(AdmittedAuthority, RevocationState)> {
            let Some(token) = token() else {
                return Err(AdmitError::StdioCredentialMissing(format!(
                    "{TOKEN_VAR} is unset: {what} admits one capability credential and acts on nothing without it"
                ))
                .into());
            };
            let now = SystemClock.now();
            let root = crate::root::root(project)?;
            let ledger = LedgerFile::at(&root, self.keyset.as_deref()).read()?;
            let keys: Arc<KeySet> = live_pins(self.public_key.as_deref(), &ledger, now)?.keys()?;
            let revocation = revocation_state(self.denylist.as_deref(), &ledger)?;
            let authority = {
                let mut admission = Admission::new(now, &revocation);
                if let Some(aud) = self.audience.as_deref() {
                    admission = admission.expecting(aud);
                }
                let holder_proof = |jkt: &str| self.holder_proof(jkt, now, &root);
                verify_local(&token, &keys, &admission, LocalTransport::InheritedPipe, holder_proof)?.into_authority()
            };
            Ok((authority, revocation))
        }

        /// Check the holder proof presented to the parsed command path at `now` for the
        /// credential binding `jkt`: one signed with `--holder-key`, else the one [`DPOP_VAR`]
        /// carries, else one signed with the seed [`HOLDER_KEY_VAR`] names; a blank variable is
        /// unset.
        fn holder_proof(&self, jkt: &str, now: Instant, root: &Path) -> Result<()> {
            let request = local_request(command_path()?);
            let sign = |path: &Path| -> Result<String> {
                let seed = std::fs::read_to_string(path).map_err(|e| {
                    AuthorityError::PossessionProofInvalid(format!("the holder seed {} is unreadable: {e}", path.display()))
                })?;
                Ok(HolderKey::from_seed(&seed)?.prove(&request, now))
            };
            let proof = match (&self.holder_key, env_value(DPOP_VAR), env_value(HOLDER_KEY_VAR)) {
                (Some(path), _, _) => sign(path)?,
                (None, Some(proof), _) => proof,
                (None, None, Some(path)) => sign(Path::new(&path))?,
                (None, None, None) => return no_holder_proof(jkt),
            };
            NonceStore::under(root).check(now, |nonces| Ok(verify_proof(jkt, &proof, &request, &FixedClock(now), nonces)?))
        }

        /// Who authors a table write verb's rows (`authority.verify.write-verbs`): under the
        /// manifest's posture, the credential in [`TOKEN_VAR`] when one accompanies the verb,
        /// admitted and holding `write` over every table in `tables`; `None` lands unauthored.
        #[cfg(feature = "data-plane")]
        pub fn author(&self, project: Option<&str>, manifest: &str, tables: &[&str]) -> Result<Option<Author>> {
            let posture = AuthoringPosture::from_manifest(manifest)?;
            let what = format!("`{}`", command_path()?);
            if token().is_none() {
                posture.unaccompanied(&what)?;
                return Ok(None);
            }
            let (authority, _) = self.admit(project, &what)?;
            for table in tables {
                authority.require_write(table)?;
            }
            Ok(Some(Author { authority, denylist: self.denylist.clone(), ledger: Arc::new(LedgerFile::at(&crate::root::root(project)?, self.keyset.as_deref())) }))
        }
    }

    /// The credential in [`TOKEN_VAR`], when one is set.
    fn token() -> Option<String> {
        std::env::var(TOKEN_VAR).ok().filter(|t| !t.trim().is_empty())
    }

    /// The admitted credential a table write lands under.
    #[cfg(feature = "data-plane")]
    #[derive(Clone)]
    pub struct Author {
        authority: AdmittedAuthority,
        denylist: Option<PathBuf>,
        ledger: Arc<LedgerFile>,
    }

    #[cfg(feature = "data-plane")]
    impl Author {
        /// The verified authority a derived source reads under.
        pub(crate) fn authority(&self) -> &AdmittedAuthority {
            &self.authority
        }

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
        face_with_pins(located, None, None)
    }

    pub fn face_with_pins(located: &Located, public_key: Option<&str>, keyset: Option<&Path>) -> Result<Face> {
        let pepper = Pepper::resolve(|k| std::env::var(k).ok());
        let declaration = &located.declaration;
        let manifest =
            std::fs::read_to_string(declaration).with_context(|| format!("reading the declaration `{}`", declaration.display()))?;
        let face = open_face_with_pins(&located.project, declaration, &manifest, pepper.clone(), public_key, keyset)?;
        if let Some(signal) = pepper.signal() {
            eprintln!("{signal}");
        }
        Ok(face)
    }

}
