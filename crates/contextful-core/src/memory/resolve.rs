//! `read.resolve-entity`: mentions to canonical identities, and candidate edges.

use super::declare::MemoryDeclarations;
use super::synthesize::CandidateEdge;
use super::MemoryError;

/// One canonical identity: its key, its name and its aliases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entity {
    pub entity_id: String,
    pub name: String,
    pub aliases: Vec<String>,
}

/// A mention's resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    One(String),
    Unresolved,
    Ambiguous(Vec<String>),
}

fn fold(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Resolve a mention. An exact canonical name is the deterministic key and wins alone;
/// otherwise names and aliases compare case-folded, and two identities matching with no
/// key between them is ambiguous.
pub fn resolve(mention: &str, entities: &[Entity]) -> Resolution {
    let exact: Vec<&Entity> = entities.iter().filter(|e| e.name == mention.trim()).collect();
    if let [one] = exact.as_slice() {
        return Resolution::One(one.entity_id.clone());
    }
    let m = fold(mention);
    let mut ids: Vec<String> = entities
        .iter()
        .filter(|e| fold(&e.name) == m || e.aliases.iter().any(|a| fold(a) == m))
        .map(|e| e.entity_id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    match ids.len() {
        0 => Resolution::Unresolved,
        1 => Resolution::One(ids.remove(0)),
        _ => Resolution::Ambiguous(ids),
    }
}

/// A mention that must resolve to one identity: an ambiguous one refuses, recorded as an
/// ambiguous skip (`read.resolve-entity.ambiguous-mention`).
pub fn resolve_mention(mention: &str, entities: &[Entity]) -> Result<Option<String>, MemoryError> {
    match resolve(mention, entities) {
        Resolution::One(id) => Ok(Some(id)),
        Resolution::Unresolved => Ok(None),
        Resolution::Ambiguous(ids) => Err(MemoryError::EntityAmbiguous(format!(
            "`{mention}` matches {} with no key separating them",
            ids.join(", ")
        ))),
    }
}

/// A candidate edge with both endpoints resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEdge {
    pub rel_type: String,
    pub source_id: String,
    pub target_id: String,
}

/// Check a candidate edge: its type in the union of reserved and declared relations
/// (`read.declare.undeclared-relation`), then each endpoint resolved to one identity
/// (`read.resolve-entity.edge-endpoint`).
pub fn check_edge(edge: &CandidateEdge, entities: &[Entity], declarations: &MemoryDeclarations) -> Result<ResolvedEdge, MemoryError> {
    if !declarations.admits_relation(&edge.rel_type) {
        return Err(MemoryError::UndeclaredRelation(format!(
            "`{}` is neither a reserved nor a declared relation type",
            edge.rel_type
        )));
    }
    let endpoint = |mention: &str, end: &str| match resolve_mention(mention, entities)? {
        Some(id) => Ok(id),
        None => Err(MemoryError::EdgeEndpointUnresolved(format!(
            "the {end} `{mention}` of a `{}` edge resolves to no identity",
            edge.rel_type
        ))),
    };
    Ok(ResolvedEdge {
        rel_type: edge.rel_type.clone(),
        source_id: endpoint(&edge.source, "source")?,
        target_id: endpoint(&edge.target, "target")?,
    })
}
