//! `memory.recall`: one subject's claims at an observed instant, under an ingest bound,
//! through the evidence gate.

use super::face::Face;
use super::fault::ReadFault;
use contextful_core::enforce::EnforceError;
use contextful_core::memory::declare::Shape;
use contextful_core::memory::recall::gate;
use contextful_core::memory::MemoryError;
use contextful_core::read::respond::{Cell, Response};
use contextful_core::read::Refusal;
use contextful_core::read::template::{Bindings, Bound as Param};
use contextful_core::store::bound_time::{Bound, Bounds};
use contextful_core::store::relation::ident;
use contextful_core::time::Instant;
use contextful_policy::enforce::session::Session;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// What a keyed recall asks for.
#[derive(Debug, Clone)]
pub struct RecallRequest {
    /// The `memory_facts` table read.
    pub table: String,
    /// The subject every returned claim carries exactly.
    pub subject: String,
    /// The valid-time instant the claims cover; `None` reads at `anchor`.
    pub observed_at: Option<Bound>,
    /// The transaction-time bound; `None` reads the latest committed state.
    pub as_of_ingest: Option<Bound>,
    pub limit: Option<u64>,
    /// The instant the call is made at.
    pub anchor: Instant,
}

impl RecallRequest {
    /// A keyed recall of `subject` in `table`, asked at `anchor`, with every option unset.
    pub fn new(table: impl Into<String>, subject: impl Into<String>, anchor: Instant) -> RecallRequest {
        RecallRequest { table: table.into(), subject: subject.into(), observed_at: None, as_of_ingest: None, limit: None, anchor }
    }

    /// The bounds the session serving this recall opens under: `as_of_ingest` as its
    /// `as_of` (`read.recall.keyed-clocks`). Valid time narrows inside the recall itself.
    pub fn bounds(&self) -> Bounds {
        Bounds { as_of: self.as_of_ingest, valid_as_of: None }
    }
}

impl Face {
    /// The claims of `request.subject`, exactly, whose validity covers `observed_at`
    /// (`read.recall.keyed`), read under `as_of_ingest` through a session opened with
    /// [`RecallRequest::bounds`] (`read.recall.keyed-clocks`). A retired claim answers
    /// inside its interval (`read.recall.keyed-history`); every claim passes the evidence
    /// gate and suppressions are counted (`read.recall.keyed-gate`); claims order tier
    /// first under the least row ceiling (`read.recall.keyed-order`).
    pub fn recall(&self, session: &Session, request: &RecallRequest) -> Result<Response, ReadFault> {
        let table = request.table.as_str();
        let relation = session.relation(table).ok_or_else(|| EnforceError::UnknownRelation(format!("`{table}`")))?;
        if self.memory().table(table).is_none_or(|t| t.shape != Shape::Facts) {
            let why = format!("`{table}` declares no `{}` shape, so it holds no claim to recall", Shape::Facts.name());
            return Err(Refusal::from(MemoryError::RecallNotClaims(why)).into());
        }
        let columns: Vec<String> = Shape::Facts.canonical_columns().iter().map(|c| c.to_string()).collect();
        let observed = request.observed_at.unwrap_or(Bound { at: request.anchor, inclusive: true });
        let engine = self.pool.engine(session)?;
        let mut suppressed: BTreeMap<&'static str, u64> = BTreeMap::new();
        let mut kept: Vec<Vec<Value>> = Vec::new();
        // A table no claim has landed in registers over the injected columns alone.
        if self.store.try_schema(table)?.is_some() {
            let (sql, parameters) = keyed_sql(relation.name(), &columns, &request.subject, observed);
            let (_, rows) = engine.run(&sql, &parameters, None)?;
            let memory_tables: Vec<String> = self.memory().tables.iter().map(|t| t.name.clone()).collect();
            let evidence_at = columns.iter().position(|c| c == "evidence").expect("a claim carries evidence");
            for row in rows {
                let evidence = match &row[evidence_at] {
                    Cell::Text(t) => Some(t.as_str()),
                    _ => None,
                };
                match gate(evidence, &memory_tables, |r| self.evidence_read(&engine, session, r)) {
                    Ok(()) => kept.push(row.iter().map(Cell::to_json).collect()),
                    Err(e) => *suppressed.entry(e.identifier()).or_insert(0) += 1,
                }
            }
        }
        let touched = BTreeSet::from([table.to_string()]);
        let ceiling = self.ceiling(session, &touched, request.limit, None);
        let mut response = Response::cut(columns, kept, Some(ceiling));
        let echo = Bounds { as_of: request.as_of_ingest, valid_as_of: request.observed_at };
        if let Some(b) = echo.echo() {
            response = response.with_block("bounds", b);
        }
        response = self.restrict(&engine, session, [table], response)?;
        // Counts per identifier; no suppressed claim is named (`read.recall.suppression-count`).
        let counts: Map<String, Value> = ["MemoryEvidenceUnresolved", "MemoryEvidenceOverflow"]
            .iter()
            .map(|id| (id.to_string(), json!(suppressed.get(id).copied().unwrap_or(0))))
            .collect();
        Ok(response.with_block("recall", json!({ "suppressed": counts })))
    }
}

/// The statement reading one subject's claims covering `observed`: the exact subject, the
/// validity comparison `store.bound-time.valid-as-of` makes, and no `superseded_by`
/// filter; ordered by tier, `curated` first, then `valid_from` newest first, then
/// `claim_id`.
fn keyed_sql(relation: &str, columns: &[String], subject: &str, observed: Bound) -> (String, Bindings) {
    // mirrors: store.bound-time.valid-as-of
    // An exclusive bound asks about the instant just before it: a claim starting at the
    // bound is not yet valid, and a claim ending at it still is.
    let (from_cmp, to_cmp) = if observed.inclusive { ("<=", ">") } else { ("<", ">=") };
    let at = |c: &str| format!("TRY_CAST({} AS TIMESTAMPTZ)", ident(c));
    let sql = format!(
        "SELECT {} FROM {} WHERE {} = ? AND {} {from_cmp} ? AND ({} IS NULL OR {} {to_cmp} ?) \
         ORDER BY CASE {} WHEN 'curated' THEN 0 WHEN 'derived' THEN 1 WHEN 'researched' THEN 2 ELSE 3 END, {} DESC, {}",
        columns.iter().map(|c| ident(c)).collect::<Vec<_>>().join(", "),
        ident(relation),
        ident("subject"),
        at("valid_from"),
        ident("valid_to"),
        at("valid_to"),
        ident("tier"),
        at("valid_from"),
        ident("claim_id"),
    );
    let parameters = Bindings::positional([
        Param::Text(subject.to_string()),
        Param::Timestamp(observed.at),
        Param::Timestamp(observed.at),
    ]);
    (sql, parameters)
}

