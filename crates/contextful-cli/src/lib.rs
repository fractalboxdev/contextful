//! The `contextful` command line as a library: the `contextful` binary runs it with no host
//! derive task and no row body, and an embedding binary runs it with the tasks and bodies it
//! registers before build (`run.bind.host-task`, `surface.fire.store-driven-body`).

mod admit;
#[cfg(feature = "read-plane")]
mod audit;
#[cfg(feature = "data-plane")]
mod build;
#[cfg(feature = "data-plane")]
mod cadence;
#[cfg(feature = "read-plane")]
mod clock;
#[cfg(feature = "data-plane")]
mod component;
#[cfg(feature = "data-plane")]
mod connector;
#[cfg(feature = "data-plane")]
mod context;
#[cfg(feature = "data-plane")]
pub use context::derived_catalog;
#[cfg(feature = "data-plane")]
mod derive;
mod differential;
mod protocol_differential;
#[cfg(feature = "data-plane")]
mod disclosure;
#[cfg(feature = "data-plane")]
mod effective;
#[cfg(feature = "data-plane")]
mod eval;
#[cfg(feature = "data-plane")]
mod export;
mod formal;
#[cfg(feature = "data-plane")]
mod job;
#[cfg(feature = "data-plane")]
mod model_source;
#[cfg(feature = "read-plane")]
mod mcp;
#[cfg(feature = "data-plane")]
mod memory;
#[cfg(feature = "data-plane")]
mod pipeline;
#[cfg(feature = "read-plane")]
mod project;
#[cfg(feature = "read-plane")]
mod query;
#[cfg(feature = "read-plane")]
mod reside;
#[cfg(feature = "data-plane")]
mod run;
mod root;
#[cfg(feature = "read-plane")]
mod serve;
#[cfg(feature = "read-plane")]
mod sync;
mod token;
#[cfg(feature = "data-plane")]
mod worker;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use contextful_core::run::derive::task::Tasks;
use contextful_core::run::drive::Bodies;

/// The profile bundle this build was selected with (`topology.package.version-profile`); a
/// build selecting none is a development build.
#[cfg(feature = "contextful-full")]
macro_rules! profile {
    () => {
        "contextful-full"
    };
}
#[cfg(all(feature = "contextful-edge", not(feature = "contextful-full")))]
macro_rules! profile {
    () => {
        "contextful-edge"
    };
}
#[cfg(all(feature = "contextful-control", not(any(feature = "contextful-full", feature = "contextful-edge"))))]
macro_rules! profile {
    () => {
        "contextful-control"
    };
}
#[cfg(not(any(feature = "contextful-full", feature = "contextful-edge", feature = "contextful-control")))]
macro_rules! profile {
    () => {
        "development"
    };
}

/// The profile this build was selected with.
pub const PROFILE: &str = profile!();

/// What this binary links, as its handshake and `/health` report it, composed from the
/// feature declarations that select [`PROFILE`] (`read.embed.build-identity`): the read
/// path's engine, lexical and vector backends, the `s3` and `wasm` connector families, and
/// the `http`, `eval` and `otlp` faces.
#[cfg(feature = "read-plane")]
pub(crate) fn build_identity() -> contextful_core::read::face::BuildIdentity {
    let named = |pairs: &[(bool, &str)]| pairs.iter().filter(|(linked, _)| *linked).map(|(_, n)| n.to_string()).collect();
    contextful_core::read::face::BuildIdentity {
        backends: named(&[(true, "duckdb"), (contextful_context::read::LEXICAL_BACKEND, "fts"), (true, "hnsw")]),
        connectors: named(&[(cfg!(feature = "s3-sync"), "s3"), (cfg!(feature = "component-host"), "wasm")]),
        faces: named(&[(true, "http"), (cfg!(feature = "data-plane"), "eval"), (cfg!(feature = "data-plane"), "otlp")]),
    }
}

/// `contextful --version`: the workspace version, then the profile.
const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " ", profile!());

