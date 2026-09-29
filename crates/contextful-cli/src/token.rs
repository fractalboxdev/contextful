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
use contextful_policy::attenuate::{attenuate, Derivation};
use contextful_policy::issue::{mint, MintClaims, SeedSigner, DEFAULT_SEED_PATH};
use contextful_policy::keyset::KeySource;
use contextful_policy::revoke::{mint_epoch, PRINCIPAL_CLASS_DELEGATED, PRINCIPAL_CLASS_UNATTRIBUTED};
use contextful_policy::verify::{introspect, verify_inherited_pipe, Admission, BEARER_LIFETIME_SECS};
use std::path::{Path, PathBuf};

#[derive(Subcommand)]
pub enum TokenCmd {
    /// Generate an issuer signing key; print its public key as a verifier pin.
    Keygen {
        /// Seed file to write; an existing file is never overwritten.
        #[arg(long, default_value = DEFAULT_SEED_PATH)]
        out: PathBuf,
        /// `Ed25519` (default) or `ES256`.
        #[arg(long, default_value = "Ed25519")]
        algorithm: String,
        /// The instant the key begins signing (RFC 3339); absent reads the system clock.
        #[arg(long)]
        now: Option<String>,
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
        TokenCmd::Keygen { out, algorithm, now } => keygen(&out, &algorithm, now.as_deref()),
        TokenCmd::Policy(cmd) => policy(cmd),
        TokenCmd::Revoke { audience, tenant, principal_class } => {
            let project = match audience {
                Some(a) => a,
                None => read_policy()?.default_audience,
            };
            if let Some(class) = principal_class.as_deref() {
                if ![PRINCIPAL_CLASS_DELEGATED, PRINCIPAL_CLASS_UNATTRIBUTED].contains(&class) {
                    bail!("`--principal-class {class}` is `{PRINCIPAL_CLASS_DELEGATED}` or `{PRINCIPAL_CLASS_UNATTRIBUTED}`");
                }
            }
            let mut ledger = crate::admit::ledger(None)?;
            let epoch = ledger.bump(EpochScope { project, tenant, principal_class });
            write_ledger(&ledger)?;
            println!("epoch {epoch}");
            Ok(())
        }
        TokenCmd::Rotate { compromise, grace_secs, now } => rotate(compromise, grace_secs, now.as_deref()),
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
            let ledger = crate::admit::ledger(keyset.as_deref())?;
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
    }
}

fn read_policy() -> Result<IssuancePolicy> {
    let text = std::fs::read_to_string(IssuancePolicy::PATH)
        .with_context(|| format!("no issuance policy at {}", IssuancePolicy::PATH))?;
    IssuancePolicy::parse(&text).map_err(|e| anyhow::anyhow!("{e:?}"))
}

fn write_ledger(ledger: &KeySetLedger) -> Result<()> {
    write_atomic(Path::new(KeySetLedger::PATH), &ledger.to_toml(), false)
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

fn policy(cmd: PolicyCmd) -> Result<()> {
    let path = Path::new(IssuancePolicy::PATH);
    match cmd {
        PolicyCmd::Init { audience, max_lifetime_secs } => {
            if path.exists() {
                bail!("{} exists; an issuance policy is never overwritten, and `token policy lower-ceiling` lowers its ceiling", path.display());
            }
            let policy = IssuancePolicy { default_audience: audience, max_lifetime_secs, lowered: Vec::new() };
            let text = policy.to_toml();
            IssuancePolicy::parse(&text).map_err(|e| anyhow::anyhow!("{e:?}"))?;
            write_atomic(path, &text, false)?;
            println!("{}: default audience {}, ceiling {} s", path.display(), policy.default_audience, policy.max_lifetime_secs);
        }
        PolicyCmd::LowerCeiling { max_lifetime_secs, now } => {
            let mut policy = read_policy()?;
            if max_lifetime_secs >= policy.max_lifetime_secs {
                bail!("{max_lifetime_secs} s does not lower the ceiling of {} s", policy.max_lifetime_secs);
            }
            policy.lower_ceiling(max_lifetime_secs, instant_or_now(now.as_deref())?).map_err(|e| anyhow::anyhow!("{e:?}"))?;
            write_atomic(path, &policy.to_toml(), false)?;
            println!("{}: ceiling {} s", path.display(), policy.max_lifetime_secs);
        }
    }
    Ok(())
}

/// Replace the seed at [`DEFAULT_SEED_PATH`] when rotation is due, or at once on
/// suspected compromise: the old key retires after the grace window, or at once with a
/// project-wide epoch bump (`authority.revoke.compromise`).
fn rotate(compromise: bool, grace_secs: Option<u64>, now: Option<&str>) -> Result<()> {
    let now = instant_or_now(now)?;
    let policy = read_policy()?;
    let rotation = grace_secs.map_or_else(RotationPolicy::default, |g| RotationPolicy::default().with_grace(g));
    if !compromise {
        rotation.validate(&policy, now)?;
    }
    let seed = Path::new(DEFAULT_SEED_PATH);
    let old = SeedSigner::resolve(Some(seed))?;
    let old_key = old.public_key_text();
    let mut ledger = crate::admit::ledger(None)?;
    if let Some(since) = ledger.version(&old_key).map(|k| k.since) {
        if !key_rotation_due(since, now, compromise) {
            bail!(
                "the issuer key signing since {since} falls due for rotation at {}; pass --compromise on suspected compromise",
                since.plus_secs(ISSUER_KEY_ROTATION_CADENCE_SECS)
            );
        }
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
    write_atomic(seed, &new.seed(), true)?;
    write_ledger(&ledger)?;
    println!("{new_key}");
    Ok(())
}

fn keygen(out: &Path, algorithm: &str, now: Option<&str>) -> Result<()> {
    let Some(algorithm) = SignatureAlgorithm::parse(algorithm) else {
        bail!("unknown signature scheme `{algorithm}`; use Ed25519 or ES256");
    };
    if out.exists() {
        bail!("{} exists; a seed file is never overwritten", out.display());
    }
    let signer = SeedSigner::generate(algorithm);
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let since = instant_or_now(now)?;
    write_private(out, &signer.seed())?;
    // The default seed's key enters the ledger, which dates its rotation.
    if out == Path::new(DEFAULT_SEED_PATH) {
        let mut ledger = crate::admit::ledger(None)?;
        ledger.record(&signer.public_key_text(), since);
        write_ledger(&ledger)?;
    }
    println!("{}", signer.public_key_text());
    Ok(())
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
    let default = Path::new(DEFAULT_SEED_PATH);
    let signer = SeedSigner::resolve(issuer_key.or_else(|| default.exists().then_some(default)))?;
    let policy = read_policy()?;
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
    let epoch = mint_epoch(&plan, &crate::admit::ledger(None)?.current_epochs());
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
