//! The entity upsert: an operator's canonical identity, landed into a `memory_entities`
//! table under its key.

use crate::claims::{objects, require, Boundary};
use crate::MemoryFault;
use contextful_context::land::{land, Batch, RunContext};
use contextful_context::read::Face;
use contextful_core::grant::Action;
use contextful_core::memory::declare::Shape;
use contextful_core::memory::MemoryError;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reconcile::ColumnType;
use contextful_core::store::reserve::Injection;
use contextful_core::time::Instant;
use contextful_policy::enforce::session::Request;
use contextful_policy::verify::AdmittedAuthority;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// One canonical identity as an upsert gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityInput {
    pub entity_id: String,
    pub kind: String,
    pub name: String,
    pub aliases: Vec<String>,
}

/// The row an upsert landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upserted {
    pub entity_id: String,
    pub aliases: Vec<String>,
    /// Whether the table already held the id.
    pub replaced: bool,
}

fn fold(s: &str) -> String {
    s.trim().to_lowercase()
}

fn aliases(row: &Map<String, Value>) -> Vec<String> {
    row.get("aliases").and_then(Value::as_str).and_then(|a| serde_json::from_str::<Vec<String>>(a).ok()).unwrap_or_default()
}

/// Land one entity into `into` (`read.resolve-entity.entity-upsert`). A held id keeps
/// its stored aliases ahead of each new one (`read.resolve-entity.upsert-merge`); a name
/// or alias another entity carries refuses (`read.resolve-entity.alias-conflict`).
pub fn upsert(
    face: &Face,
    authority: &AdmittedAuthority,
    into: &str,
    input: EntityInput,
    node: &NodeId,
    now: Instant,
    boundary: &Boundary<'_>,
) -> Result<Upserted, MemoryFault> {
    let table = face.memory().table(into).filter(|t| t.shape == Shape::Entities).ok_or_else(|| {
        MemoryFault::Undeclared(format!("`{into}` is no memory_entities table the manifest declares"))
    })?;
    for (field, value) in [("id", &input.entity_id), ("kind", &input.kind), ("name", &input.name)] {
        if value.trim().is_empty() {
            return Err(MemoryFault::Invalid(format!("the entity's {field} is empty")));
        }
    }
    if input.aliases.iter().any(|a| a.trim().is_empty()) {
        return Err(MemoryFault::Invalid("an alias is empty".into()));
    }
    require(authority, Action::Read, &table.name)?;
    require(authority, Action::Write, &table.name)?;
    let session = face.session(authority, &Request::default(), Bounds::default())?;
    let rows = objects(&face.rows(&session, &table.name, None)?);
    let held = rows.iter().find(|r| r.get("entity_id").and_then(Value::as_str) == Some(input.entity_id.as_str()));
    let mut merged = held.map(aliases).unwrap_or_default();
    for alias in input.aliases {
        if !merged.iter().any(|a| fold(a) == fold(&alias)) {
            merged.push(alias);
        }
    }
    let mentions: Vec<String> = std::iter::once(&input.name).chain(&merged).map(|m| fold(m)).collect();
    for other in rows.iter().filter(|r| r.get("entity_id").and_then(Value::as_str) != Some(input.entity_id.as_str())) {
        let id = other.get("entity_id").and_then(Value::as_str).unwrap_or_default();
        let names = other.get("name").and_then(Value::as_str).map(str::to_string).into_iter().chain(aliases(other));
        if let Some(taken) = names.into_iter().find(|n| mentions.contains(&fold(n))) {
            return Err(MemoryError::EntityAliasConflict(format!("`{taken}` already names entity `{id}`")).into());
        }
    }
    let row = json!({
        "entity_id": input.entity_id, "kind": input.kind, "name": input.name,
        "aliases": serde_json::to_string(&merged).expect("aliases serialize"),
    });
    let types: HashMap<String, ColumnType> = Shape::Entities.canonical_columns().iter().map(|c| (c.to_string(), ColumnType::Utf8)).collect();
    let context = RunContext {
        node: node.clone(),
        injection: Injection {
            run_id: format!("memory-entity-upsert-{}", now.unix_nanos()),
            site_id: "memory".into(),
            batch_seq: Some(0),
            authored_by: authority.subject().on_behalf_of().map(str::to_string),
            taint: None,
        },
        committed_at: now,
    };
    boundary()?;
    let rows = vec![row.as_object().expect("an entity row is an object").clone()];
    land(face.store(), &face.decl(&table.name), &Batch { rows, types }, &context)?;
    Ok(Upserted { entity_id: input.entity_id, aliases: merged, replaced: held.is_some() })
}
