//! Settlement over the store: a prediction registration lands in a `predictions` table, an
//! observation of it in an `outcomes` table, and the label view joins the two through the
//! reader's session (`read.settle`).

use super::claims::{objects, require, Boundary, Landing, Writer};
use super::MemoryFault;
use contextful_context::read::{Face, ReadOptions};
use contextful_core::grant::Action;
use contextful_core::memory::calibrate::{self, GroupReport, Settled};
use contextful_core::memory::declare::Shape;
use contextful_core::memory::settle::{label_join, observe as check_observation, Observation, RawRegistration, Registration, LABEL_VIEW_NAME};
use contextful_core::memory::MemoryError;
use contextful_core::read::respond::Response;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reconcile::ColumnType;
use contextful_core::time::Instant;
use contextful_policy::enforce::session::{Request, Session};
use contextful_policy::verify::AdmittedAuthority;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// What a verb landed: whether a row landed, `false` where it restated the stored row
/// (`read.settle.restated-landing`).
#[derive(Debug, Clone, PartialEq)]
pub struct Settlement {
    pub prediction_id: String,
    pub landed: bool,
}

/// The declared table `name`, which must carry `shape`; absent, the first table of
/// `shape` the manifest declares.
pub fn shaped<'a>(face: &'a Face, name: Option<&'a str>, shape: Shape) -> Result<&'a str, MemoryFault> {
    let table = match name {
        Some(n) => face.memory().table(n).filter(|t| t.shape == shape),
        None => face.memory().of_shape(shape),
    };
    table.map(|t| t.name.as_str()).ok_or_else(|| {
        MemoryFault::Undeclared(match name {
            Some(n) => format!("`{n}` is no `{}` table the manifest declares", shape.name()),
            None => format!("the manifest declares no `{}` table", shape.name()),
        })
    })
}

fn text(row: &Map<String, Value>, k: &str) -> Option<String> {
    row.get(k).and_then(Value::as_str).map(str::to_string)
}

fn instant(row: &Map<String, Value>, k: &str) -> Option<Instant> {
    text(row, k).and_then(|s| Instant::parse(&s).ok())
}

/// The stored row of `prediction_id` in `table`, read through `session`; `None` where
/// the table holds none or has never landed a row.
fn stored(face: &Face, session: &Session, table: &str, prediction_id: &str) -> Result<Option<Map<String, Value>>, MemoryFault> {
    if face.store().try_schema(table)?.is_none() {
        return Ok(None);
    }
    let rows = face.rows(session, table, None)?;
    Ok(objects(&rows).into_iter().find(|r| text(r, "prediction_id").as_deref() == Some(prediction_id)))
}

/// A stored registration row as the registration it records.
fn registration_of(row: &Map<String, Value>) -> Option<Registration> {
    let horizon_secs = row.get("horizon_secs").and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()));
    let deadline_at = if horizon_secs.is_some() { None } else { instant(row, "deadline_at") };
    Registration::check(RawRegistration {
        prediction_id: text(row, "prediction_id")?,
        subject: text(row, "subject")?,
        predicted_at: instant(row, "predicted_at")?,
        horizon_secs,
        deadline_at,
        watch: horizon_secs.is_none() && deadline_at.is_none(),
        resolution_source: text(row, "resolution_source"),
        comparator: text(row, "comparator"),
        statement: text(row, "statement"),
        metric: text(row, "metric"),
    })
    .ok()
}

/// A stored outcome row as the observation it records.
fn observation_of(row: &Map<String, Value>) -> Option<Observation> {
    Some(Observation {
        prediction_id: text(row, "prediction_id")?,
        observed_at: instant(row, "observed_at")?,
        verdict: row.get("verdict").and_then(Value::as_bool),
        resolution_source: text(row, "resolution_source"),
        signal: text(row, "signal"),
        settling_citation: text(row, "settling_citation"),
        metric: text(row, "metric"),
    })
}

fn types(columns: &[(&str, ColumnType)]) -> HashMap<String, ColumnType> {
    columns.iter().map(|(c, t)| (c.to_string(), t.clone())).collect()
}

fn one_row(v: Value) -> Vec<Map<String, Value>> {
    vec![v.as_object().expect("a settlement row is an object").clone()]
}

