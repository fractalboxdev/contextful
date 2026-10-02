//! `contextful serve --http` — the tool server over MCP Streamable HTTP.
//!
//! A thin adapter: it checks the declared audience and in-flight ceiling and the issuer
//! key, opens the read face over the project's store and manifest, binds the listener,
//! and hands every request to the network transport, which admits each one on its own
//! credential. Every value is resolved before the listener binds, so a process that
//! cannot serve binds nothing.

use crate::admit::{face, revocation_state, LedgerFile, LivePins, AUDIENCE_VAR, PUBKEY_VAR};
use crate::project::locate;
use crate::root::root as project_root;
use crate::clock::SystemClock;
use anyhow::Result;
use contextful_agent::http::{audience, ceiling, Admitting, HttpFace};
use contextful_policy::keyset::{KeyCheckpoint, StaticPins};
use contextful_policy::revoke::RevocationState;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;

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
    /// The key-set ledger holding retired keys and scoped epochs, its epochs re-read on
    /// every request; absent, `.contextful/keyset.toml` under the project root.
    #[arg(long)]
    keyset: Option<PathBuf>,
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
    let root = project_root(args.project.as_deref())?;
    let ledger = Arc::new(LedgerFile::at(&root, args.keyset.as_deref()));
    // Every admission filters the pins through the ledger it reads then, so a key retired
    // while the face runs verifies nothing from the next request; a start with every
    // pinned key retired, or an unreadable ledger, binds nothing.
    let pins = issuer_pins(args.public_key.as_deref())?;
    let live = LivePins::new(pins, ledger.clone(), SystemClock);
    let checkpoint = KeyCheckpoint::start(Box::new(live), SystemClock).map_err(|e| ServeError::IssuerKeyUnusable(e.to_string()))?;
    let denylist = args.denylist.clone();
    let revocation = move || -> Result<RevocationState, String> {
        let ledger = ledger.read().map_err(|e| e.to_string())?;
        revocation_state(denylist.as_deref(), &ledger).map_err(|e| format!("{e:#}"))
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
