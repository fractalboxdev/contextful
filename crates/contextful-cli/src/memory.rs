//! `contextful memory` — synthesis, direct write, keyed recall and settlement.
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
use contextful_core::memory::declare::Shape;
use contextful_core::memory::settle::{Observation as SettleObservation, RawRegistration};
use contextful_core::memory::synthesize::CandidateClaim;
use contextful_core::ports::Clock;
use contextful_policy::verify::{effect_boundary, Admission};
use contextful_memory::synthesize::Pass;
use contextful_memory::relations;
use contextful_core::time::Instant;
use contextful_core::store::bound_time::Bound;
use contextful_memory::settle;
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
    /// Register one prediction, given as JSON, into a `predictions` table.
    Predict {
        #[command(flatten)]
        project: Project,
        /// The `predictions` table the registration lands in.
        #[arg(long)]
        into: String,
        /// The registration: `prediction_id`, `subject`, `predicted_at`, one of
        /// `horizon_secs`, `deadline_at` or `watch`, `resolution_source` and, for a
        /// `metric` source, `comparator`.
        #[arg(long)]
        registration: String,
    },
    /// Observe one registered prediction's outcome, given as JSON, into an `outcomes` table.
    Observe {
        #[command(flatten)]
        project: Project,
        /// The `outcomes` table the observation lands in.
        #[arg(long)]
        into: String,
        /// The `predictions` table holding the registration; absent, the first the
        /// manifest declares.
        #[arg(long)]
        predictions: Option<String>,
        /// The observation: `prediction_id`, `observed_at`, and optionally `verdict`,
        /// `resolution_source`, `signal` and `settling_citation`.
        #[arg(long)]
        observation: String,
    },
    /// Report how well a claims table's emitted confidence held on settled predictions.
    Calibration {
        #[command(flatten)]
        project: Project,
        /// The `memory_facts` table whose claims the predictions score.
        #[arg(long)]
        table: String,
        /// The `predictions` table; absent, the first the manifest declares.
        #[arg(long)]
        predictions: Option<String>,
        /// The `outcomes` table; absent, the first the manifest declares.
        #[arg(long)]
        outcomes: Option<String>,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
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
            let located = locate(project.project.as_deref(), project.declaration.clone())?;
            synthesize(&located, &authority, &revocation, &source, &into, &endpoint, &model, SystemClock.now())
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
        MemoryCmd::Predict { project, into, registration } => {
            let raw: RawRegistration = serde_json::from_str(&registration).context("`--registration` is one registration as JSON")?;
            let (authority, revocation) = project.admit.admit(project.project.as_deref(), "a prediction registration")?;
            let boundary = || effect_boundary(&authority, &Admission::new(SystemClock.now(), &revocation));
            let face = face(&locate(project.project.as_deref(), project.declaration.clone())?)?;
            let (node, _) = node::resolve(face.store(), |k| std::env::var(k).ok())?;
            let settled = settle::predict(&face, &authority, &into, raw, &node, SystemClock.now(), &boundary)?;
            println!("prediction_id: {}", settled.prediction_id);
            Ok(())
        }
        MemoryCmd::Observe { project, into, predictions, observation } => {
            let observation: SettleObservation = serde_json::from_str(&observation).context("`--observation` is one observation as JSON")?;
            let (authority, revocation) = project.admit.admit(project.project.as_deref(), "an outcome observation")?;
            let boundary = || effect_boundary(&authority, &Admission::new(SystemClock.now(), &revocation));
            let face = face(&locate(project.project.as_deref(), project.declaration.clone())?)?;
            let (node, _) = node::resolve(face.store(), |k| std::env::var(k).ok())?;
            let settled = settle::observe(&face, &authority, &into, predictions.as_deref(), observation, &node, SystemClock.now(), &boundary)?;
            // The label row reads after the landing, through a fresh session.
            let predictions = settle::shaped(&face, predictions.as_deref(), Shape::Predictions)?;
            let session = face.session(&authority, &Request::default(), Default::default())?;
            let label = settle::labels(&face, &session, predictions, &into, Some(&settled.prediction_id))?;
            println!("{}", serde_json::to_string(&label.to_json())?);
            Ok(())
        }
        MemoryCmd::Calibration { project, table, predictions, outcomes, json } => {
            let (authority, _) = project.admit.admit(project.project.as_deref(), "a calibration report")?;
            let face = face(&locate(project.project.as_deref(), project.declaration.clone())?)?;
            let predictions = settle::shaped(&face, predictions.as_deref(), Shape::Predictions)?;
            let outcomes = settle::shaped(&face, outcomes.as_deref(), Shape::Outcomes)?;
            let session = face.session(&authority, &Request::default(), Default::default())?;
            let groups = settle::calibration(&face, &session, &table, predictions, outcomes)?;
            if json {
                let report: Vec<serde_json::Value> = groups.iter().map(settle::report_json).collect();
                println!("{}", serde_json::to_string(&report)?);
            } else if groups.is_empty() {
                println!("{table}: no settled prediction");
            } else {
                for g in &groups {
                    let held_out = g.held_out_brier.map_or("none".to_string(), |b| format!("{b:.4}"));
                    println!(
                        "{} {}: {} samples, brier {:.4}, ece {:.4}, held-out brier {held_out}, {}",
                        g.shape.name(),
                        g.predicate,
                        g.samples,
                        g.brier,
                        g.ece,
                        if g.calibrated { "calibrated" } else { "uncalibrated" }
                    );
                }
            }
            Ok(())
        }
    }
}

/// One synthesis pass from `source` into `into`, printing its report. The operator's
/// `memory synthesize` and a scheduled `synthesize` job both run it
/// (`read.synthesize.cadence`).
#[allow(clippy::too_many_arguments)]
fn synthesize(
    located: &crate::project::Located,
    authority: &contextful_policy::verify::AdmittedAuthority,
    revocation: &contextful_policy::revoke::RevocationState,
    source: &str,
    into: &str,
    endpoint: &str,
    model: &str,
    now: Instant,
) -> Result<()> {
    let boundary = || effect_boundary(authority, &Admission::new(SystemClock.now(), revocation));
    let face = face(located)?;
    let (node, _) = node::resolve(face.store(), |k| std::env::var(k).ok())?;
    let inference = crate::telemetry::traced(Endpoint::new(endpoint, model, std::env::var(INFERENCE_KEY_VAR).ok()).map_err(anyhow::Error::msg)?);
    // Machine-local state: pass cursors live beside the run state, outside the store root.
    let state = located.project.memory_dir();
    let report = Pass { face: &face, authority, inference: &inference, source, into, state: &state, node: &node, now, boundary: &boundary }.run()?;
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

/// Fire a `synthesize` job: admit its credential and run one pass into its target.
pub(crate) fn fire_synthesis(
    located: &crate::project::Located,
    project: &crate::run::ProjectArgs,
    admit: &AdmitArgs,
    job: &contextful_core::job::Synthesis,
    into: &str,
) -> Result<()> {
    let (authority, revocation) = admit.admit(project.project.as_deref(), "a synthesize job")?;
    let now = crate::run::clock(&project.now)?.now();
    synthesize(located, &authority, &revocation, &job.source, into, &job.endpoint, &job.model, now)
}
