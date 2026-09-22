//! `contextful token` — the authority surface of the command line.

use clap::Subcommand;

#[derive(Subcommand)]
pub enum TokenCmd {}

pub fn run(cmd: TokenCmd) -> anyhow::Result<()> {
    match cmd {}
}
