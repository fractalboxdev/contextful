//! `contextful formal` — elaboration and axiom audit of the Lean models.

use clap::Subcommand;

#[derive(Subcommand)]
pub enum FormalCmd {}

pub fn run(cmd: FormalCmd) -> anyhow::Result<()> {
    match cmd {}
}
