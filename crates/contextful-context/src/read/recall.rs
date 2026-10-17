//! `memory.recall`: one subject's claims at an observed instant, under an ingest bound,
//! through the evidence gate.

use super::engine::SqlEngine;
use super::face::Face;
use super::evidence::ReadableRows;
use super::fault::ReadFault;
use contextful_core::enforce::EnforceError;
use contextful_core::memory::calibrate::{Calibration, Confidence, Settled};
use contextful_core::memory::declare::Shape;
use contextful_core::memory::recall::{admit, parse_evidence, withheld, Admission, EvidenceRead, Grounding};
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
    /// Further exact subjects a claim may carry instead: a knowledge card's canonical name
    /// beside its `entity_id` (`read.resolve-entity.card-claims`). Empty for `memory.recall`.
    pub also_subjects: Vec<String>,
    /// The valid-time instant the claims cover; `None` reads at `anchor`.
    pub observed_at: Option<Bound>,
    /// The transaction-time bound; `None` reads the latest committed state.
    pub as_of_ingest: Option<Bound>,
    /// The availability bound a credential's horizon imposes (`read.horizon.recall-horizon`).
    pub available_as_of: Option<Bound>,
    pub limit: Option<u64>,
    pub max_duration_ms: Option<u64>,
    pub max_response_bytes: Option<u64>,
    /// The instant the call is made at.
    pub anchor: Instant,
    /// Return the internals object (`read.respond.internals-opt-in`).
    pub internals: bool,
}

impl RecallRequest {
    /// A keyed recall of `subject` in `table`, asked at `anchor`, with every option unset.
    pub fn new(table: impl Into<String>, subject: impl Into<String>, anchor: Instant) -> RecallRequest {
        RecallRequest { table: table.into(), subject: subject.into(), also_subjects: Vec::new(), observed_at: None, as_of_ingest: None, available_as_of: None, limit: None, max_duration_ms: None, max_response_bytes: None, anchor, internals: false }
    }

    /// Every exact subject a returned claim may carry: `subject`, then `also_subjects`,
    /// each once.
    fn subjects(&self) -> Vec<&str> {
        let mut subjects = vec![self.subject.as_str()];
        for s in &self.also_subjects {
            if !subjects.contains(&s.as_str()) {
                subjects.push(s);
            }
        }
        subjects
    }

    /// The bounds the session serving this recall opens under: `as_of_ingest` as its
    /// `as_of` (`read.recall.keyed-clocks`) beside any availability bound. Valid time
    /// narrows inside the recall itself.
    pub fn bounds(&self) -> Bounds {
        Bounds { as_of: self.as_of_ingest, valid_as_of: None, available_as_of: self.available_as_of, ..Bounds::default() }
    }

    /// The `contextful.bounds` echo: each supplied bound under its argument name, written
    /// as `store.bound-time.echo` writes an instant, beside an `inclusive` map keyed the
    /// same way (`read.recall.keyed-clocks`).
    pub fn echo(&self) -> Option<Value> {
        let supplied: Vec<(&str, Bound)> = [("as_of_ingest", self.as_of_ingest), ("observed_at", self.observed_at), ("available_as_of", self.available_as_of)]
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
        self.recall_read(session, request).map(|(response, _)| response)
    }

