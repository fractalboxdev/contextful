//! The direct write: a principal's claim, stamped `curated` and revised like any other.

use super::claims::{read_claims, require, Boundary, Landing, Writer};
use super::MemoryFault;
use contextful_context::read::evidence::Stamped;
use contextful_context::read::Face;
use contextful_core::grant::Action;
use contextful_core::memory::MemoryError;
use contextful_core::memory::revise::{claim_id, direct_write, keyed_claim_id, observed_order, revise, tier, Claim, WritePath};
use contextful_core::memory::synthesize::{validate_claim, CandidateClaim};
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::lay_out::NodeId;
use contextful_core::time::Instant;
use contextful_policy::enforce::session::Request;
use contextful_policy::verify::AdmittedAuthority;

/// What a direct write landed.
#[derive(Debug, Clone, PartialEq)]
pub struct Written {
    /// The claim as it landed, or `None` where it restated a live claim.
    pub claim: Option<Claim>,
    pub retired: Vec<Claim>,
}

/// When a directly written claim was observed, and the key a retry of it carries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observation {
    /// The claim's `valid_from`; `None` is the write's own instant (`read.revise.observed-at`).
    pub observed_at: Option<Instant>,
    /// The key `claim_id` derives from (`read.revise.dedup-key`).
    pub dedup_key: Option<String>,
}

/// Write one claim into a memory table, valid from the write's own instant.
pub fn write_claim(
    face: &Face,
    authority: &AdmittedAuthority,
    into: &str,
    candidate: CandidateClaim,
    node: &NodeId,
    now: Instant,
    boundary: &Boundary<'_>,
) -> Result<Written, MemoryFault> {
    write_observed(face, authority, into, candidate, &Observation::default(), node, now, boundary)
}

/// Write one claim into a memory table. The table's shape decides whether the direct
/// write accepts it (`read.revise.direct-write`); the claim passes the validation a
/// synthesized claim does, and the writer's authority is re-read just before it lands.
/// The claim is valid from its observed instant (`read.revise.observed-at`); a dedup key
/// seeds its `claim_id`, and a `claim_id` the table already holds lands nothing
/// (`read.revise.dedup-key`). Each citation carries the key of the row it cites, and a
/// citation into a keyed table naming no live version refuses (`read.revise.citation-live`).
#[allow(clippy::too_many_arguments)]
pub fn write_observed(
    face: &Face,
    authority: &AdmittedAuthority,
    into: &str,
    candidate: CandidateClaim,
    observation: &Observation,
    node: &NodeId,
    now: Instant,
    boundary: &Boundary<'_>,
) -> Result<Written, MemoryFault> {
    let table = face
        .memory()
        .table(into)
        .ok_or_else(|| MemoryFault::Undeclared(format!("`{into}` is no memory table the manifest declares")))?;
    direct_write(into, table.shape)?;
    validate_claim(&candidate).map_err(|why| MemoryFault::Invalid(format!("the claim does not validate: {why}")))?;
    require(authority, Action::Write, into)?;
    let writer = Writer::of(authority);
    let session = face.session(authority, &Request::default(), Bounds::default())?;
    let mut candidate = candidate;
    let stamped = face.stamp_evidence(&session, &mut candidate.evidence)?;
    if let Some(i) = stamped.iter().position(|s| *s == Stamped::NotLive) {
        let r = &candidate.evidence[i];
        return Err(MemoryError::CitationNotLive(format!(
            "`{}` row {}:{} is not its key's live version in this session",
            r.table, r.run, r.seq
        ))
        .into());
    }
    let live = read_claims(face, &session, into)?;
    let id = match observation.dedup_key.as_deref() {
        Some(key) if key.trim().is_empty() => return Err(MemoryFault::Invalid("the dedup key is empty".into())),
        Some(key) => {
            let id = keyed_claim_id(key, &candidate.subject, &candidate.predicate, &candidate.object, candidate.scope.as_deref());
            if live.iter().any(|c| c.claim_id == id) {
                return Ok(Written { claim: None, retired: Vec::new() });
            }
            id
        }
        None => claim_id(&candidate.subject, &candidate.predicate, &candidate.object, candidate.scope.as_deref()),
    };
    let claim = Claim {
        claim_id: id,
        subject: candidate.subject,
        predicate: candidate.predicate,
        object: candidate.object,
        scope: candidate.scope,
        tier: tier(WritePath::Direct, &[]),
        confidence: candidate.confidence,
        valid_from: observation.observed_at.unwrap_or(now),
        valid_to: None,
        evidence: candidate.evidence,
        superseded_by: None,
        grant_id: writer.grant_id.clone(),
        agent: writer.agent.clone(),
    };
    observed_order(&claim, &live)?;
    let revision = revise(claim, &live);
    let mut writes = revision.retired.clone();
    writes.extend(revision.landed.iter().cloned());
    let run_id = format!("memory-write-{}", now.unix_nanos());
    Landing { node, at: now, writer: &writer, run_id, boundary, taint: None }.commit(face, into, &writes, &[])?;
    Ok(Written { claim: revision.landed, retired: revision.retired })
}
