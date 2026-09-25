//! The `contextful` binary.

mod context;
mod differential;
mod formal;
mod pipeline;
mod run;
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
    /// Land, list, scan and fold a project's store tables.
    #[command(subcommand)]
    Context(context::ContextCmd),
    /// Validate and fire declared pipelines.
    #[command(subcommand)]
    Pipeline(pipeline::PipelineCmd),
    /// Start, inspect, stop and resume durable runs.
    #[command(subcommand)]
    Run(run::RunCmd),
    /// Elaborate and audit the Lean models under `formal/`.
    #[command(subcommand)]
    Formal(formal::FormalCmd),
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Token(c) => token::run(c),
        Cmd::Context(c) => context::run(c),
        Cmd::Run(c) => run::run(c),
        Cmd::Pipeline(c) => pipeline::run(c),
        Cmd::Formal(c) => formal::run(c),
    };
    if let Err(e) = result {
        eprintln!("{e:#}");
        std::process::exit(1);
    }
}
