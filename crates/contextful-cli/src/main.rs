//! The `contextful` binary.

mod formal;
mod token;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "contextful", about = "The Contextful command line")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Mint, attenuate, verify, introspect and exchange capability credentials.
    #[command(subcommand)]
    Token(token::TokenCmd),
    /// Elaborate and audit the Lean models under `formal/`.
    #[command(subcommand)]
    Formal(formal::FormalCmd),
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Token(c) => token::run(c),
        Cmd::Formal(c) => formal::run(c),
    };
    if let Err(e) = result {
        eprintln!("{e:#}");
        std::process::exit(1);
    }
}
