//! The entity upsert: an operator's canonical identity, landed into a `memory_entities`
//! table under its key.

use crate::claims::{objects, require, Boundary};
use crate::MemoryFault;
use contextful_context::land::{land, Batch, RunContext};
use contextful_context::read::{Face, ReadOptions};
use contextful_context::store::{FileLock, LOCK_WAIT_SECS};
use contextful_core::grant::Action;
use contextful_core::memory::declare::Shape;
use contextful_core::memory::MemoryError;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reconcile::ColumnType;
use contextful_core::store::reserve::{Injection, INGESTED_AT};
use contextful_core::time::Instant;
use contextful_core::store::relation::{ident, literal};
use contextful_policy::enforce::session::{Request, Session};
use contextful_policy::verify::AdmittedAuthority;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
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

/// The microsecond `_ingested_at` of `entity_id`'s live row; the response renders the
/// stamp at coarser precision, so the statement reads it as an integer.
fn newest_stamp(face: &Face, session: &Session, table: &str, entity_id: &str) -> Result<Option<Instant>, MemoryFault> {
    let sql = format!(
        "SELECT CAST(epoch_us(max({at})) AS VARCHAR) FROM {table} WHERE {key} = {id}",
        at = ident(INGESTED_AT),
        table = ident(table),
        key = ident("entity_id"),
        id = literal(entity_id),
    );
    let response = face.query(session, &sql, ReadOptions::default())?;
    let Some(Value::String(stamp)) = response.rows.first().and_then(|r| r.first()) else { return Ok(None) };
    let micros = stamp.parse::<i128>().map_err(|e| MemoryFault::Invalid(format!("`{table}` stamp `{stamp}`: {e}")))?;
    Ok(Some(Instant::from_unix_nanos(micros * 1_000)?))
}

/// The lock file an upsert holds in its table's directory, so concurrent upserts on one
/// table read, check and land one after another (`read.resolve-entity.upsert-serial`).
const UPSERT_LOCK_FILE: &str = "entity-upsert.lock";

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
    // Synthesis resolves mentions against the first declared entities table alone.
    let table = face.memory().of_shape(Shape::Entities).ok_or_else(|| {
        MemoryFault::Undeclared(format!("`{into}` is no memory_entities table the manifest declares"))
    })?;
    if table.name != into {
        return Err(MemoryFault::Undeclared(format!(
            "`{into}` is not `{}`, the memory_entities table mentions resolve against",
            table.name
        )));
    }
    let decl = face.decl(&table.name);
    if let Some(available) = &decl.available_time {
        return Err(MemoryError::EntityAvailabilityDeclared(format!(
            "`{into}` declares availability column `{}`, and an upsert carries no source availability instant",
            available.column
        ))
        .into());
    }
    let input = EntityInput {
        entity_id: input.entity_id.trim().to_string(),
        kind: input.kind.trim().to_string(),
        name: input.name.trim().to_string(),
        aliases: input.aliases.iter().map(|a| a.trim().to_string()).collect(),
    };
    for (field, value) in [("id", &input.entity_id), ("kind", &input.kind), ("name", &input.name)] {
        if value.is_empty() {
            return Err(MemoryError::EntityFieldEmpty(format!("the entity's {field} is empty")).into());
        }
    }
    if input.aliases.iter().any(String::is_empty) {
        return Err(MemoryError::EntityFieldEmpty("an alias is empty".into()).into());
    }
    require(authority, Action::Read, &table.name)?;
    require(authority, Action::Write, &table.name)?;
    let dir = face.store().table_dir(&table.name)?;
    std::fs::create_dir_all(&dir).map_err(|source| MemoryFault::Io { path: dir.clone(), source })?;
    let _serial = FileLock::acquire(&dir.join(UPSERT_LOCK_FILE), std::time::Duration::from_secs(LOCK_WAIT_SECS))?;
    let session = face.session(authority, &Request::default(), Bounds::default())?;
    let response = face.rows(&session, &table.name, None)?;
    if response.truncated {
        return Err(MemoryError::EntityTableTruncated(format!(
            "`{into}` holds more rows than one read of this session returns, so the merge and the conflict check would see part of it"
        ))
        .into());
    }
    let rows = objects(&response);
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
    // The newest `_ingested_at` copy of a key is its live row, so a landing stamped no
    // later than the held copy would lose to it (`read.resolve-entity.upsert-serial`).
    let committed_at = match held {
        Some(_) => match newest_stamp(face, &session, &table.name, &input.entity_id)? {
            Some(prior) => now.max(Instant::from_unix_nanos(prior.unix_nanos() + 1_000)?),
            None => now,
        },
        None => now,
    };
    let types: HashMap<String, ColumnType> = Shape::Entities.canonical_columns().iter().map(|c| (c.to_string(), ColumnType::Utf8)).collect();
    let context = RunContext {
        node: node.clone(),
        injection: Injection {
            // The row's digest keeps two upserts landing distinct rows at one instant apart.
            run_id: format!("memory-entity-upsert-{}-{}", committed_at.unix_nanos(), &format!("{:x}", Sha256::digest(row.to_string()))[..16]),
            site_id: "memory".into(),
            batch_seq: Some(0),
            authored_by: authority.subject().on_behalf_of().map(str::to_string),
            authored_agent: authority.subject().agent().map(str::to_string),
            taint: None,
        },
        committed_at,
    };
    boundary()?;
    let rows = vec![row.as_object().expect("an entity row is an object").clone()];
    land(face.store(), &decl, &Batch { rows, types }, &context)?;
    Ok(Upserted { entity_id: input.entity_id, aliases: merged, replaced: held.is_some() })
}
