//! `contextful token` — the authority surface of the command line.
//!
//! Each subcommand is a thin adapter: it reads files and flags, calls the domain crate
//! and the policy crate, and prints. No authority rule lives here.

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use contextful_core::grant::{Action, Grant, TablePattern, TenantScope};
use contextful_core::identify::{MintSurface, Subject};
use contextful_core::issue::{
    key_rotation_due, IssuancePolicy, Lifetime, MintContext, MintRequest, NodeRole, SignatureAlgorithm,
    ISSUER_KEY_ROTATION_CADENCE_SECS,
};
use contextful_core::revoke::{EpochScope, KeySetLedger, RotationPolicy};
use contextful_core::ports::{Clock, FixedClock, SigningPort};
use contextful_core::time::Instant;
use contextful_core::exchange::ExchangePolicy;
use contextful_policy::attenuate::{attenuate, Derivation};
use contextful_policy::exchange::{redeem, Exchange};
use contextful_policy::issue::{mint, MintClaims, SeedSigner, DEFAULT_SEED_PATH};
use contextful_policy::possession::HolderKey;
use contextful_policy::keyset::KeySource;
use contextful_policy::possession::ProofChecker;
use crate::admit::LedgerFile;
use contextful_policy::revoke::{mint_epoch, PRINCIPAL_CLASS_DELEGATED, PRINCIPAL_CLASS_UNATTRIBUTED};
use contextful_policy::verify::{introspect, verify_inherited_pipe, Admission, BEARER_LIFETIME_SECS};
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Subcommand)]
pub enum TokenCmd {
    /// Generate an issuer signing key and print its public key as a verifier pin; with
    /// `--holder`, an Ed25519 holder key and its thumbprint for `token mint --holder`.
    Keygen {
        /// Seed file to write; absent, `.contextful/issuer.seed` under the project root. An
        /// existing file is never overwritten.
        #[arg(long)]
        out: Option<PathBuf>,
        /// `Ed25519` (default) or `ES256`.
        #[arg(long, default_value = "Ed25519", conflicts_with = "holder")]
        algorithm: String,
        /// The instant the key begins signing (RFC 3339); absent reads the system clock.
        #[arg(long, conflicts_with = "holder")]
        now: Option<String>,
        /// Write a holder seed to `--out` and print its RFC 7638 thumbprint
        /// (`authority.verify.holder-keygen`).
        #[arg(long, requires = "out")]
        holder: bool,
    },
    /// Write or lower the project's persisted issuance policy.
    #[command(subcommand)]
    Policy(PolicyCmd),
    /// Bump one scoped revocation epoch, withdrawing every credential minted under an
    /// earlier epoch of that scope.
    Revoke {
        /// The project scope; absent, the issuance policy's default audience.
        #[arg(long)]
        audience: Option<String>,
        /// Narrow the bump to one tenant.
        #[arg(long)]
        tenant: Option<String>,
        /// Narrow the bump to `delegated` or `unattributed` credentials.
        #[arg(long)]
        principal_class: Option<String>,
    },
    /// Date the default seed's key in the key-set ledger, so rotation falls due 90 d on.
    Record {
        /// The instant the key began signing (RFC 3339).
        #[arg(long)]
        since: String,
    },
    /// Replace the issuer key once rotation is due, or at once on suspected compromise.
    Rotate {
        /// Retire the key at once and bump the project-wide epoch.
        #[arg(long)]
        compromise: bool,
        /// The retiring key's grace window in seconds; absent, the issuance bound.
        #[arg(long)]
        grace_secs: Option<u64>,
        /// The rotation instant (RFC 3339); absent reads the system clock.
        #[arg(long)]
        now: Option<String>,
    },
    /// Mint a credential under the project's persisted issuance policy.
    Mint {
        /// Seed file holding the issuer key; absent, `.contextful/issuer.seed`.
        #[arg(long)]
        issuer_key: Option<PathBuf>,
        #[arg(long)]
        on_behalf_of: Option<String>,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        zone: Option<String>,
        #[arg(long)]
        incognito: bool,
        /// Repeatable; absent mints `read` alone.
        #[arg(long = "action")]
        actions: Vec<String>,
        /// Repeatable table pattern.
        #[arg(long = "table")]
        tables: Vec<String>,
        /// `<table>=<value>`: scope the grant to one tenant of that table.
        #[arg(long)]
        tenant: Option<String>,
        /// Repeatable template identifier; absent confers none.
        #[arg(long = "template")]
        templates: Vec<String>,
        #[arg(long)]
        max_rows: Option<u64>,
        #[arg(long)]
        max_duration_ms: Option<u64>,
        #[arg(long)]
        max_response_bytes: Option<u64>,
        /// Lifetime in seconds; absent takes the persisted ceiling.
        #[arg(long)]
        ttl: Option<u64>,
        #[arg(long)]
        audience: Option<String>,
        /// The RFC 7638 thumbprint of the holder's Ed25519 public key, bound as the
        /// credential's confirmation claim (`authority.verify.possession-binding`).
        #[arg(long)]
        holder: Option<String>,
        /// Issue instant (RFC 3339); absent reads the system clock.
        #[arg(long)]
        now: Option<String>,
    },
    /// Derive a narrower child offline by appending a block.
    Attenuate {
        #[arg(long)]
        token: String,
        /// Repeatable; replaces each grant's actions.
        #[arg(long = "action")]
        actions: Vec<String>,
        /// Repeatable; replaces each grant's table patterns.
        #[arg(long = "table")]
        tables: Vec<String>,
        /// Child expiry (RFC 3339); absent inherits the parent's.
        #[arg(long)]
        expires: Option<String>,
        /// Turn incognito on; a derivation never turns it off.
        #[arg(long)]
        incognito: bool,
    },
    /// Admit a credential and print the admitted authority.
    Verify {
        #[arg(long)]
        token: String,
        /// Comma-separated issuer key pins.
        #[arg(long, env = crate::admit::PUBKEY_VAR)]
        public_key: Option<String>,
        /// Expected audience; absent performs no audience check.
        #[arg(long, env = crate::admit::AUDIENCE_VAR)]
        audience: Option<String>,
        /// Evaluation instant (RFC 3339); absent reads the system clock.
        #[arg(long)]
        at: Option<String>,
        /// A file of revocation identifiers, one per line.
        #[arg(long)]
        denylist: Option<PathBuf>,
        /// The key-set ledger holding retired keys and scoped epochs; absent,
        /// `.contextful/keyset.toml` when it exists.
        #[arg(long)]
        keyset: Option<PathBuf>,
    },
    /// Print a credential's declared scope without verifying it.
    Introspect {
        #[arg(long)]
        token: String,
    },
    /// Trade a signed external assertion for a credential under the project's exchange
    /// policy; the same exchange a served face answers at `POST /auth/exchange`.
    Exchange {
        /// The assertion; absent, read from standard input.
        #[arg(long)]
        jwt: Option<String>,
        /// A holder possession proof over `POST /auth/exchange` and the body
        /// `{"jwt":"<assertion>"}`; the credential binds the proof key. Absent, the
        /// credential is a bearer.
        #[arg(long)]
        dpop: Option<String>,
        /// Seed file holding the issuer key.
        #[arg(long)]
        issuer_key: Option<PathBuf>,
        /// Mint instant (RFC 3339); absent reads the system clock.
        #[arg(long)]
        now: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum PolicyCmd {
    /// Write `.contextful/issuance.toml`; an existing policy is never overwritten.
    Init {
        /// The audience a mint names by default.
        #[arg(long)]
        audience: String,
        /// The longest lifetime any mint produces, in seconds.
        #[arg(long, default_value_t = BEARER_LIFETIME_SECS)]
        max_lifetime_secs: u64,
    },
    /// Lower the ceiling, recording the value it replaces and the instant.
    LowerCeiling {
        #[arg(long)]
        max_lifetime_secs: u64,
        /// The lowering instant (RFC 3339); absent reads the system clock.
        #[arg(long)]
        now: Option<String>,
    },
}

pub fn run(cmd: TokenCmd) -> Result<()> {
    match cmd {
        TokenCmd::Keygen { out: Some(out), holder: true, .. } => {
            let key = HolderKey::generate();
            write_seed(&out, &key.seed())?;
            println!("{}", key.thumbprint());
            Ok(())
        }
        TokenCmd::Keygen { out, algorithm, now, .. } => keygen(&Root::find()?, out.as_deref(), &algorithm, now.as_deref()),
        TokenCmd::Policy(cmd) => policy(&Root::find()?, cmd),
        TokenCmd::Revoke { audience, tenant, principal_class } => {
            let root = Root::find()?;
            if let Some(class) = principal_class.as_deref() {
                if ![PRINCIPAL_CLASS_DELEGATED, PRINCIPAL_CLASS_UNATTRIBUTED].contains(&class) {
                    return Err(TokenError::RevocationPrincipalClassUnknown(format!(
                        "`--principal-class {class}` is `{PRINCIPAL_CLASS_DELEGATED}` or `{PRINCIPAL_CLASS_UNATTRIBUTED}`"
                    ))
                    .into());
                }
            }
            let project = match audience {
                Some(a) => a,
                None => root.policy()?.default_audience,
            };
            let mut ledger = root.ledger().read()?;
            let epoch = ledger.bump(EpochScope { project, tenant, principal_class });
            root.write_ledger(&ledger)?;
            println!("epoch {epoch}");
            Ok(())
        }
        TokenCmd::Record { since } => {
            let root = Root::find()?;
            let since = Instant::parse(&since)?;
            let key = SeedSigner::resolve(Some(&root.seed()))?.public_key_text();
            let mut ledger = root.ledger().read()?;
            ledger.record(&key, since);
            root.write_ledger(&ledger)?;
            let since = ledger.version(&key).map_or(since, |k| k.since);
            println!("{key}: signing since {since}");
            Ok(())
        }
        TokenCmd::Rotate { compromise, grace_secs, now } => rotate(&Root::find()?, compromise, grace_secs, now.as_deref()),
        TokenCmd::Mint {
            issuer_key,
            on_behalf_of,
            agent,
            host,
            task,
            zone,
            incognito,
            actions,
            tables,
            tenant,
            templates,
            max_rows,
            max_duration_ms,
            max_response_bytes,
            ttl,
            audience,
            holder,
            now,
        } => {
            let subject = Subject { on_behalf_of, agent, host, task, zone, incognito };
            let grant = Grant {
                actions: parse_actions(&actions)?,
                tables: parse_tables(&tables)?,
                tenant: tenant.as_deref().map(parse_tenant).transpose()?,
                aggregate: None,
                templates: (!templates.is_empty()).then_some(templates),
                max_rows,
                max_duration_ms,
                max_response_bytes,
            };
            let lifetime = ttl.map_or(Lifetime::Default, Lifetime::Requested);
            let token = mint_one(issuer_key.as_deref(), subject, grant, lifetime, audience, holder, now.as_deref())?;
            println!("{token}");
            Ok(())
        }
        TokenCmd::Attenuate { token, actions, tables, expires, incognito } => {
            let parent = introspect(&token)?;
            let grants = parent.blocks.iter().rev().find_map(|h| h.grants.clone()).unwrap_or(parent.authority.grants);
            let actions = parse_actions(&actions)?;
            let tables = parse_tables(&tables)?;
            let mut derivation = Derivation::narrowing(
                &grants,
                (!actions.is_empty()).then_some(actions.as_slice()),
                (!tables.is_empty()).then_some(tables.as_slice()),
            );
            derivation.expires_at = expires.as_deref().map(Instant::parse).transpose()?;
            if incognito {
                derivation.subject.incognito = Some(true);
            }
            println!("{}", attenuate(&token, &derivation)?);
            Ok(())
        }
        TokenCmd::Verify { token, public_key, audience, at, denylist, keyset } => {
            let at = instant_or_now(at.as_deref())?;
            let ledger = LedgerFile::at(&Root::find()?.dir, keyset.as_deref()).read()?;
            let keys = crate::admit::live_pins(public_key.as_deref(), &ledger, at)?.keys()?;
            let revocation = crate::admit::revocation_state(denylist.as_deref(), &ledger)?;
            let mut admission = Admission::new(at, &revocation);
            if let Some(aud) = audience.as_deref() {
                admission = admission.expecting(aud);
            }
            println!("{}", verify_inherited_pipe(&token, &keys, &admission)?.to_json());
            Ok(())
        }
        TokenCmd::Introspect { token } => {
            println!("{}", introspect(&token)?.to_json());
            Ok(())
        }
        TokenCmd::Exchange { jwt, dpop, issuer_key, now } => {
            let assertion = match jwt {
                Some(jwt) => jwt,
                None => {
                    let mut text = String::new();
                    std::io::stdin().read_to_string(&mut text).context("reading the assertion from standard input")?;
                    text
                }
            };
            let assertion = assertion.trim();
            if assertion.is_empty() {
                bail!("no assertion: pass --jwt or write it to standard input");
            }
            let root = Root::find()?;
            let exchange = configured_exchange(&root.dir)?;
            let default = root.seed();
            let signer = SeedSigner::resolve(issuer_key.as_deref().or_else(|| default.exists().then_some(default.as_path())))?;
            let issuance = root.policy()?;
            let clock = FixedClock(instant_or_now(now.as_deref())?);
            let ctx = MintContext { node: NodeRole::Primary, signer: &signer as &dyn SigningPort, clock: &clock as &dyn Clock };
            // The served faces' wire body, through the same ceiling, parse and proof check.
            let body = serde_json::json!({ "jwt": assertion }).to_string();
            let proofs = ProofChecker::new(clock);
            println!("{}", redeem(exchange.as_ref(), body.as_bytes(), dpop.as_deref(), &proofs, &issuance, &ctx)?);
            Ok(())
        }
    }
}

/// The refusals of the key and policy writers. `Display` begins with the identifier.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    /// (`authority.issue.policy-init`)
    #[error("IssuancePolicyExists: {0}")]
    IssuancePolicyExists(String),
    /// (`authority.issue.lower-only`)
    #[error("IssuanceCeilingNotLowered: {0}")]
    IssuanceCeilingNotLowered(String),
    /// (`authority.issue.rotation-not-due`)
    #[error("IssuerKeyRotationNotDue: {0}")]
    IssuerKeyRotationNotDue(String),
    /// (`authority.issue.unrecorded-key`)
    #[error("IssuerKeyUnrecorded: {0}")]
    IssuerKeyUnrecorded(String),
    /// (`authority.revoke.unknown-principal-class`)
    #[error("RevocationPrincipalClassUnknown: {0}")]
    RevocationPrincipalClassUnknown(String),
}

/// The project root the issuance policy, the default seed and the key-set ledger sit
/// under (`authority.issue.project-root`).
struct Root {
    dir: PathBuf,
}

impl Root {
    fn find() -> Result<Root> {
        Ok(Root { dir: crate::root::root(None)? })
    }

