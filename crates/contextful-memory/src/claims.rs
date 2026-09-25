//! Claim rows: reading a claims table through a session, and landing claims with their
//! attribution.

use super::MemoryFault;
use contextful_context::land::{land, Batch, RunContext};
use contextful_context::read::Face;
use contextful_core::grant::Action;
use contextful_core::memory::revise::{Claim, Tier};
use contextful_core::memory::synthesize::EvidenceRef;
use contextful_core::read::respond::Response;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reconcile::ColumnType;
use contextful_core::store::reserve::Injection;
use contextful_core::time::Instant;
use contextful_policy::enforce::session::Session;
use contextful_policy::verify::AdmittedAuthority;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// Who writes: the grant's chain-final revocation identifier, its agent, and its
/// on-behalf-of principal (`read.synthesize.attribution`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Writer {
    pub grant_id: String,
    pub agent: Option<String>,
    pub on_behalf_of: Option<String>,
}

impl Writer {
    pub fn of(authority: &AdmittedAuthority) -> Writer {
        Writer {
            grant_id: authority.revocation_ids().last().cloned().unwrap_or_else(|| authority.credential_id().to_string()),
            agent: authority.subject().agent().map(str::to_string),
            on_behalf_of: authority.subject().on_behalf_of().map(str::to_string),
        }
    }
}

/// Refuse unless one grant carries `action` over `table`.
pub fn require(authority: &AdmittedAuthority, action: Action, table: &str) -> Result<(), MemoryFault> {
    if authority.permits(action, &[table]) {
        Ok(())
    } else {
        Err(MemoryFault::Denied(format!("the credential holds no {action:?} grant on `{table}`").to_lowercase()))
    }
}

fn text(row: &Map<String, Value>, k: &str) -> Option<String> {
    row.get(k).and_then(Value::as_str).map(str::to_string)
}

fn instant(row: &Map<String, Value>, k: &str) -> Option<Instant> {
    text(row, k).and_then(|s| Instant::parse(&s).ok())
}

/// The rows of a response as objects keyed by column.
pub fn objects(r: &Response) -> Vec<Map<String, Value>> {
    r.rows.iter().map(|row| r.columns.iter().cloned().zip(row.iter().cloned()).collect()).collect()
}

/// Every claim of a claims table the session reads.
pub fn read_claims(face: &Face, session: &Session, table: &str) -> Result<Vec<Claim>, MemoryFault> {
    let rows = face.rows(session, table, None)?;
    Ok(objects(&rows)
        .iter()
        .filter_map(|row| {
            Some(Claim {
                claim_id: text(row, "claim_id")?,
                subject: text(row, "subject")?,
                predicate: text(row, "predicate")?,
                object: text(row, "object")?,
                scope: text(row, "scope"),
                tier: Tier::parse(&text(row, "tier")?)?,
                confidence: row.get("confidence").and_then(Value::as_f64).unwrap_or(0.0),
                valid_from: instant(row, "valid_from")?,
                valid_to: instant(row, "valid_to"),
                evidence: text(row, "evidence").and_then(|e| serde_json::from_str::<Vec<EvidenceRef>>(&e).ok()).unwrap_or_default(),
                superseded_by: text(row, "superseded_by"),
                grant_id: text(row, "grant_id").unwrap_or_default(),
                agent: text(row, "agent"),
            })
        })
        .collect())
}

fn row(c: &Claim) -> Map<String, Value> {
    let evidence = serde_json::to_string(&c.evidence).expect("evidence serializes");
    let v = json!({
        "claim_id": c.claim_id, "subject": c.subject, "predicate": c.predicate, "object": c.object, "scope": c.scope,
        "tier": c.tier.name(), "confidence": c.confidence, "valid_from": c.valid_from.to_rfc3339_nanos(),
        "valid_to": c.valid_to.map(|t| t.to_rfc3339_nanos()), "evidence": evidence, "superseded_by": c.superseded_by,
        "grant_id": c.grant_id, "agent": c.agent,
    });
    v.as_object().expect("a claim row is an object").clone()
}

/// The column types every claim row lands under, whatever a batch's values are.
fn claim_types() -> HashMap<String, ColumnType> {
    let mut types: HashMap<String, ColumnType> = ["claim_id", "subject", "predicate", "object", "scope", "tier", "evidence", "superseded_by", "grant_id", "agent"]
        .iter()
        .map(|c| (c.to_string(), ColumnType::Utf8))
        .collect();
    types.insert("confidence".into(), ColumnType::Float64);
    types.insert("valid_from".into(), ColumnType::Timestamp);
    types.insert("valid_to".into(), ColumnType::Timestamp);
    types
}

/// Where and when a write lands.
pub struct Landing<'a> {
    pub node: &'a NodeId,
    pub at: Instant,
    pub writer: &'a Writer,
}

impl Landing<'_> {
    fn context(&self) -> RunContext {
        RunContext {
            node: self.node.clone(),
            injection: Injection {
                run_id: format!("memory-{}", self.at.unix_nanos()),
                site_id: "memory".into(),
                batch_seq: Some(0),
                authored_by: self.writer.on_behalf_of.clone(),
            },
            committed_at: self.at,
        }
    }

    /// Land claims as one run; a later version of a claim replaces the earlier on read.
    pub fn claims(&self, face: &Face, table: &str, claims: &[Claim]) -> Result<(), MemoryFault> {
        if claims.is_empty() {
            return Ok(());
        }
        let batch = Batch { rows: claims.iter().map(row).collect(), types: claim_types() };
        land(face.store(), &face.decl(table), &batch, &self.context())?;
        Ok(())
    }

    /// Land dead-lettered items into `<table>_dead_letter`.
    pub fn dead_letters(&self, face: &Face, table: &str, items: &[Value]) -> Result<(), MemoryFault> {
        if items.is_empty() {
            return Ok(());
        }
        let rows = items.iter().filter_map(|v| v.as_object().cloned()).collect();
        let name = dead_letter_table(table);
        land(face.store(), &face.decl(&name), &Batch { rows, types: HashMap::new() }, &self.context())?;
        Ok(())
    }
}

/// The dead-letter table beside a memory table.
pub fn dead_letter_table(table: &str) -> String {
    format!("{table}_dead_letter")
}
