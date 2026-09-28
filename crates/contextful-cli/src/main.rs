//! The `contextful` binary.

mod admit;
mod context;
mod derive;
mod differential;
mod formal;
mod mcp;
mod memory;
mod pipeline;
mod project;
mod run;
mod sync;
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
    /// Declare a project in `contextful.toml` here and create its store root.
    Init(project::InitArgs),
    /// Mint, attenuate, verify, introspect and exchange capability credentials.
    #[command(subcommand)]
    Token(token::TokenCmd),
    /// Land, list, scan and fold a project's store tables.
    #[command(subcommand)]
    Context(context::ContextCmd),
    /// Run one derive engine outside a pipeline.
    #[command(subcommand)]
    Derive(derive::DeriveCmd),
    /// Validate and fire declared pipelines.
    #[command(subcommand)]
    Pipeline(pipeline::PipelineCmd),
    /// Push, pull, lease and compact against the store's bucket.
    #[command(subcommand)]
    Sync(sync::SyncCmd),
    /// Start, inspect, stop and resume durable runs.
    #[command(subcommand)]
    Run(run::RunCmd),
    /// Synthesize memory from landed rows, and write claims directly.
    #[command(subcommand)]
    Memory(memory::MemoryCmd),
    /// Serve the read face over the tool protocol on standard input and output.
    Mcp(mcp::McpArgs),
    /// Elaborate and audit the Lean models under `formal/`.
    #[command(subcommand)]
    Formal(formal::FormalCmd),
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Init(c) => project::run(c),
        Cmd::Token(c) => token::run(c),
        Cmd::Context(c) => context::run(c),
        Cmd::Run(c) => run::run(c),
        Cmd::Sync(c) => sync::run(c),
        Cmd::Pipeline(c) => pipeline::run(c),
        Cmd::Mcp(c) => mcp::run(c),
        Cmd::Derive(c) => derive::run(c),
        Cmd::Memory(c) => memory::run(c),
        Cmd::Formal(c) => formal::run(c),
    };
    if let Err(e) = result {
        eprintln!("{e:#}");
        std::process::exit(1);
    }
}
