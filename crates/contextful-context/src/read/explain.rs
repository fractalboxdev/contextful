//! `memory.explain`: the knowledge card. One reference resolves against a
//! `memory_entities` table to its canonical record, beside the claims recall serves about
//! it. The declared identities a session reads are shared with synthesis's Resolve step.

use super::face::{internals_block, Face};
use super::fault::ReadFault;
use super::recall::RecallRequest;
use contextful_core::enforce::EnforceError;
use contextful_core::memory::declare::Shape;
use contextful_core::memory::resolve::{resolve, Entity, Resolution};
use contextful_core::memory::MemoryError;
use contextful_core::read::respond::Response;
use contextful_core::read::Refusal;
use contextful_core::store::bound_time::{Bound, Bounds};
use contextful_core::store::relation::ident;
use contextful_core::time::Instant;
use contextful_policy::enforce::session::Session;
use serde_json::{json, Map, Value};

/// What a knowledge card asks for.
#[derive(Debug, Clone)]
pub struct ExplainRequest {
    /// The `memory_entities` table resolved against.
    pub table: String,
    /// A canonical name or an alias, in any script.
    pub reference: String,
    /// The `memory_facts` table whose claims about the identity the card carries.
    pub claims: Option<String>,
    /// The instant the call is made at; claims are those live at it.
    pub anchor: Instant,
    /// The availability bound a credential's horizon imposes; claims then read at it.
    pub available_as_of: Option<Bound>,
    /// The budgets the claims read runs under (`read.register.budget-arguments`).
    pub max_duration_ms: Option<u64>,
    pub max_response_bytes: Option<u64>,
    /// Return the internals object (`read.respond.internals-opt-in`).
    pub internals: bool,
}

impl ExplainRequest {
    /// A card for `reference` in `table`, asked at `anchor`, naming no claims table.
    pub fn new(table: impl Into<String>, reference: impl Into<String>, anchor: Instant) -> ExplainRequest {
        ExplainRequest { table: table.into(), reference: reference.into(), claims: None, anchor, available_as_of: None, max_duration_ms: None, max_response_bytes: None, internals: false }
    }

    /// The bounds the session serving this card opens under.
    pub fn bounds(&self) -> Bounds {
        Bounds { available_as_of: self.available_as_of, ..Bounds::default() }
    }
}

/// One identity row: the resolver's key, name and aliases, beside its `kind`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityRecord {
    pub entity: Entity,
    pub kind: Option<String>,
}

/// The block a read's internals ride under (`read.respond.internals-opt-in`).
const INTERNALS: &str = "contextful.internals";

/// `card` with the internals of the statements it ran, when the request asks for them: the
/// identity read, then the claims read, under the claims read's ceiling. The row count
/// counts the identity and each claim.
fn with_internals(mut card: Value, request: &ExplainRequest, executed: &[String], limit: Option<u64>, started: std::time::Instant) -> Value {
    if request.internals {
        let rows = u64::from(!card["entity"].is_null()) + card["claims"]["rows"].as_array().map_or(0, |r| r.len() as u64);
        card[INTERNALS] = internals_block(executed, limit, rows, started);
    }
    card
}

fn objects(r: &Response) -> Vec<Map<String, Value>> {
    r.rows.iter().map(|row| r.columns.iter().cloned().zip(row.iter().cloned()).collect()).collect()
}

impl Face {
    /// The knowledge card of `request.reference` (`read.resolve-entity.knowledge-card`):
    /// one identity, or its candidates when several match and none when nothing does
    /// (`read.resolve-entity.card-candidates`). Claims ride the card only where the session
    /// registers the claims table (`read.resolve-entity.card-authorization`), read by keyed
    /// recall at the call instant (`read.resolve-entity.card-claims`).
    pub fn explain(&self, session: &Session, request: &ExplainRequest) -> Result<Value, ReadFault> {
        self.explain_read(session, request).map(|(card, _)| card)
    }

    /// [`Face::explain`] beside every table the card read under the session: the entities
    /// table, then each table [`Face::recall_read`] read for its claims. The read entry names
    /// them (`disclosure.record.read-attributes`).
    pub fn explain_read(&self, session: &Session, request: &ExplainRequest) -> Result<(Value, Vec<String>), ReadFault> {
        let started = std::time::Instant::now();
        let table = request.table.as_str();
        let Some(relation) = session.relation(table) else {
            return Err(EnforceError::UnknownRelation(format!("`{table}`")).into());
        };
        if self.memory().table(table).is_none_or(|t| t.shape != Shape::Entities) {
            let why = format!("`{table}` declares no `{}` shape, so it holds no identity to explain", Shape::Entities.name());
            return Err(Refusal::from(MemoryError::ExplainNotEntities(why)).into());
        }
        let mut executed = vec![format!("SELECT * FROM {}", ident(relation.name()))];
        let records = self.entities(session, table)?;
        let entities: Vec<Entity> = records.iter().map(|r| r.entity.clone()).collect();
        let id = match resolve(&request.reference, &entities) {
            Resolution::One(id) => id,
            Resolution::Ambiguous(ids) => return Ok((with_internals(json!({ "entity": null, "candidates": ids }), request, &executed, None, started), vec![table.to_string()])),
            Resolution::Unresolved => return Ok((with_internals(json!({ "entity": null, "candidates": [] }), request, &executed, None, started), vec![table.to_string()])),
        };
        let record = records.iter().find(|r| r.entity.entity_id == id).expect("a resolved id names a read identity");
        let mut card = json!({
            "entity": {
                "entity_id": record.entity.entity_id,
                "kind": record.kind,
                "name": record.entity.name,
                "aliases": record.entity.aliases,
            },
            "candidates": [],
        });
        let mut limit = None;
        let mut tables = vec![table.to_string()];
        if let Some(claims) = request.claims.as_deref().filter(|c| session.relation(c).is_some()) {
            // Synthesis keys a resolved claim by canonical name, a direct write by whatever
            // subject it names; both keys reach the card (`read.resolve-entity.card-claims`).
            let recall = RecallRequest {
                also_subjects: vec![record.entity.name.clone()],
                observed_at: request.available_as_of,
                available_as_of: request.available_as_of,
                max_duration_ms: request.max_duration_ms,
                max_response_bytes: request.max_response_bytes,
                internals: request.internals,
                ..RecallRequest::new(claims, id.clone(), request.anchor)
            };
            let (answer, read) = self.recall_read(session, &recall)?;
            let mut answer = answer.to_json();
            tables.extend(read);
            if let Some(Value::Object(internals)) = answer.as_object_mut().and_then(|a| a.remove(INTERNALS)) {
                executed.extend(internals.get("sql").and_then(Value::as_str).map(str::to_string));
                limit = internals.get("limit").and_then(Value::as_u64);
            }
            card["claims"] = answer;
        }
        Ok((with_internals(card, request, &executed, limit, started), tables))
    }

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
