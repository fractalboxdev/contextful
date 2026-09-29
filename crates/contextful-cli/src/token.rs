//! `contextful token` — the authority surface of the command line.
//!
//! Each subcommand is a thin adapter: it reads files and flags, calls the domain crate
//! and the policy crate, and prints. No authority rule lives here.

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use contextful_core::grant::{Action, Grant, TablePattern, TenantScope};
use contextful_core::identify::{MintSurface, Subject};
use contextful_core::issue::{IssuancePolicy, Lifetime, MintContext, MintRequest, NodeRole, SignatureAlgorithm};
use contextful_core::ports::{Clock, FixedClock, SigningPort};
use contextful_core::time::Instant;
use contextful_policy::attenuate::{attenuate, Derivation};
use contextful_policy::issue::{mint, MintClaims, SeedSigner};
use contextful_policy::keyset::KeySource;
use contextful_policy::revoke::{parse_denylist, RevocationState};
use contextful_policy::verify::{introspect, verify_inherited_pipe, Admission};
use std::path::{Path, PathBuf};

/// The key version a denylist entry records for credentials verified under static pins.
const STATIC_KEY_VERSION: &str = "static";

#[derive(Subcommand)]
pub enum TokenCmd {
    /// Generate an issuer signing key; print its public key as a verifier pin.
    Keygen {
        /// Seed file to write; an existing file is never overwritten.
        #[arg(long)]
        out: PathBuf,
        /// `Ed25519` (default) or `ES256`.
        #[arg(long, default_value = "Ed25519")]
        algorithm: String,
    },
    /// Mint a credential under the project's persisted issuance policy.
    Mint {
        /// Seed file holding the issuer key.
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
    },
    /// Print a credential's declared scope without verifying it.
    Introspect {
        #[arg(long)]
        token: String,
    },
}

pub fn run(cmd: TokenCmd) -> Result<()> {
    match cmd {
        TokenCmd::Keygen { out, algorithm } => keygen(&out, &algorithm),
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
        TokenCmd::Verify { token, public_key, audience, at, denylist } => {
            let keys = crate::admit::static_pins(public_key.as_deref())?.keys()?;
            let at = instant_or_now(at.as_deref())?;
            let mut revocation = RevocationState::default();
            if let Some(path) = denylist {
                let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
                revocation.denylist = parse_denylist(&text, STATIC_KEY_VERSION);
            }
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

fn keygen(out: &Path, algorithm: &str) -> Result<()> {
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
    write_private(out, &signer.seed())?;
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
    let signer = SeedSigner::resolve(issuer_key)?;
    let text = std::fs::read_to_string(IssuancePolicy::PATH)
        .with_context(|| format!("no issuance policy at {}", IssuancePolicy::PATH))?;
    let policy = IssuancePolicy::parse(&text).map_err(|e| anyhow::anyhow!("{e:?}"))?;
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
    Ok(mint(&plan, &MintClaims { confirmation: holder, ..MintClaims::default() }, &signer)?)
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
