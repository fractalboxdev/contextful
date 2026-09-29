//! The `contextful` command line as a library: the `contextful` binary runs it with no host
//! derive task and no row body, and an embedding binary runs it with the tasks and bodies it
//! registers before build (`run.bind.host-task`, `surface.fire.store-driven-body`).

mod admit;
mod component;
mod context;
mod derive;
mod differential;
mod eval;
mod formal;
mod job;
mod mcp;
mod memory;
mod pipeline;
mod project;
mod query;
mod run;
mod serve;
mod sync;
mod token;

use clap::{Parser, Subcommand};
use contextful_core::run::derive::task::Tasks;
use contextful_core::run::drive::Bodies;

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
    /// Validate job blocks and fire a store-driven job.
    #[command(subcommand)]
    Job(job::JobCmd),
    /// Start, inspect, stop and resume durable runs.
    #[command(subcommand)]
    Run(run::RunCmd),
    /// Synthesize memory from landed rows, and write claims directly.
    #[command(subcommand)]
    Memory(memory::MemoryCmd),
    /// Run one operator statement raw and print the response projection; no network face reaches it.
    Query(query::QueryArgs),
    /// Serve the read face over the tool protocol on standard input and output.
    Mcp(mcp::McpArgs),
    /// Serve the tool protocol over MCP Streamable HTTP, admitting each request on its own credential.
    Serve(serve::ServeArgs),
    /// Score a case file through the ranked read and hold it to the floors and a baseline.
    #[command(subcommand)]
    Eval(eval::EvalCmd),
    /// Elaborate and audit the Lean models under `formal/`.
    #[command(subcommand)]
    Formal(formal::FormalCmd),
    /// The decode worker a compiled-in source runs behind the process boundary
    /// (`run.land.parse-boundary`): standard input in, decoded JSON out.
    #[cfg(feature = "drive")]
    #[command(hide = true)]
    Decode {
        kind: String,
        #[arg(long)]
        input: String,
    },
}

/// The compiled code an embedding binary registers before build: host derive tasks and
/// store-driven row bodies.
#[derive(Clone, Default, Debug)]
pub struct Host {
    pub tasks: Tasks,
    pub bodies: Bodies,
}

/// Parse the process arguments and run the command, with `tasks` registered as host derive
/// tasks; a failure prints and exits 1.
pub fn main_with(tasks: Tasks) {
    main_host(Host { tasks, bodies: Bodies::default() });
}

/// Parse the process arguments and run the command with `host`'s tasks and bodies
/// registered; a failure prints and exits 1.
pub fn main_host(host: Host) {
    let Host { tasks, bodies } = host;
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Init(c) => project::run(c),
        Cmd::Token(c) => token::run(c),
        Cmd::Context(c) => context::run(c),
        Cmd::Run(c) => run::run(c),
        Cmd::Job(c) => job::run(c, &bodies),
        Cmd::Sync(c) => sync::run(c),
        Cmd::Pipeline(c) => pipeline::run(c, &tasks),
        Cmd::Query(c) => query::run(c),
        Cmd::Mcp(c) => mcp::run(c),
        Cmd::Serve(c) => serve::run(c),
        Cmd::Derive(c) => derive::run(c),
        Cmd::Memory(c) => memory::run(c),
        Cmd::Eval(c) => eval::run(c),
        Cmd::Formal(c) => formal::run(c),
        #[cfg(feature = "drive")]
        Cmd::Decode { kind, input } => std::process::exit(contextful_connectors::boundary::worker(&kind, &input)),
    };
    if let Err(e) = result {
        eprintln!("{e:#}");
        std::process::exit(1);
    }
}