    /// [`Face::recall`] beside every table the recall read under the session, in first-read
    /// order: the claims table, each evidence table the gate read, and a calibration's
    /// labels table. The read entry names them; the response never does
    /// (`disclosure.attest.lineage-elision`).
    pub fn recall_read(&self, session: &Session, request: &RecallRequest) -> Result<(Response, Vec<String>), ReadFault> {
        let started = std::time::Instant::now();
        let executed: std::cell::RefCell<Vec<String>> = Default::default();
        let table = request.table.as_str();
        let read_tables = std::cell::RefCell::new(vec![table.to_string()]);
        let note = |t: &str| {
            let mut read = read_tables.borrow_mut();
            if session.relation(t).is_some() && !read.iter().any(|r| r == t) {
                read.push(t.to_string());
            }
        };
        let relation = session.relation(table).ok_or_else(|| EnforceError::UnknownRelation(format!("`{table}`")))?;
        self.bind_bounds(session, &std::collections::BTreeSet::from([table.to_string()]), request.bounds())?;
        if self.memory().table(table).is_none_or(|t| t.shape != Shape::Facts) {
            let why = format!("`{table}` declares no `{}` shape, so it holds no claim to recall", Shape::Facts.name());
            return Err(Refusal::from(MemoryError::RecallNotClaims(why)).into());
        }
        let columns: Vec<String> = Shape::Facts.canonical_columns().iter().map(|c| c.to_string()).collect();
        let observed = request.observed_at.unwrap_or(Bound { at: request.anchor, inclusive: true });
        let engine = self.pool.engine(session, self.store.parquet_key())?;
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
                let (sql, parameters) = keyed_sql(relation.name(), &columns, &request.subjects(), observed, page, offset);
                let (_, rows) = match deadline {
                    Some((ms, source)) => engine.run_timed(&sql, &parameters, None, ms, source)?,
                    None => engine.run(&sql, &parameters, None)?,
                };
                executed.borrow_mut().push(sql);
                let read = rows.len() as u64;
                for row in rows {
                    if full(&kept) {
                        break;
                    }
                    let evidence = match &row[evidence_at] {
                        Cell::Text(t) => Some(t.as_str()),
                        _ => None,
                    };
                    // The claim's citations resolve one statement per cited table and run
                    // (`read.recall.keyed-evidence-batch`), on the first citation the gate
                    // reaches there, so a claim the gate stops early runs none past it.
                    let refs = parse_evidence(evidence).unwrap_or_default();
                    let readable: std::cell::RefCell<ReadableRows> = Default::default();
                    let row_read = |r: &EvidenceRef| -> Result<Option<bool>, ReadFault> {
                        let key = (r.table.clone(), r.run.clone());
                        if !readable.borrow().contains_key(&key) {
                            let seqs: BTreeSet<i64> = refs.iter().filter(|c| c.table == r.table && c.run == r.run).map(|c| c.seq).collect();
                            let Some(found) = self.readable_run(&engine, session, &r.table, &r.run, &seqs, &touched, request.max_duration_ms, &mut executed.borrow_mut())? else { return Ok(None) };
                            readable.borrow_mut().insert(key.clone(), found);
                        }
                        Ok(readable.borrow().get(&key).map(|seqs| seqs.contains(&r.seq)))
                    };
                    let fault = std::cell::RefCell::new(None);
                    let admitted = tally.gate(evidence, &memory_tables, session, |r| {
                        note(&r.table);
                        match row_read(r).and_then(|given| self.evidence_read_given(&engine, session, r, &touched, request.max_duration_ms, given, &mut executed.borrow_mut())) {
                            Ok(read) => read,
                            Err(error) => {
                                *fault.borrow_mut() = Some(error);
                                EvidenceRead::Unreadable
                            }
                        }
                    });
                    if let Some(error) = fault.into_inner() {
                        return Err(error);
                    }
                    if let Some(admission) = admitted {
                        let mut served: Vec<Value> = row.iter().map(Cell::to_json).collect();
                        if let Some(capped) = served_evidence(&admission) {
                            served[evidence_at] = capped;
                        }
                        served.push(json!(admission.omitted));
                        kept.push(served);
                    }
                }
                offset += page;
                if read < page || full(&kept) {
                    break;
                }
            }
        }
        // Each claim's confidence is reported through the table's validated calibration
        // (`read.recall.confidence-label`).
        let calibration = self.calibration(&engine, session, table, deadline, &note)?;
        let (confidence_at, predicate_at) = (position(&columns, "confidence"), position(&columns, "predicate"));
        for row in &mut kept {
            let emitted = row[confidence_at].as_f64();
            let reported = emitted.map(|c| calibration.confidence(Shape::Facts, row[predicate_at].as_str().unwrap_or_default(), c));
            if let Some(Confidence::Calibrated(v)) = reported {
                row[confidence_at] = json!(v);
            }
            row.push(json!(reported.map_or("uncalibrated", Confidence::label)));
        }
        let mut columns = columns;
        columns.push(EVIDENCE_OMITTED.to_string());
        columns.push(CONFIDENCE_LABEL.to_string());
        let mut response = Response::cut(columns, kept, Some(ceiling));
        if let Some(b) = request.echo() {
            response = response.with_block("bounds", b);
        }
        response = self.restrict_timed(&engine, session, [table], response, deadline)?;
        let response = self.finish_budget(session, &touched, ReadOptions { limit: request.limit, max_duration_ms: request.max_duration_ms, max_response_bytes: request.max_response_bytes, ..ReadOptions::default() }, None, ceiling, response.with_block("recall", tally.block()))?;
        let response = if request.internals {
            let rows = response.rows.len() as u64;
            response.with_block("internals", super::face::internals_block(&executed.borrow(), Some(ceiling), rows, started))
        } else {
            response
        };
        Ok((response, read_tables.into_inner()))
    }
}

/// The column counting a served claim's evidence entries past the served cap
/// (`read.recall.evidence-references`), projected null outside memory on the ranked arm
/// (`read.recall.evidence-omitted-column`).
pub const EVIDENCE_OMITTED: &str = "evidence_omitted";

/// A capped claim's served evidence list, as stored text; `None` where nothing was
/// omitted and the stored list serves as written (`read.recall.evidence-references`).
pub(crate) fn served_evidence(admission: &Admission) -> Option<Value> {
    (admission.omitted > 0).then(|| json!(serde_json::to_string(&admission.served).expect("evidence references serialize")))
}

/// The keyed response's column labelling each claim's confidence
/// (`read.recall.confidence-label`).
pub const CONFIDENCE_LABEL: &str = "confidence_label";

fn position(columns: &[String], name: &str) -> usize {
    columns.iter().position(|c| c == name).expect("a claim carries its canonical columns")
}