/// The read-path subcommands, which a build without the read path answers with
/// `ProfileCapabilityAbsent` (`topology.package.capability-absent`).
#[cfg(not(feature = "read-plane"))]
const READ_PLANE: [&str; 4] = ["sync", "query", "mcp", "serve"];

/// The run-path subcommands, which a build without the run path, the read replica
/// included, answers with `ProfileCapabilityAbsent` (`topology.package.edge-profile`).
#[cfg(not(feature = "data-plane"))]
const RUN_PLANE: [&str; 11] = ["init", "context", "derive", "build", "pipeline", "export", "job", "run", "memory", "eval", "connector"];

/// The refusal of a subcommand this build's profile does not link. `Display` begins with
/// the identifier.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    /// (`topology.package.capability-absent`)
    #[error("ProfileCapabilityAbsent: {0}")]
    ProfileCapabilityAbsent(String),
}

#[derive(Parser)]
#[command(name = "contextful", about = "The Contextful command line", version = VERSION)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Declare a project in `contextful.toml` here and create its store root.
    #[cfg(feature = "data-plane")]
    Init(project::InitArgs),
    /// Mint, attenuate, verify, introspect and exchange capability credentials.
    #[command(subcommand)]
    Token(token::TokenCmd),
    /// Land, list, scan and fold a project's store tables.
    #[cfg(feature = "data-plane")]
    #[command(subcommand)]
    Context(context::ContextCmd),
    /// Verify, prove and query the project's audit chain, as its local owner.
    #[cfg(feature = "read-plane")]
    #[command(subcommand)]
    Audit(audit::AuditCmd),
    /// Run one derive engine outside a pipeline.
    #[cfg(feature = "data-plane")]
    #[command(subcommand)]
    Derive(derive::DeriveCmd),
    /// Build a declared model into a published table, or hold one of its builds.
    #[cfg(feature = "data-plane")]
    Build(build::BuildArgs),
    /// Diagnose local model disclosure declarations.
    #[cfg(feature = "data-plane")]
    #[command(subcommand)]
    Disclosure(disclosure::DisclosureCmd),
    /// Validate and fire declared pipelines.
    #[cfg(feature = "data-plane")]
    #[command(subcommand)]
    Pipeline(pipeline::PipelineCmd),
    /// Inspect a connector artifact's local content digest.
    #[cfg(feature = "data-plane")]
    #[command(subcommand)]
    Connector(connector::ConnectorCmd),
    /// Push, pull, lease and compact against the store's bucket.
    #[cfg(feature = "read-plane")]
    #[command(subcommand)]
    Sync(sync::SyncCmd),
    /// Deliver a landed table's rows to an operator-declared OTLP target.
    #[cfg(feature = "data-plane")]
    #[command(subcommand)]
    Export(export::ExportCmd),
    /// Validate job blocks and fire a store-driven job.
    #[cfg(feature = "data-plane")]
    #[command(subcommand)]
    Job(job::JobCmd),
    /// Start, inspect, stop and resume durable runs.
    #[cfg(feature = "data-plane")]
    #[command(subcommand)]
    Run(run::RunCmd),
    /// Synthesize memory from landed rows, and write claims directly.
    #[cfg(feature = "data-plane")]
    #[command(subcommand)]
    Memory(memory::MemoryCmd),
    /// Run one operator statement raw and print the response projection; no network face reaches it.
    #[cfg(feature = "read-plane")]
    Query(query::QueryArgs),
    /// Serve the read face over the tool protocol on standard input and output.
    #[cfg(feature = "read-plane")]
    Mcp(mcp::McpArgs),
    /// Serve the tool protocol over MCP Streamable HTTP, admitting each request on its own credential.
    #[cfg(feature = "read-plane")]
    Serve(serve::ServeArgs),
    /// Score a case file through the ranked read and hold it to the floors and a baseline.
    #[cfg(feature = "data-plane")]
    #[command(subcommand)]
    Eval(eval::EvalCmd),
    /// Elaborate and audit the Lean models under `formal/`.
    #[command(subcommand)]
    Formal(formal::FormalCmd),
    /// The decode worker a compiled-in source runs behind the process boundary
    /// (`run.land.parse-boundary`): standard input in, decoded JSON out.
    #[cfg(feature = "pdf")]
    #[command(hide = true)]
    Decode {
        kind: String,
        #[arg(long)]
        input: String,
    },
    /// A data-plane subcommand on a build without the data plane.
    #[cfg(not(feature = "data-plane"))]
    #[command(external_subcommand)]
    Absent(Vec<String>),
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
    #[cfg_attr(not(feature = "data-plane"), allow(unused_variables))]
    let Host { tasks, bodies } = host;
    let matches = Cli::command().get_matches();
    admit::record_command_path(&matches);
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.format(&mut Cli::command()).exit());
    let result = match cli.cmd {
        #[cfg(feature = "data-plane")]
        Cmd::Init(c) => project::run(c),
        Cmd::Token(c) => token::run(c),
        #[cfg(feature = "data-plane")]
        Cmd::Context(c) => context::run(c),
        #[cfg(feature = "data-plane")]
        Cmd::Run(c) => run::run(c),
        #[cfg(feature = "data-plane")]
        Cmd::Job(c) => job::run(c, &bodies),
        #[cfg(feature = "data-plane")]
        Cmd::Export(c) => export::run(c),
        #[cfg(feature = "read-plane")]
        Cmd::Sync(c) => sync::run(c),
        #[cfg(feature = "data-plane")]
        Cmd::Pipeline(c) => pipeline::run(c, &tasks, &bodies),
        #[cfg(feature = "data-plane")]
        Cmd::Connector(c) => connector::run(c),
        #[cfg(feature = "data-plane")]
        Cmd::Build(c) => build::run(c),
        #[cfg(feature = "data-plane")]
        Cmd::Disclosure(c) => disclosure::run(c),
        #[cfg(feature = "read-plane")]
        Cmd::Query(c) => query::run(c),
        #[cfg(feature = "read-plane")]
        Cmd::Mcp(c) => mcp::run(c),
        #[cfg(feature = "read-plane")]
        Cmd::Serve(c) => serve::run(c, &tasks),
        #[cfg(feature = "data-plane")]
        Cmd::Derive(c) => derive::run(c),
        #[cfg(feature = "read-plane")]
        Cmd::Audit(c) => audit::run(c),
        #[cfg(feature = "data-plane")]
        Cmd::Memory(c) => memory::run(c),
        #[cfg(feature = "data-plane")]
        Cmd::Eval(c) => eval::run(c),
        Cmd::Formal(c) => formal::run(c),
        #[cfg(not(feature = "data-plane"))]
        Cmd::Absent(argv) => absent(&argv),
        #[cfg(feature = "pdf")]
        Cmd::Decode { kind, input } => std::process::exit(contextful_connectors::boundary::worker(&kind, &input)),
    };
    if let Err(e) = result {
        eprintln!("{e:#}");
        std::process::exit(1);
    }
}

/// Answer a subcommand the parser did not define: a data-plane name raises
/// `ProfileCapabilityAbsent` naming it and the profile; any other name is a usage error.
#[cfg(not(feature = "data-plane"))]
fn absent(argv: &[String]) -> anyhow::Result<()> {
    use clap::CommandFactory;
    let name = argv.first().map(String::as_str).unwrap_or_default();
    #[cfg(not(feature = "read-plane"))]
    if READ_PLANE.contains(&name) {
        return Err(ProfileError::ProfileCapabilityAbsent(format!(
            "`{name}` reaches the read path, which `{PROFILE}` does not link; run it on a `contextful-full` or `contextful-edge` build"
        ))
        .into());
    }
    if RUN_PLANE.contains(&name) {
        return Err(ProfileError::ProfileCapabilityAbsent(format!(
            "`{name}` reaches the run path, which `{PROFILE}` does not link; run it on a `contextful-full` build"
        ))
        .into());
    }
    Cli::command().error(clap::error::ErrorKind::InvalidSubcommand, format!("unrecognized subcommand '{name}'")).exit()
}