    fn policy_path(&self) -> PathBuf {
        self.dir.join(IssuancePolicy::PATH)
    }

    fn seed(&self) -> PathBuf {
        self.dir.join(DEFAULT_SEED_PATH)
    }

    fn ledger(&self) -> LedgerFile {
        LedgerFile::at(&self.dir, None)
    }

    fn policy(&self) -> Result<IssuancePolicy> {
        let path = self.policy_path();
        let text = std::fs::read_to_string(&path).with_context(|| format!("no issuance policy at {}", path.display()))?;
        IssuancePolicy::parse(&text).map_err(|e| anyhow::anyhow!("{e:?}"))
    }

    fn write_ledger(&self, ledger: &KeySetLedger) -> Result<()> {
        write_atomic(self.ledger().path(), &ledger.to_toml(), false)
    }

    /// Whether `path` names this root's default seed, however it is spelled.
    fn is_default_seed(&self, path: &Path) -> bool {
        match (std::fs::canonicalize(path), std::fs::canonicalize(self.seed())) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }
}

/// Write `text` to `path` through a sibling file renamed into place, so a reader sees
/// the old content or the new and never a torn one.
fn write_atomic(path: &Path, text: &str, private: bool) -> Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let staged = path.with_extension("staged");
    let _ = std::fs::remove_file(&staged);
    if private {
        write_private(&staged, text)?;
    } else {
        std::fs::write(&staged, text)?;
    }
    std::fs::rename(&staged, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn policy(root: &Root, cmd: PolicyCmd) -> Result<()> {
    let path = root.policy_path();
    match cmd {
        PolicyCmd::Init { audience, max_lifetime_secs } => {
            if path.exists() {
                return Err(TokenError::IssuancePolicyExists(format!(
                    "{} exists; an issuance policy is never overwritten, and `token policy lower-ceiling` lowers its ceiling",
                    path.display()
                ))
                .into());
            }
            let policy = IssuancePolicy { default_audience: audience, max_lifetime_secs, lowered: Vec::new() };
            let text = policy.to_toml();
            IssuancePolicy::parse(&text).map_err(|e| anyhow::anyhow!("{e:?}"))?;
            write_atomic(&path, &text, false)?;
            println!("{}: default audience {}, ceiling {} s", path.display(), policy.default_audience, policy.max_lifetime_secs);
        }
        PolicyCmd::LowerCeiling { max_lifetime_secs, now } => {
            let mut policy = root.policy()?;
            if max_lifetime_secs >= policy.max_lifetime_secs {
                return Err(TokenError::IssuanceCeilingNotLowered(format!(
                    "{max_lifetime_secs} s does not lower the ceiling of {} s",
                    policy.max_lifetime_secs
                ))
                .into());
            }
            policy.lower_ceiling(max_lifetime_secs, instant_or_now(now.as_deref())?).map_err(|e| anyhow::anyhow!("{e:?}"))?;
            write_atomic(&path, &policy.to_toml(), false)?;
            println!("{}: ceiling {} s", path.display(), policy.max_lifetime_secs);
        }
    }
    Ok(())
}

/// Replace the default seed when rotation is due, or at once on suspected compromise:
/// the old key retires after the grace window, or at once with a project-wide epoch bump
/// (`authority.revoke.compromise`). A scheduled rotation answers to the date the ledger
/// records for the key; an undated key refuses (`authority.issue.unrecorded-key`).
fn rotate(root: &Root, compromise: bool, grace_secs: Option<u64>, now: Option<&str>) -> Result<()> {
    let now = instant_or_now(now)?;
    let policy = root.policy()?;
    let rotation = grace_secs.map_or_else(RotationPolicy::default, |g| RotationPolicy::default().with_grace(g));
    let seed = root.seed();
    let old = SeedSigner::resolve(Some(&seed))?;
    let old_key = old.public_key_text();
    let mut ledger = root.ledger().read()?;
    if !compromise {
        let Some(since) = ledger.version(&old_key).map(|k| k.since) else {
            return Err(TokenError::IssuerKeyUnrecorded(format!(
                "the key-set ledger does not date the issuer key {old_key}; run `contextful token record --since <the instant it began signing>`, or pass --compromise on suspected compromise"
            ))
            .into());
        };
        if !key_rotation_due(since, now, false) {
            return Err(TokenError::IssuerKeyRotationNotDue(format!(
                "the issuer key signing since {since} falls due for rotation at {}; pass --compromise on suspected compromise",
                since.plus_secs(ISSUER_KEY_ROTATION_CADENCE_SECS)
            ))
            .into());
        }
        rotation.validate(&policy, now)?;
    }
    let new = SeedSigner::generate(old.algorithm());
    let new_key = new.public_key_text();
    if compromise {
        ledger.retire_now(&old_key, now);
        ledger.bump(EpochScope { project: policy.default_audience.clone(), tenant: None, principal_class: None });
    } else {
        ledger.retire_after_grace(&old_key, now, &rotation);
    }
    ledger.record(&new_key, now);
    // The ledger lands first: a crash between the writes leaves the old seed signing under
    // a ledger that already dates the new key, never a new seed the ledger does not date.
    root.write_ledger(&ledger)?;
    write_atomic(&seed, &new.seed(), true)?;
    println!("{new_key}");
    Ok(())
}

/// The project's configured exchange under `root`: `None` when it declares no policy at
/// [`ExchangePolicy::PATH`]; an absent material file configures no material.
pub(crate) fn configured_exchange(root: &Path) -> Result<Option<Exchange>> {
    let path = root.join(ExchangePolicy::PATH);
    let policy = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let key = root.join(ExchangePolicy::VERIFY_KEY_PATH);
    let material = match std::fs::read(&key) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", key.display())),
    };
    Ok(Some(Exchange::from_config(&policy, &material).map_err(|e| anyhow::anyhow!("{e:?}"))?))
}

fn keygen(root: &Root, out: Option<&Path>, algorithm: &str, now: Option<&str>) -> Result<()> {
    let Some(algorithm) = SignatureAlgorithm::parse(algorithm) else {
        bail!("unknown signature scheme `{algorithm}`; use Ed25519 or ES256");
    };
    let out = out.map_or_else(|| root.seed(), Path::to_path_buf);
    let signer = SeedSigner::generate(algorithm);
    let since = instant_or_now(now)?;
    write_seed(&out, &signer.seed())?;
    // The default seed's key enters the ledger, which dates its rotation.
    if root.is_default_seed(&out) {
        let mut ledger = root.ledger().read()?;
        ledger.record(&signer.public_key_text(), since);
        root.write_ledger(&ledger)?;
    }
    println!("{}", signer.public_key_text());
    Ok(())
}

/// Write `seed` to a new owner-only file at `out`; an existing file is never overwritten.
fn write_seed(out: &Path, seed: &str) -> Result<()> {
    if out.exists() {
        bail!("{} exists; a seed file is never overwritten", out.display());
    }
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    write_private(out, seed)
}

#[cfg(unix)]
fn write_private(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    f.write_all(text.as_bytes())?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, text: &str) -> Result<()> {
    std::fs::write(path, text)?;
    Ok(())
}

fn mint_one(
    issuer_key: Option<&Path>,
    subject: Subject,
    grant: Grant,
    lifetime: Lifetime,
    audience: Option<String>,
    holder: Option<String>,
    now: Option<&str>,
) -> Result<String> {
    let root = Root::find()?;
    let default = root.seed();
    let signer = SeedSigner::resolve(issuer_key.or_else(|| default.exists().then_some(default.as_path())))?;
    let policy = root.policy()?;
    let subject = subject.mint(MintSurface::CommandLine)?.to_subject();
    let mut request = MintRequest::custody(subject, vec![grant]);
    request.lifetime = lifetime;
    request.audience = audience;
    let clock = FixedClock(instant_or_now(now)?);
    let ctx = MintContext { node: NodeRole::Primary, signer: &signer as &dyn SigningPort, clock: &clock as &dyn Clock };
    let plan = policy.check(&request, &ctx)?;
    if let Some(jkt) = holder.as_deref() {
        let alphabet = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
        if jkt.len() != 43 || !jkt.chars().all(alphabet) {
            bail!("`--holder` is the 43-character base64url RFC 7638 thumbprint of an Ed25519 public key, not `{jkt}`");
        }
    }
    let epoch = mint_epoch(&plan, &root.ledger().read()?.current_epochs());
    Ok(mint(&plan, &MintClaims { confirmation: holder, epoch }, &signer)?)
}

fn instant_or_now(text: Option<&str>) -> Result<Instant> {
    match text {
        Some(t) => Ok(Instant::parse(t)?),
        None => {
            let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
            Ok(Instant::from_unix_secs(secs as i64)?)
        }
    }
}

fn parse_actions(names: &[String]) -> Result<Vec<Action>> {
    Ok(names.iter().map(|a| Action::parse(a)).collect::<Result<_, _>>()?)
}

fn parse_tables(patterns: &[String]) -> Result<Vec<TablePattern>> {
    Ok(patterns.iter().map(|p| TablePattern::parse(p)).collect::<Result<_, _>>()?)
}

fn parse_tenant(text: &str) -> Result<TenantScope> {
    match text.split_once('=') {
        Some((table, value)) if !table.is_empty() && !value.is_empty() => {
            Ok(TenantScope { table: table.to_string(), value: value.to_string() })
        }
        _ => bail!("`--tenant {text}` is not <table>=<value>"),
    }
}