impl Face {
    /// The validated calibration of `table`'s claims: its declared `labels` table joined on
    /// `prediction_id` = `claim_id`, read through `session` in settlement order. A table
    /// declaring none, or one the session does not register, fits nothing
    /// (`read.recall.confidence-label`).
    fn calibration(&self, engine: &SqlEngine, session: &Session, table: &str, deadline: Option<(u64, &'static str)>, read: &dyn Fn(&str)) -> Result<Calibration, ReadFault> {
        let Some(labels) = self.memory().table(table).and_then(|t| t.labels.as_deref()) else { return Ok(Calibration::default()) };
        let (Some(facts), Some(outcomes)) = (session.relation(table), session.relation(labels)) else { return Ok(Calibration::default()) };
        if self.store.try_schema(labels)?.is_none() || self.store.try_schema(table)?.is_none() {
            return Ok(Calibration::default());
        }
        read(labels);
        let sql = format!(
            "SELECT f.{predicate}, CAST(f.{confidence} AS DOUBLE), CAST(o.{verdict} AS BOOLEAN) FROM {facts} f JOIN {outcomes} o ON o.{prediction} = f.{claim} \
             WHERE o.{verdict} IS NOT NULL ORDER BY {settled}o.{prediction}",
            predicate = ident("predicate"),
            confidence = ident("confidence"),
            verdict = ident("verdict"),
            prediction = ident("prediction_id"),
            claim = ident("claim_id"),
            // A horizon withholds the load stamp; settlement order then falls to the label id
            // (`read.horizon.engine-stamps`).
            settled = match session.columns(labels).is_some_and(|c| c.iter().any(|c| c.name == contextful_core::store::reserve::INGESTED_AT)) {
                true => format!("o.{}, ", ident(contextful_core::store::reserve::INGESTED_AT)),
                false => String::new(),
            },
            facts = ident(facts.name()),
            outcomes = ident(outcomes.name()),
        );
        let (_, rows) = match deadline {
            Some((ms, source)) => engine.run_timed(&sql, &Bindings::default(), None, ms, source)?,
            None => engine.run(&sql, &Bindings::default(), None)?,
        };
        let samples: Vec<Settled> = rows
            .into_iter()
            .filter_map(|row| match (row[0].to_json(), row[1].to_json(), row[2].to_json()) {
                (Value::String(predicate), Value::Number(c), Value::Bool(held)) => Some(Settled { shape: Shape::Facts, predicate, confidence: c.as_f64()?, held }),
                _ => None,
            })
            .collect();
        Ok(Calibration::fit(&samples))
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
    /// not register; the admission where the claim is served.
    pub(crate) fn gate(&mut self, evidence: Option<&str>, memory_tables: &[String], session: &Session, read: impl Fn(&EvidenceRef) -> EvidenceRead) -> Option<Admission> {
        self.withheld += withheld(evidence, |t| session.relation(t).is_some());
        self.admit(admit(evidence, memory_tables, read))
    }

    /// Count one gate outcome; the admission where the claim is served.
    fn admit(&mut self, outcome: Result<Admission, MemoryError>) -> Option<Admission> {
        match outcome {
            Ok(admission) => {
                self.stale += u64::from(admission.grounding == Grounding::Stale);
                Some(admission)
            }
            Err(e) => {
                *self.suppressed.entry(e.identifier()).or_insert(0) += 1;
                None
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
fn keyed_sql(relation: &str, columns: &[String], subjects: &[&str], observed: Bound, limit: u64, offset: u64) -> (String, Bindings) {
    // mirrors: store.bound-time.valid-as-of
    // An exclusive bound asks about the instant just before it: a claim starting at the
    // bound is not yet valid, and a claim ending at it still is.
    let (from_cmp, to_cmp) = if observed.inclusive { ("<=", ">") } else { ("<", ">=") };
    let at = |c: &str| format!("TRY_CAST({} AS TIMESTAMPTZ)", ident(c));
    // The highest standing ranks 0; a tier name the enum does not hold ranks last.
    let ranks: String = Tier::ALL.iter().rev().enumerate().map(|(i, t)| format!("WHEN '{}' THEN {i} ", t.name())).collect();
    let sql = format!(
        "SELECT {} FROM {} WHERE {} IN ({}) AND {} {from_cmp} ? AND ({} IS NULL OR {} {to_cmp} ?) \
         ORDER BY CASE {} {ranks}ELSE {} END, {} DESC, {} LIMIT {limit} OFFSET {offset}",
        columns.iter().map(|c| ident(c)).collect::<Vec<_>>().join(", "),
        ident(relation),
        ident("subject"),
        vec!["?"; subjects.len()].join(", "),
        at("valid_from"),
        ident("valid_to"),
        at("valid_to"),
        ident("tier"),
        Tier::ALL.len(),
        at("valid_from"),
        ident("claim_id"),
    );
    let parameters = Bindings::positional(
        subjects.iter().map(|s| Param::Text(s.to_string())).chain([Param::Timestamp(observed.at), Param::Timestamp(observed.at)]),
    );
    (sql, parameters)
}