/// Register one prediction into a `predictions` table (`read.settle.predict-verb`). The
/// registration checks (`read.settle.registration`); one identical to the stored row of
/// its id lands nothing (`read.settle.restated-landing`), and one differing from it
/// refuses (`read.settle.registration-conflict`). The row carries the writer's subject
/// as `_authored_by` (`read.settle.verb-attribution`).
pub fn predict(
    face: &Face,
    authority: &AdmittedAuthority,
    into: &str,
    raw: RawRegistration,
    node: &NodeId,
    now: Instant,
    boundary: &Boundary<'_>,
) -> Result<Settlement, MemoryFault> {
    let into = shaped(face, Some(into), Shape::Predictions)?;
    let registration = Registration::check(raw)?;
    require(authority, Action::Write, into)?;
    let session = face.session(authority, &Request::default(), Bounds::default())?;
    let id = registration.prediction_id.clone();
    if let Some(row) = stored(face, &session, into, &id)? {
        return match registration_of(&row) {
            Some(prior) if prior == registration => Ok(Settlement { prediction_id: id, landed: false }),
            _ => Err(MemoryError::OutcomeRegistrationConflict(format!("prediction `{id}` is already registered in `{into}` with other fields")).into()),
        };
    }
    let raw = registration.raw();
    let row = json!({
        "prediction_id": id, "subject": registration.subject, "predicted_at": registration.predicted_at.to_rfc3339_nanos(),
        "horizon_secs": raw.horizon_secs, "deadline_at": registration.deadline().map(|d| d.to_rfc3339_nanos()),
        "resolution_source": raw.resolution_source, "comparator": registration.comparator,
        "statement": registration.statement, "metric": registration.metric,
    });
    let column_types = types(&[
        ("prediction_id", ColumnType::Utf8),
        ("subject", ColumnType::Utf8),
        ("predicted_at", ColumnType::Timestamp),
        ("horizon_secs", ColumnType::Int64),
        ("deadline_at", ColumnType::Timestamp),
        ("resolution_source", ColumnType::Utf8),
        ("comparator", ColumnType::Utf8),
        ("statement", ColumnType::Utf8),
        ("metric", ColumnType::Utf8),
    ]);
    let writer = Writer::of(authority);
    let run_id = format!("memory-predict-{}", now.unix_nanos());
    Landing { node, at: now, writer: &writer, run_id, boundary, taint: None }.commit_rows(face, into, one_row(row), column_types)?;
    Ok(Settlement { prediction_id: id, landed: true })
}

/// Observe one prediction's outcome into an `outcomes` table (`read.settle.observe-verb`).
/// The registration is read from `predictions` through the writer's session, and an
/// observation naming none refuses (`read.settle.prediction-unregistered`); the
/// observation checks against it (`read.settle.source-mismatch`,
/// `read.settle.settling-citation`). An identical re-observation lands nothing, and one
/// differing from a stored verdict refuses (`read.settle.outcome-settled`).
#[allow(clippy::too_many_arguments)]
pub fn observe(
    face: &Face,
    authority: &AdmittedAuthority,
    into: &str,
    predictions: Option<&str>,
    observation: Observation,
    node: &NodeId,
    now: Instant,
    boundary: &Boundary<'_>,
) -> Result<Settlement, MemoryFault> {
    let into = shaped(face, Some(into), Shape::Outcomes)?;
    let predictions = shaped(face, predictions, Shape::Predictions)?;
    require(authority, Action::Write, into)?;
    let session = face.session(authority, &Request::default(), Bounds::default())?;
    let id = observation.prediction_id.clone();
    let registration = stored(face, &session, predictions, &id)?.as_ref().and_then(registration_of).ok_or_else(|| {
        MemoryError::OutcomePredictionUnregistered(format!("no registration of prediction `{id}` in `{predictions}` reads in the writer's session"))
    })?;
    check_observation(&registration, &observation)?;
    if let Some(prior) = stored(face, &session, into, &id)?.as_ref().and_then(observation_of) {
        if prior == observation {
            return Ok(Settlement { prediction_id: id, landed: false });
        }
        if prior.verdict.is_some() {
            return Err(MemoryError::OutcomeAlreadySettled(format!("prediction `{id}` already carries a verdict in `{into}`")).into());
        }
    }
    let row = json!({
        "prediction_id": id, "observed_at": observation.observed_at.to_rfc3339_nanos(), "verdict": observation.verdict,
        "resolution_source": observation.resolution_source, "signal": observation.signal, "settling_citation": observation.settling_citation,
        "metric": observation.metric,
    });
    let column_types = types(&[
        ("prediction_id", ColumnType::Utf8),
        ("observed_at", ColumnType::Timestamp),
        ("verdict", ColumnType::Boolean),
        ("resolution_source", ColumnType::Utf8),
        ("signal", ColumnType::Utf8),
        ("settling_citation", ColumnType::Utf8),
        ("metric", ColumnType::Utf8),
    ]);
    let writer = Writer::of(authority);
    let run_id = format!("memory-observe-{}", now.unix_nanos());
    Landing { node, at: now, writer: &writer, run_id, boundary, taint: None }.commit_rows(face, into, one_row(row), column_types)?;
    Ok(Settlement { prediction_id: id, landed: true })
}

