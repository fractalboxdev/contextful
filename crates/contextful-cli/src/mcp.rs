//! `contextful mcp` — the tool server over standard input and output.
//!
//! A thin adapter: it admits the one credential the process serves, resolves the pepper,
//! opens the read face over the project's store and manifest, and hands both to the tool
//! server. Every value is resolved before the first protocol line is written, so a
//! process that cannot serve exits with nothing on standard output.

use anyhow::{bail, Context, Result};
use contextful_agent::mcp::Server;
use contextful_context::read::Face;
use contextful_context::Store;
use contextful_core::time::Instant;
use contextful_core::AuthorityError;
use contextful_policy::enforce::mask::Pepper;
use contextful_policy::keyset::{KeySource, StaticPins};
use contextful_policy::revoke::{parse_denylist, RevocationState};
use contextful_policy::verify::{effect_boundary, verify, Admission, AdmittedAuthority};
use std::path::PathBuf;

/// The environment variable carrying the credential, kept out of the process arguments.
const TOKEN_VAR: &str = "CONTEXTFUL_TOKEN";

/// The key version a denylist entry records for credentials verified under static pins.
const STATIC_KEY_VERSION: &str = "static";

#[derive(clap::Args)]
pub struct McpArgs {
    /// The project whose store root is `.contextful/context/<project>/`.
    #[arg(long)]
    project: String,
    /// The pipeline manifest holding the table declarations and query templates.
    #[arg(long, default_value = "contextful.toml")]
    declaration: PathBuf,
    /// Comma-separated issuer key pins.
    #[arg(long)]
    public_key: String,
    /// Expected audience; absent performs no audience check.
    #[arg(long)]
    audience: Option<String>,
    /// A file of revocation identifiers, one per line.
    #[arg(long)]
    denylist: Option<PathBuf>,
}

fn now() -> Result<Instant> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
    Ok(Instant::from_unix_nanos(i128::try_from(nanos)?)?)
}

pub fn run(args: McpArgs) -> Result<()> {
    let Some(token) = std::env::var(TOKEN_VAR).ok().filter(|t| !t.trim().is_empty()) else {
        bail!("{TOKEN_VAR} is unset: the tool server admits one capability credential and serves nothing without it");
    };
    let keys = StaticPins::parse(&args.public_key)?.keys()?;
    let mut revocation = RevocationState::default();
    if let Some(path) = &args.denylist {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        revocation.denylist = parse_denylist(&text, STATIC_KEY_VERSION);
    }
    let mut admission = Admission::new(now()?, &revocation);
    if let Some(aud) = args.audience.as_deref() {
        admission = admission.expecting(aud);
    }
    let authority = verify(&token, &keys, &admission)?;

    let pepper = Pepper::resolve(|k| std::env::var(k).ok());
    let manifest = std::fs::read_to_string(&args.declaration)
        .with_context(|| format!("reading the declaration `{}`", args.declaration.display()))?;
    let store = Store::open(&std::env::current_dir()?, &args.project)?;
    let face = Face::open(store, &manifest, pepper.clone())?;
    if let Some(signal) = pepper.signal() {
        eprintln!("{signal}");
    }

    let boundary = |a: &AdmittedAuthority| -> Result<(), AuthorityError> {
        let at = now().map_err(|e| AuthorityError::TimestampMalformed(e.to_string()))?;
        effect_boundary(a, &Admission::new(at, &revocation))
    };
    let server = Server::new(&face, authority, &boundary).map_err(anyhow::Error::msg)?;
    server.serve(std::io::stdin().lock(), std::io::stdout().lock())?;
    Ok(())
}
