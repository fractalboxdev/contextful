//! `contextful memory` — synthesis, direct write, keyed recall, settlement and the knowledge card.
//!
//! The adapter admits a credential, opens the read face, wires the inference endpoint
//! for synthesis, and prints the selected command's result. No memory rule lives here.

use crate::admit::{face, AdmitArgs};
use crate::project::locate;
use crate::clock::SystemClock;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use contextful_context::node;
use contextful_core::memory::declare::Shape;
use contextful_core::memory::settle::{Observation as SettleObservation, RawRegistration};
use contextful_core::memory::synthesize::CandidateClaim;
use contextful_core::ports::Clock;
use contextful_agent::mcp::Server;
use contextful_policy::verify::{effect_boundary, Admission, AdmittedAuthority};
use contextful_memory::synthesize::Pass;
use contextful_memory::entity::{self, EntityInput};
use contextful_memory::relations;
use contextful_core::time::Instant;
use contextful_memory::settle;
use contextful_memory::write::{write_observed, Observation};
use contextful_outbound::infer::Endpoint;
use contextful_policy::enforce::session::Request;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

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
pub enum EntityCmd {
    /// Land one canonical identity into a `memory_entities` table. A held id keeps its
    /// stored aliases; a name or alias another entity carries refuses.
    Upsert {
        #[command(flatten)]
        project: Project,
        /// The `memory_entities` table to land into.
        #[arg(long)]
        table: String,
        /// The entity's key.
        #[arg(long)]
        id: String,
        #[arg(long)]
        kind: String,
        /// The canonical name, the deterministic key a mention resolves on.
        #[arg(long)]
        name: String,
        /// Another name the entity answers to; repeatable.
        #[arg(long = "alias")]
        aliases: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum MemoryCmd {
    /// Add or update a canonical identity.
    Entity {
        #[command(subcommand)]
        command: EntityCmd,
    },
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
        /// The issuer key that signs the read's audit entry; absent, the entry lands unanchored.
        #[arg(long)]
        issuer_key: Option<PathBuf>,
    },
    /// Resolve a name or alias to its canonical record and the live claims about it.
    Explain {
        #[command(flatten)]
        project: Project,
        /// The memory entities table resolved against.
        #[arg(long)]
        table: String,
        /// A canonical name or an alias, in any script.
        #[arg(long)]
        reference: String,
        /// The memory facts table whose claims about the identity the card carries.
        #[arg(long)]
        claims: Option<String>,
        /// Return the internals object: the statements run, the engine, the limit, the rows.
        #[arg(long)]
        internals: bool,
        /// The issuer key that signs the read's audit entry; absent, the entry lands unanchored.
        #[arg(long)]
        issuer_key: Option<PathBuf>,
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
        /// The registration as one JSON object (`read.settle.registration`):
        /// `prediction_id` (caller-chosen), `subject`, `predicted_at` (RFC 3339); exactly
        /// one of `horizon_secs`, `deadline_at` (RFC 3339 or `YYYY-MM-DD`, through that
        /// UTC day) or `watch: true`; `resolution_source` (`metric`, `adjudicator` or
        /// `manual`); `comparator` exactly for `metric`; optional `statement`, the
        /// falsifiable claim, at most 4096 B; optional `metric`, `<table>:<series_id>`,
        /// for a `metric` source only.
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
        /// The observation as one JSON object (`read.settle.source-mismatch`):
        /// `prediction_id`, `observed_at` (RFC 3339), and optionally `verdict` (boolean),
        /// `resolution_source` (the registration's source, required with a verdict),
        /// `signal` (`self-rated` keeps it out of the label view), `settling_citation`
        /// (an http or https URL, required for an `adjudicator` or `manual` verdict) and
        /// `metric` (the registration's series, required with a verdict on one).
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
        MemoryCmd::Entity { command: EntityCmd::Upsert { project, table, id, kind, name, aliases } } => {
            let (authority, revocation) = project.admit.admit(project.project.as_deref(), "an entity upsert")?;
            let boundary = || effect_boundary(&authority, &Admission::new(SystemClock.now(), &revocation));
            let face = face(&locate(project.project.as_deref(), project.declaration.clone())?)?;
            let (node, _) = node::resolve(face.store(), |k| std::env::var(k).ok())?;
            let input = EntityInput { entity_id: id, kind, name, aliases };
            let landed = entity::upsert(&face, &authority, &table, input, &node, SystemClock.now(), &boundary)?;
            let verb = if landed.replaced { "updated" } else { "added" };
            println!("{table}: {verb} {} (aliases: {})", landed.entity_id, landed.aliases.join(", "));
            Ok(())
        }
        MemoryCmd::Relations { command: RelationsCmd::Rename { from, to, project } } => {
            let (authority, revocation) = project.admit.admit(project.project.as_deref(), "a relation rename")?;
            let boundary = || effect_boundary(&authority, &Admission::new(SystemClock.now(), &revocation));
            let face = face(&locate(project.project.as_deref(), project.declaration.clone())?)?;
            let (node, _) = node::resolve(face.store(), |k| std::env::var(k).ok())?;
            let (table, count) = relations::rename(&face, &authority, &from, &to, &node, SystemClock.now(), &boundary)?;
            println!("{table}: {count} edges renamed from {from} to {to}");
            Ok(())
        }
        MemoryCmd::Recall { project, table, subject, observed_at, as_of_ingest, limit, issuer_key } => {
            let mut arguments = json!({ "table": table, "subject": subject });
            for (name, value) in [("observed_at", observed_at), ("as_of_ingest", as_of_ingest)] {
                if let Some(value) = value {
                    arguments[name] = json!(value);
                }
            }
            if let Some(limit) = limit {
                arguments["limit"] = json!(limit);
            }
            audited_read(&project, issuer_key.as_deref(), "memory recall", "memory.recall", arguments)
        }
        MemoryCmd::Explain { project, table, reference, claims, internals, issuer_key } => {
            let mut arguments = json!({ "table": table, "reference": reference, "internals": internals });
            if let Some(claims) = claims {
                arguments["claims"] = json!(claims);
            }
            audited_read(&project, issuer_key.as_deref(), "a knowledge card", "memory.explain", arguments)
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
            let report = settle::calibration(&face, &session, &table, predictions, outcomes)?;
            if json {
                let groups: Vec<serde_json::Value> = report.groups.iter().map(settle::report_json).collect();
                println!("{}", serde_json::json!({ "joined": report.joined, "groups": groups }));
            } else {
                println!("{} settled predictions joined a claim in {table}", report.joined);
                for g in &report.groups {
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

/// One read tool call through the tool server, so the command line audits a read exactly
/// as the tool protocol does (`disclosure.record.read-entry`), printing its structured result.
fn audited_read(project: &Project, issuer_key: Option<&Path>, purpose: &str, tool: &str, arguments: Value) -> Result<()> {
    let (authority, revocation) = project.admit.admit(project.project.as_deref(), purpose)?;
    let located = locate(project.project.as_deref(), project.declaration.clone())?;
    let face = crate::admit::face_with_pins(&located, project.admit.public_key.as_deref(), project.admit.keyset.as_deref())?;
    let audit = crate::project::read_audit(&located.project, issuer_key, project.admit.public_key.as_deref(), project.admit.keyset.as_deref())?;
    let clock = SystemClock;
    let boundary = |authority: &AdmittedAuthority| effect_boundary(authority, &Admission::new(clock.now(), &revocation));
    let server = Server::new(&face, authority, &boundary, &clock, &audit).map_err(anyhow::Error::msg)?.with_build(crate::build_identity());
    let message = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": tool, "arguments": arguments } });
    let answer = server.handle(&message.to_string()).with_context(|| format!("the `{tool}` request has no answer"))?;
    if answer.get("error").is_some() || answer["result"]["isError"] == true {
        bail!("{answer}");
    }
    println!("{}", serde_json::to_string(&answer["result"]["structuredContent"])?);
    Ok(())
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
