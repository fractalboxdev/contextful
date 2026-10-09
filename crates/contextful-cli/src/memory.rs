//! `contextful memory` — synthesis, direct write and keyed recall.
//!
//! The adapter admits a credential, opens the read face, wires the inference endpoint
//! for synthesis, and prints the selected command's result. No memory rule lives here.

use crate::admit::{face, AdmitArgs};
use crate::project::locate;
use crate::clock::SystemClock;
use anyhow::{Context, Result};
use clap::Subcommand;
use contextful_context::node;
use contextful_context::read::RecallRequest;
use contextful_core::memory::synthesize::CandidateClaim;
use contextful_core::ports::Clock;
use contextful_policy::verify::{effect_boundary, Admission};
use contextful_memory::synthesize::Pass;
use contextful_memory::relations;
use contextful_core::time::Instant;
use contextful_core::store::bound_time::Bound;
use contextful_memory::write::{write_observed, Observation};
use contextful_outbound::infer::Endpoint;
use contextful_policy::enforce::session::Request;
use std::path::PathBuf;

/// The environment variable holding the inference endpoint's bearer key, if it takes one.
pub(crate) const INFERENCE_KEY_VAR: &str = "CONTEXTFUL_INFERENCE_KEY";

#[derive(clap::Args)]
pub struct Project {
    /// The project whose store root is `.contextful/context/<project>/` under the working
    /// directory; absent, the nearest `contextful.toml` upward names it.
    #[arg(long)]
    project: Option<String>,
    /// The manifest declaring the tables and memory shapes; absent, the project's
    /// `contextful.toml`.
    #[arg(long)]
    declaration: Option<PathBuf>,
    #[command(flatten)]
    admit: AdmitArgs,
}

#[derive(Subcommand)]
pub enum MemoryCmd {
    /// Migrate stored edges between declared relation names.
    Relations {
        #[command(subcommand)]
        command: RelationsCmd,
    },
    /// Read one subject's claims at an observed instant.
    Recall {
        #[command(flatten)]
        project: Project,
        /// The memory facts table to read.
        #[arg(long)]
        table: String,
        /// The exact claim subject.
        #[arg(long)]
        subject: String,
        /// The instant at which the claims hold; absent takes the current instant.
        #[arg(long)]
        observed_at: Option<String>,
        /// The transaction-time bound over claims and evidence.
        #[arg(long)]
        as_of_ingest: Option<String>,
        /// The maximum number of claims returned.
        #[arg(long)]
        limit: Option<u64>,
    },
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
        /// The RFC 3339 instant the claim holds from; absent, the write's own instant.
        #[arg(long)]
        observed_at: Option<String>,
        /// The key `claim_id` derives from; a claim the table already holds under it lands
        /// nothing.
        #[arg(long)]
        dedup_key: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum RelationsCmd {
    /// Rewrite stored edges before removing the old name from the manifest.
    Rename {
        from: String,
        to: String,
        #[command(flatten)]
        project: Project,
    },
}

pub fn run(cmd: MemoryCmd) -> Result<()> {
    match cmd {
        MemoryCmd::Relations { command: RelationsCmd::Rename { from, to, project } } => {
            let (authority, revocation) = project.admit.admit(project.project.as_deref(), "a relation rename")?;
            let boundary = || effect_boundary(&authority, &Admission::new(SystemClock.now(), &revocation));
            let face = face(&locate(project.project.as_deref(), project.declaration.clone())?)?;
            let (node, _) = node::resolve(face.store(), |k| std::env::var(k).ok())?;
            let (table, count) = relations::rename(&face, &authority, &from, &to, &node, SystemClock.now(), &boundary)?;
            println!("{table}: {count} edges renamed from {from} to {to}");
            Ok(())
        }
        MemoryCmd::Recall { project, table, subject, observed_at, as_of_ingest, limit } => {
            let observed_at = observed_at.map(|s| Bound::parse(&s)).transpose()?;
            let as_of_ingest = as_of_ingest.map(|s| Bound::parse(&s)).transpose()?;
            let request = RecallRequest { observed_at, as_of_ingest, limit, ..RecallRequest::new(table, subject, SystemClock.now()) };
            let (authority, _) = project.admit.admit(project.project.as_deref(), "memory recall")?;
            let face = face(&locate(project.project.as_deref(), project.declaration.clone())?)?;
            let session = face.session(&authority, &Request::default(), request.bounds())?;
            let response = face.recall(&session, &request)?;
            println!("{}", serde_json::to_string(&response.to_json())?);
            Ok(())
        }
        MemoryCmd::Synthesize { project, source, into, endpoint, model } => {
            let (authority, revocation) = project.admit.admit(project.project.as_deref(), "a synthesis pass")?;
            let boundary = || effect_boundary(&authority, &Admission::new(SystemClock.now(), &revocation));
            let located = locate(project.project.as_deref(), project.declaration.clone())?;
            let face = face(&located)?;
            let (node, _) = node::resolve(face.store(), |k| std::env::var(k).ok())?;
            let inference = Endpoint::new(&endpoint, &model, std::env::var(INFERENCE_KEY_VAR).ok()).map_err(anyhow::Error::msg)?;
            // Machine-local state: pass cursors live beside the run state, outside the store root.
            let state = located.project.memory_dir();
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
        MemoryCmd::Write { project, into, claim, observed_at, dedup_key } => {
            let candidate: CandidateClaim = serde_json::from_str(&claim).context("`--claim` is one claim as JSON")?;
            let observed_at = observed_at
                .map(|s| Instant::parse(&s).with_context(|| format!("`--observed-at {s}` is no RFC 3339 instant")))
                .transpose()?;
            let observation = Observation { observed_at, dedup_key };
            let (authority, revocation) = project.admit.admit(project.project.as_deref(), "the direct write")?;
            let boundary = || effect_boundary(&authority, &Admission::new(SystemClock.now(), &revocation));
            let face = face(&locate(project.project.as_deref(), project.declaration.clone())?)?;
            let (node, _) = node::resolve(face.store(), |k| std::env::var(k).ok())?;
            let written = write_observed(&face, &authority, &into, candidate, &observation, &node, SystemClock.now(), &boundary)?;
            match written.claim {
                Some(c) => println!("{into}: landed {} ({}), retired {}", c.claim_id, c.tier.name(), written.retired.len()),
                None => println!("{into}: restates a claim the table holds; nothing landed"),
            }
            Ok(())
        }
    }
}
