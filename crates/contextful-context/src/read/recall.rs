//! `memory.recall`: one subject's claims at an observed instant, under an ingest bound,
//! through the evidence gate.

use super::face::Face;
use super::fault::ReadFault;
use contextful_core::enforce::EnforceError;
use contextful_core::memory::declare::Shape;
use contextful_core::memory::recall::{gate, withheld, EvidenceRead, Grounding};
use contextful_core::memory::synthesize::EvidenceRef;
use contextful_core::memory::revise::Tier;
use contextful_core::memory::MemoryError;
use contextful_core::read::respond::{Cell, Response};
use super::face::ReadOptions;
use contextful_core::read::Refusal;
use contextful_core::read::template::{Bindings, Bound as Param};
use contextful_core::store::bound_time::{Bound, Bounds};
use contextful_core::store::relation::ident;
use contextful_core::time::Instant;
use contextful_policy::enforce::session::Session;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Claims read per page when no row ceiling bounds the recall.
const UNBOUNDED_PAGE: u64 = 256;

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
    pub max_duration_ms: Option<u64>,
    pub max_response_bytes: Option<u64>,
    /// The instant the call is made at.
    pub anchor: Instant,
}

impl RecallRequest {
    /// A keyed recall of `subject` in `table`, asked at `anchor`, with every option unset.
    pub fn new(table: impl Into<String>, subject: impl Into<String>, anchor: Instant) -> RecallRequest {
        RecallRequest { table: table.into(), subject: subject.into(), observed_at: None, as_of_ingest: None, limit: None, max_duration_ms: None, max_response_bytes: None, anchor }
    }

    /// The bounds the session serving this recall opens under: `as_of_ingest` as its
    /// `as_of` (`read.recall.keyed-clocks`). Valid time narrows inside the recall itself.
    pub fn bounds(&self) -> Bounds {
        Bounds { as_of: self.as_of_ingest, valid_as_of: None }
    }

    /// The `contextful.bounds` echo: each supplied bound under its argument name, written
    /// as `store.bound-time.echo` writes an instant, beside an `inclusive` map keyed the
    /// same way (`read.recall.keyed-clocks`).
    pub fn echo(&self) -> Option<Value> {
        let supplied: Vec<(&str, Bound)> = [("as_of_ingest", self.as_of_ingest), ("observed_at", self.observed_at)]
            .into_iter()
            .filter_map(|(k, b)| b.map(|b| (k, b)))
            .collect();
        if supplied.is_empty() {
            return None;
        }
        let mut echo = Map::new();
        let mut inclusive = Map::new();
        for (k, b) in supplied {
            echo.insert(k.to_string(), json!(b.at.to_rfc3339_nanos()));
            inclusive.insert(k.to_string(), json!(b.inclusive));
        }
        echo.insert("inclusive".to_string(), Value::Object(inclusive));
        Some(Value::Object(echo))
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
        let touched = BTreeSet::from([table.to_string()]);
        let ceiling = self.ceiling(session, &touched, request.limit, None);
        let deadline = self.duration_budget(session, &touched, request.max_duration_ms);
        // One claim past the ceiling marks the response truncated; gating stops there
        // (`read.recall.keyed-window`).
        let wanted = Response::fetch_count(Some(ceiling));
        let page = wanted.unwrap_or(UNBOUNDED_PAGE);
        let mut tally = RecallTally::default();
        let mut kept: Vec<Vec<Value>> = Vec::new();
        let full = |kept: &Vec<Vec<Value>>| wanted.is_some_and(|w| kept.len() as u64 >= w);
        // A table no claim has landed in registers over the injected columns alone.
        if self.store.try_schema(table)?.is_some() {
            let memory_tables: Vec<String> = self.memory().tables.iter().map(|t| t.name.clone()).collect();
            let evidence_at = columns.iter().position(|c| c == "evidence").expect("a claim carries evidence");
            let mut offset = 0u64;
            loop {
                let (sql, parameters) = keyed_sql(relation.name(), &columns, &request.subject, observed, page, offset);
                let (_, rows) = match deadline {
                    Some((ms, source)) => engine.run_timed(&sql, &parameters, None, ms, source)?,
                    None => engine.run(&sql, &parameters, None)?,
                };
                let read = rows.len() as u64;
                for row in rows {
                    if full(&kept) {
                        break;
                    }
                    let evidence = match &row[evidence_at] {
                        Cell::Text(t) => Some(t.as_str()),
                        _ => None,
                    };
                    let fault = std::cell::RefCell::new(None);
                    let admitted = tally.gate(evidence, &memory_tables, session, |r| match self.evidence_read(&engine, session, r, &touched, request.max_duration_ms) {
                        Ok(read) => read,
                        Err(error) => {
                            *fault.borrow_mut() = Some(error);
                            EvidenceRead::Unreadable
                        }
                    });
                    if let Some(error) = fault.into_inner() {
                        return Err(error);
                    }
                    if admitted {
                        kept.push(row.iter().map(Cell::to_json).collect());
                    }
                }
                offset += page;
                if read < page || full(&kept) {
                    break;
                }
            }
        }
        let mut response = Response::cut(columns, kept, Some(ceiling));
        if let Some(b) = request.echo() {
            response = response.with_block("bounds", b);
        }
        response = self.restrict_timed(&engine, session, [table], response, deadline)?;
        self.finish_budget(session, &touched, ReadOptions { limit: request.limit, max_duration_ms: request.max_duration_ms, max_response_bytes: request.max_response_bytes, ..ReadOptions::default() }, None, ceiling, response.with_block("recall", tally.block()))
    }
}

