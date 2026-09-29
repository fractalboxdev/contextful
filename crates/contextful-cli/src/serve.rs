//! `contextful serve --http` — the tool server over MCP Streamable HTTP.
//!
//! A thin adapter: it checks the declared audience and in-flight ceiling and the issuer
//! key, opens the read face over the project's store and manifest, binds the listener,
//! and hands every request to the network transport, which admits each one on its own
//! credential. Every value is resolved before the listener binds, so a process that
//! cannot serve binds nothing.

use crate::admit::{face, AUDIENCE_VAR, PUBKEY_VAR};
use crate::project::locate;
use crate::run::SystemClock;
use anyhow::Result;
use contextful_agent::http::{audience, ceiling, Admitting, HttpFace};
use contextful_policy::keyset::{KeyCheckpoint, StaticPins};
use contextful_policy::revoke::{parse_denylist, RevocationState};
use std::net::TcpListener;
use std::path::PathBuf;

/// The key version a denylist entry records for credentials verified under static pins.
const STATIC_KEY_VERSION: &str = "static";

/// The refusals of starting the network transport. `Display` begins with the identifier.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServeError {
    /// (`topology.publish-hostname.issuer-key`)
    #[error("IssuerKeyUnusable: {0}")]
    IssuerKeyUnusable(String),
}

#[derive(clap::Args)]
pub struct ServeArgs {
    /// The address the transport listens on, such as `127.0.0.1:8080`; it answers
    /// MCP Streamable HTTP at `POST /mcp`.
    #[arg(long)]
    http: String,
    /// The audience every credential names. Required, with no default.
    #[arg(long, env = AUDIENCE_VAR)]
    audience: Option<String>,
    /// Requests in flight at once; past it a request answers `503`. Required, with no default.
    #[arg(long)]
    max_in_flight: Option<usize>,
    /// The project whose store root is `.contextful/context/<project>/` under the working
    /// directory; absent, the nearest `contextful.toml` upward names it.
    #[arg(long)]
    project: Option<String>,
    /// The pipeline manifest holding the table declarations and query templates; absent,
    /// the project's `contextful.toml`.
    #[arg(long)]
    declaration: Option<PathBuf>,
    /// Comma-separated issuer key pins.
    #[arg(long, env = PUBKEY_VAR)]
    public_key: Option<String>,
    /// A file of revocation identifiers, one per line, re-read on every request.
    #[arg(long)]
    denylist: Option<PathBuf>,
}

/// The issuer key pins, resolved and parsed before the listener binds; no generated key
/// substitutes (`topology.publish-hostname.issuer-key`).
fn issuer_pins(flag: Option<&str>) -> Result<StaticPins, ServeError> {
    let Some(text) = flag.filter(|t| !t.trim().is_empty()) else {
        return Err(ServeError::IssuerKeyUnusable(format!("no issuer key: pass --public-key or set {PUBKEY_VAR}")));
    };
    StaticPins::parse(text).map_err(|e| ServeError::IssuerKeyUnusable(format!("--public-key / {PUBKEY_VAR}: {e}")))
}

pub fn run(args: ServeArgs) -> Result<()> {
    let clock = SystemClock;
    // The declarations are checked before anything opens (`read.register.serve-declaration`).
    let audience = audience(args.audience.as_deref()).map_err(anyhow::Error::msg)?;
    let ceiling = ceiling(args.max_in_flight).map_err(anyhow::Error::msg)?;
    let pins = issuer_pins(args.public_key.as_deref())?;
    let checkpoint = KeyCheckpoint::start(Box::new(pins), SystemClock).map_err(|e| ServeError::IssuerKeyUnusable(e.to_string()))?;
    let denylist = args.denylist.clone();
    let revocation = move || -> Result<RevocationState, String> {
        let mut state = RevocationState::default();
        if let Some(path) = &denylist {
            let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            state.denylist = parse_denylist(&text, STATIC_KEY_VERSION);
        }
        Ok(state)
    };
    // The denylist reads once before binding, so a missing file refuses the start.
    revocation().map_err(anyhow::Error::msg)?;
    let admitting = Admitting { checkpoint: &checkpoint, audience, revocation: &revocation };
    let face = face(&locate(args.project.as_deref(), args.declaration)?)?;
    let http = HttpFace::new(&face, &clock, admitting, Some(ceiling)).map_err(anyhow::Error::msg)?;
    let listener = TcpListener::bind(&args.http)?;
    eprintln!("listening on http://{}/mcp", listener.local_addr()?);
    http.serve(listener)?;
    Ok(())
}
