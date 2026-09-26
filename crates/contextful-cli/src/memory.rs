//! `contextful memory` — synthesis passes and the direct write.
//!
//! A thin adapter: it admits the writing credential, opens the read face, wires the one
//! inference endpoint, and prints what the memory crate did. No memory rule lives here.

use crate::admit::{face, AdmitArgs};
use crate::run::SystemClock;
use anyhow::{Context, Result};
use clap::Subcommand;
use contextful_context::node;
use contextful_core::memory::synthesize::CandidateClaim;
use contextful_core::ports::Clock;
use contextful_policy::verify::{effect_boundary, Admission};
use contextful_memory::synthesize::Pass;
use contextful_memory::write::write_claim;
use contextful_runtime::infer::Endpoint;
use std::path::PathBuf;

/// The environment variable holding the inference endpoint's bearer key, if it takes one.
const INFERENCE_KEY_VAR: &str = "CONTEXTFUL_INFERENCE_KEY";

#[derive(clap::Args)]
pub struct Project {
    /// The project whose store root is `.contextful/context/<project>/`.
    #[arg(long)]
    project: String,
    /// The manifest declaring the tables and memory shapes.
    #[arg(long, default_value = "contextful.toml")]
    declaration: PathBuf,
    #[command(flatten)]
    admit: AdmitArgs,
}

#[derive(Subcommand)]
pub enum MemoryCmd {
    /// Run one synthesis pass from a source table into a claims table.
    Synthesize {
        #[command(flatten)]
        project: Project,
        #[arg(long)]
        source: String,
        #[arg(long)]
        into: String,
        /// The OpenAI-compatible endpoint `/chat/completions` hangs off.
        #[arg(long)]
        endpoint: String,
        #[arg(long)]
        model: String,
    },
    /// Write one claim, given as JSON, into a claims table.
    Write {
        #[command(flatten)]
        project: Project,
        #[arg(long)]
        into: String,
        #[arg(long)]
        claim: String,
    },
}

/// Machine-local state: pass cursors live beside the run state, outside the store root.
fn state_dir(project: &str) -> Result<PathBuf> {
    Ok(std::env::current_dir()?.join(".contextful/memory").join(project))
}

pub fn run(cmd: MemoryCmd) -> Result<()> {
    match cmd {
        MemoryCmd::Synthesize { project, source, into, endpoint, model } => {
            let (authority, revocation) = project.admit.admit("a synthesis pass")?;
            let boundary = || effect_boundary(&authority, &Admission::new(SystemClock.now(), &revocation));
            let face = face(&project.project, &project.declaration)?;
            let (node, _) = node::resolve(face.store(), |k| std::env::var(k).ok())?;
            let inference = Endpoint::new(&endpoint, &model, std::env::var(INFERENCE_KEY_VAR).ok()).map_err(anyhow::Error::msg)?;
            let state = state_dir(&project.project)?;
            let report = Pass {
                face: &face,
                authority: &authority,
                inference: &inference,
                source: &source,
                into: &into,
                state: &state,
                node: &node,
                now: SystemClock.now(),
                boundary: &boundary,
            }
            .run()?;
            if report.runs.is_empty() {
                println!("{into}: no new runs in {source}");
            } else {
                println!(
                    "{into}: read {} from {source}; {} landed, {} retired, {} restated, {} dead-lettered",
                    report.runs.join(", "),
                    report.landed,
                    report.retired,
                    report.restated,
                    report.dead_lettered
                );
            }
            Ok(())
        }
        MemoryCmd::Write { project, into, claim } => {
            let candidate: CandidateClaim = serde_json::from_str(&claim).context("`--claim` is one claim as JSON")?;
            let (authority, revocation) = project.admit.admit("the direct write")?;
            let boundary = || effect_boundary(&authority, &Admission::new(SystemClock.now(), &revocation));
            let face = face(&project.project, &project.declaration)?;
            let (node, _) = node::resolve(face.store(), |k| std::env::var(k).ok())?;
            let written = write_claim(&face, &authority, &into, candidate, &node, SystemClock.now(), &boundary)?;
            match written.claim {
                Some(c) => println!("{into}: landed {} ({}), retired {}", c.claim_id, c.tier.name(), written.retired.len()),
                None => println!("{into}: restates a live claim; nothing landed"),
            }
            Ok(())
        }
    }
}
