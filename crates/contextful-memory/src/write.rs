//! The direct write: a principal's claim, stamped `curated` and revised like any other.

use super::claims::{read_claims, require, Boundary, Landing, Writer};
use super::MemoryFault;
use contextful_context::read::Face;
use contextful_core::grant::Action;
use contextful_core::memory::revise::{claim_id, direct_write, revise, tier, Claim, WritePath};
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

/// Write one claim into a memory table. The table's shape decides whether the direct
/// write accepts it (`read.revise.direct-write`); the claim passes the validation a
/// synthesized claim does, and the writer's authority is re-read just before it lands.
pub fn write_claim(
    face: &Face,
    authority: &AdmittedAuthority,
    into: &str,
    candidate: CandidateClaim,
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
    let live = read_claims(face, &session, into)?;
    let claim = Claim {
        claim_id: claim_id(&candidate.subject, &candidate.predicate, &candidate.object, candidate.scope.as_deref()),
        subject: candidate.subject,
        predicate: candidate.predicate,
        object: candidate.object,
        scope: candidate.scope,
        tier: tier(WritePath::Direct, &[]),
        confidence: candidate.confidence,
        valid_from: now,
        valid_to: None,
        evidence: candidate.evidence,
        superseded_by: None,
        grant_id: writer.grant_id.clone(),
        agent: writer.agent.clone(),
    };
    let revision = revise(claim, &live);
    let mut writes = revision.retired.clone();
    writes.extend(revision.landed.iter().cloned());
    let run_id = format!("memory-write-{}", now.unix_nanos());
    Landing { node, at: now, writer: &writer, run_id, boundary }.commit(face, into, &writes, &[])?;
    Ok(Written { claim: revision.landed, retired: revision.retired })
}
