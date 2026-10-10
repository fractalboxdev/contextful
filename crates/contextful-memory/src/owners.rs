//! Ownership answers: the principals an artifact's `owned_by` edges attach, read through the
//! caller's enforced session at its frontier (`read.resolve-entity.graph-index`).

use crate::MemoryFault;
use contextful_context::read::{Face, ReadOptions};
use contextful_core::memory::declare::Shape;
use contextful_core::store::relation::{ident, literal};
use contextful_core::store::reserve::INGESTED_AT;
use contextful_core::time::Instant;
use contextful_policy::enforce::session::Session;
use serde_json::Value;
use std::collections::BTreeMap;

/// The relation type attaching an owning principal to an artifact.
pub const OWNED_BY: &str = "owned_by";

/// One attached principal and the instant its newest attachment committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    pub principal: String,
    pub attached_at: Instant,
}

/// Every principal an `owned_by` edge from `artifact` in `edges` attaches, as `session`
/// reads them: newest attachment first, principal id breaking equal-instant ties
/// (`read.resolve-entity.ownership-answer`). The statement runs on the read face under the
/// session's grants, policies and bounds, so no other index answers it.
pub fn owners(face: &Face, session: &Session, edges: &str, artifact: &str) -> Result<Vec<Owner>, MemoryFault> {
    if face.memory().table(edges).is_none_or(|t| t.shape != Shape::Edges) {
        return Err(MemoryFault::Undeclared(format!("`{edges}` declares no `{}` shape", Shape::Edges.name())));
    }
    let sql = format!(
        "SELECT {target}, {at} FROM {table} WHERE {rel} = {owned} AND {source} = {artifact}",
        target = ident("target_id"),
        at = ident(INGESTED_AT),
        table = ident(edges),
        rel = ident("rel_type"),
        owned = literal(OWNED_BY),
        source = ident("source_id"),
        artifact = literal(artifact),
    );
    let response = face.query(session, &sql, ReadOptions::default())?;
    if response.truncated {
        // A cut answer would drop owners silently; every attached principal or none.
        return Err(MemoryFault::Invalid(format!("`{artifact}` holds more `{OWNED_BY}` edges than one read returns")));
    }
    let mut newest: BTreeMap<String, Instant> = BTreeMap::new();
    for row in &response.rows {
        let (Some(Value::String(principal)), Some(Value::String(at))) = (row.first(), row.get(1)) else { continue };
        let at = Instant::parse(at)?;
        let entry = newest.entry(principal.clone()).or_insert(at);
        *entry = (*entry).max(at);
    }
    let mut owners: Vec<Owner> = newest.into_iter().map(|(principal, attached_at)| Owner { principal, attached_at }).collect();
    owners.sort_by(|a, b| b.attached_at.cmp(&a.attached_at).then_with(|| a.principal.cmp(&b.principal)));
    Ok(owners)
}
