//! `contextful mcp` — the tool server over standard input and output.
//!
//! A thin adapter: it admits the one credential the process serves, resolves the pepper,
//! opens the read face over the project's store and manifest, and hands both to the tool
//! server. Every value is resolved before the first protocol line is written, so a
//! process that cannot serve exits with nothing on standard output.

use crate::admit::{face, AdmitArgs};
use crate::project::locate;
use crate::run::SystemClock;
use anyhow::Result;
use contextful_agent::mcp::Server;
use contextful_core::ports::Clock;
use contextful_core::AuthorityError;
use contextful_policy::verify::{effect_boundary, Admission, AdmittedAuthority};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct McpArgs {
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
    let located = locate(args.project.as_deref(), args.declaration)?;
    crate::sync::pull_before_run(&located)?;
    let face = face(&located)?;
    let clock = SystemClock;
    let boundary = |a: &AdmittedAuthority| -> Result<(), AuthorityError> { effect_boundary(a, &Admission::new(clock.now(), &revocation)) };
    let server = Server::new(&face, authority, &boundary, &clock).map_err(anyhow::Error::msg)?;
    server.serve(std::io::stdin().lock(), std::io::stdout().lock())?;
    Ok(())
}
