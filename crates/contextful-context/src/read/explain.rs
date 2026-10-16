//! The declared identities of a `memory_entities` table as a session reads them, shared by
//! synthesis's Resolve step and the knowledge card.

use super::face::Face;
use super::fault::ReadFault;
use contextful_core::memory::resolve::Entity;
use contextful_core::read::respond::Response;
use contextful_policy::enforce::session::Session;
use serde_json::{Map, Value};

/// One identity row: the resolver's key, name and aliases, beside its `kind`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityRecord {
    pub entity: Entity,
    pub kind: Option<String>,
}

fn objects(r: &Response) -> Vec<Map<String, Value>> {
    r.rows.iter().map(|row| r.columns.iter().cloned().zip(row.iter().cloned()).collect()).collect()
}

impl Face {
    /// Every row of `table` the session reads that carries an `entity_id` and a `name`.
    /// `aliases` is a JSON list of strings; any other value reads as none.
    pub fn entities(&self, session: &Session, table: &str) -> Result<Vec<EntityRecord>, ReadFault> {
        Ok(objects(&self.rows(session, table, None)?)
            .iter()
            .filter_map(|row| {
                Some(EntityRecord {
                    entity: Entity {
                        entity_id: row.get("entity_id")?.as_str()?.to_string(),
                        name: row.get("name")?.as_str()?.to_string(),
                        aliases: row
                            .get("aliases")
                            .and_then(Value::as_str)
                            .and_then(|a| serde_json::from_str::<Vec<String>>(a).ok())
                            .unwrap_or_default(),
                    },
                    kind: row.get("kind").and_then(Value::as_str).map(str::to_string),
                })
            })
            .collect())
    }
}