/// The label view's rows over `predictions` and `outcomes`, read through `session`,
/// narrowed to one prediction when `prediction_id` names one (`read.settle.label-join`).
/// A table that has never landed a row yields no label.
pub fn labels(face: &Face, session: &Session, predictions: &str, outcomes: &str, prediction_id: Option<&str>) -> Result<Response, MemoryFault> {
    let columns = ["prediction_id", "subject", "resolution_source", "verdict", "signal", "settling_citation", "lead_time_s"];
    if face.store().try_schema(predictions)?.is_none() || face.store().try_schema(outcomes)?.is_none() {
        return Ok(Response::cut(columns.iter().map(|c| c.to_string()).collect(), Vec::new(), None));
    }
    let (sql, parameters) = match prediction_id {
        Some(id) => (
            label_join(predictions, outcomes, &format!("SELECT * FROM {LABEL_VIEW_NAME} WHERE prediction_id = $id")),
            Map::from_iter([("id".to_string(), json!({ "type": "string", "value": id }))]),
        ),
        None => (label_join(predictions, outcomes, &format!("SELECT * FROM {LABEL_VIEW_NAME} ORDER BY prediction_id")), Map::new()),
    };
    Ok(face.query_with(session, &sql, &parameters, ReadOptions::default())?)
}

/// What a calibration read found: the settled predictions that joined a claim, and the
/// report over them.
#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationReport {
    pub joined: usize,
    pub groups: Vec<GroupReport>,
}

/// The calibration report over `claims`: label rows carrying a verdict joined to the claim
/// whose `claim_id` is their `prediction_id`, through `session`, in `prediction_id` order
/// (`read.settle.calibration-verb`).
pub fn calibration(face: &Face, session: &Session, claims: &str, predictions: &str, outcomes: &str) -> Result<CalibrationReport, MemoryFault> {
    let claims_table = face
        .memory()
        .table(claims)
        .filter(|t| t.shape == Shape::Facts)
        .ok_or_else(|| MemoryFault::Undeclared(format!("`{claims}` is no `{}` table the manifest declares", Shape::Facts.name())))?;
    for t in [claims, predictions, outcomes] {
        if face.store().try_schema(t)?.is_none() {
            return Ok(CalibrationReport { joined: 0, groups: Vec::new() });
        }
    }
    let quoted = format!("\"{}\"", claims_table.name.replace('"', "\"\""));
    let sql = label_join(
        predictions,
        outcomes,
        &format!(
            "SELECT f.predicate, CAST(f.confidence AS DOUBLE) AS confidence, CAST(l.verdict AS BOOLEAN) AS verdict \
             FROM {LABEL_VIEW_NAME} l JOIN {quoted} f ON f.claim_id = l.prediction_id WHERE l.verdict IS NOT NULL ORDER BY l.prediction_id"
        ),
    );
    let rows = face.query(session, &sql, ReadOptions::default())?;
    let joined = rows.rows.len();
    let samples: Vec<Settled> = objects(&rows)
        .iter()
        .filter_map(|r| {
            Some(Settled {
                shape: Shape::Facts,
                predicate: text(r, "predicate")?,
                confidence: r.get("confidence").and_then(Value::as_f64)?,
                held: r.get("verdict").and_then(Value::as_bool)?,
            })
        })
        .collect();
    Ok(CalibrationReport { joined, groups: calibrate::report(&samples) })
}

/// One group's figures as JSON (`read.settle.calibration-report`).
pub fn report_json(g: &GroupReport) -> Value {
    json!({
        "shape": g.shape.name(), "predicate": g.predicate, "samples": g.samples, "brier": g.brier, "ece": g.ece,
        "held_out": g.held_out, "held_out_raw_brier": g.held_out_raw_brier, "held_out_brier": g.held_out_brier, "calibrated": g.calibrated,
    })
}