/// What the evidence gate counted over one read: suppressions per error identifier, and
/// claims served through a key whose cited version no longer reads.
#[derive(Debug, Default)]
pub(crate) struct RecallTally {
    suppressed: BTreeMap<&'static str, u64>,
    stale: u64,
    withheld: u64,
}

impl RecallTally {
    /// Gate one claim through `read`, counting its references into tables `session` does
    /// not register; true where the claim is served.
    pub(crate) fn gate(&mut self, evidence: Option<&str>, memory_tables: &[String], session: &Session, read: impl Fn(&EvidenceRef) -> EvidenceRead) -> bool {
        self.withheld += withheld(evidence, |t| session.relation(t).is_some());
        self.admit(gate(evidence, memory_tables, read))
    }

    /// Count one gate outcome; true where the claim is served.
    fn admit(&mut self, outcome: Result<Grounding, MemoryError>) -> bool {
        match outcome {
            Ok(grounding) => {
                self.stale += u64::from(grounding == Grounding::Stale);
                true
            }
            Err(e) => {
                *self.suppressed.entry(e.identifier()).or_insert(0) += 1;
                false
            }
        }
    }

    /// The `contextful.recall` block: counts per identifier (`read.recall.suppression-count`),
    /// the stale count (`read.recall.evidence-stale`) and the withheld-evidence count
    /// (`disclosure.attest.lineage-elision`); no claim and no withheld table is named.
    pub(crate) fn block(&self) -> Value {
        let counts: Map<String, Value> = ["MemoryEvidenceUnresolved", "MemoryEvidenceOverflow"]
            .iter()
            .map(|id| (id.to_string(), json!(self.suppressed.get(id).copied().unwrap_or(0))))
            .collect();
        json!({ "suppressed": counts, "stale": self.stale, "withheld": self.withheld })
    }
}

/// The statement reading one subject's claims covering `observed`: the exact subject, the
/// validity comparison `store.bound-time.valid-as-of` makes, and no `superseded_by`
/// filter; ordered by tier, `curated` first, then `valid_from` newest first, then
/// `claim_id`.
fn keyed_sql(relation: &str, columns: &[String], subject: &str, observed: Bound, limit: u64, offset: u64) -> (String, Bindings) {
    // mirrors: store.bound-time.valid-as-of
    // An exclusive bound asks about the instant just before it: a claim starting at the
    // bound is not yet valid, and a claim ending at it still is.
    let (from_cmp, to_cmp) = if observed.inclusive { ("<=", ">") } else { ("<", ">=") };
    let at = |c: &str| format!("TRY_CAST({} AS TIMESTAMPTZ)", ident(c));
    // The highest standing ranks 0; a tier name the enum does not hold ranks last.
    let ranks: String = Tier::ALL.iter().rev().enumerate().map(|(i, t)| format!("WHEN '{}' THEN {i} ", t.name())).collect();
    let sql = format!(
        "SELECT {} FROM {} WHERE {} = ? AND {} {from_cmp} ? AND ({} IS NULL OR {} {to_cmp} ?) \
         ORDER BY CASE {} {ranks}ELSE {} END, {} DESC, {} LIMIT {limit} OFFSET {offset}",
        columns.iter().map(|c| ident(c)).collect::<Vec<_>>().join(", "),
        ident(relation),
        ident("subject"),
        at("valid_from"),
        ident("valid_to"),
        at("valid_to"),
        ident("tier"),
        Tier::ALL.len(),
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
