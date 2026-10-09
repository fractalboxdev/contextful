//! `contextful mcp` — the tool server over standard input and output.
//!
//! A thin adapter: it admits the one credential the process serves, resolves the pepper,
//! opens the read face over the project's store and manifest and the project's audit chain
//! with optional explicit startup signing custody (`disclosure.record.read-chain`), and hands them to the tool server. Every value is resolved before the first protocol line is written, so a
//! process that cannot serve exits with nothing on standard output.

use crate::admit::{AdmitArgs, AdmitError};
use crate::project::locate;
use crate::clock::SystemClock;
use anyhow::Result;
use contextful_agent::mcp::Server;
use contextful_core::ports::Clock;
use contextful_core::AuthorityError;
use contextful_policy::verify::{effect_boundary, Admission, AdmittedAuthority};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct McpArgs {
    /// Explicit startup audit signing port, checked against independent current pins.
    #[arg(long)]
    issuer_key: Option<PathBuf>,
    /// Admit the selected local project under a verified signed owner claim.
    #[arg(long)]
    owner: bool,
    /// The project whose store root is `.contextful/context/<project>/` under the working
    /// directory; absent, the nearest `contextful.toml` upward names it.
    #[arg(long)]
    project: Option<String>,
    /// The pipeline manifest holding the table declarations and query templates; absent,
    /// the project's `contextful.toml`.
    #[arg(long)]
    declaration: Option<PathBuf>,
    #[command(flatten)]
    admit: AdmitArgs,
}

pub fn run(args: McpArgs) -> Result<()> {
    let (authority, revocation) = args.admit.admit(args.project.as_deref(), "the tool server")?;
    if args.project.is_none() {
        let cwd = std::env::current_dir()?;
        if !cwd.ancestors().any(|dir| dir.join("contextful.toml").is_file()) {
            anyhow::bail!("StoreSelectorAbsent: no contextful.toml in {} or any ancestor", cwd.display());
        }
    }
    let located = locate(args.project.as_deref(), args.declaration)?;
    let authority = if args.owner {
        let identity = crate::project::owner_identity(&located.project)
            .map_err(|_| AdmitError::OwnerCredentialInvalid("the selected local store has no readable UUID".into()))?;
        authority.activate_owner(&identity)
            .ok_or_else(|| AdmitError::OwnerCredentialInvalid("the verified credential does not own the selected local store".into()))?
    } else {
        authority
    };
    crate::sync::pull_before_run(&located)?;
    let face = crate::admit::face_with_pins(&located, args.admit.public_key.as_deref(), args.admit.keyset.as_deref())?;
    let audit = crate::project::read_audit(&located.project, args.issuer_key.as_deref(), args.admit.public_key.as_deref(), args.admit.keyset.as_deref())?;
    let clock = SystemClock;
    let boundary = |a: &AdmittedAuthority| -> Result<(), AuthorityError> { effect_boundary(a, &Admission::new(clock.now(), &revocation)) };
    let server = Server::new(&face, authority, &boundary, &clock, &audit).map_err(anyhow::Error::msg)?.with_build(crate::build_identity());
    server.serve(std::io::stdin().lock(), std::io::stdout().lock())?;
    Ok(())
}
