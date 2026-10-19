//! `read.horizon.rank-statistics` and `read.retrieve`: ranked reads under past horizons
//! over a dated, full-text-indexed table, and an unbounded ranked read's latency.
#![cfg(feature = "fts")]

use super::*;
use contextful_context::fold::fold;
use contextful_context::read::RetrieveRequest;
use contextful_core::store::bound_time::Bound;
use contextful_core::store::reconcile::ColumnType;
use contextful_eval::record::{emit, MEASURE_DIR_VAR};
use contextful_eval::trend::median_batch;
use std::collections::BTreeMap;
use std::time::Instant as Clock;

const SEED: u64 = 0x5eed_0601;
const NEWS: &str = "lab/news";
/// Rows from the horizon back past the 200-row recency window to a horizon's needle.
const NEEDLE_DEPTH: usize = 500;
/// The first row a horizon may sit on, so its needle exists.
const FIRST_HORIZON: usize = 600;

fn measuring() -> bool {
    std::env::var_os(MEASURE_DIR_VAR).is_some()
}

/// The instant row `i` publishes at: one row a day from 2000-01-01.
fn day(i: usize) -> String {
    at("2000-01-01T00:00:00Z").plus_secs(i as u64 * 86_400).to_rfc3339()
}

/// The word only horizon `k`'s needle holds: letters alone, so no plural or number splits it.
fn needle(k: usize) -> String {
    const L: &[u8] = b"bcdfghjklm";
    format!("zephyr{}{}", L[k / 10 % 10] as char, L[k % 10] as char)
}

/// The dated rows: `needles` maps a row to its needle word. `margin` sits on one row in 97
/// before row `late` and on every second row from it; `widens` sits on one row in 7
/// throughout, so a horizon before `late` sees `margin` rare and the whole table sees it
/// frequent.
struct Dated {
    rows: usize,
    late: usize,
    needles: BTreeMap<usize, String>,
}

impl Dated {
    fn body(&self, i: usize) -> String {
        match self.needles.get(&i) {
            Some(word) => format!("quarterly filing mentions {word}"),
            None if i.is_multiple_of(7) => "acme widens guidance".to_string(),
            None if (i >= self.late && i.is_multiple_of(2)) || i.is_multiple_of(97) => "acme margin report".to_string(),
            None => format!("routine story {i}"),
        }
    }

    /// Land rows `0..upto` as one run and fold them, then open the face.
    fn land(&self, upto: usize) -> Reads {
        let manifest = format!("{MANIFEST}{DATED_TEXT}");
        let mut r = Reads::with_manifest(&manifest);
        let decl = TableDecl::parse_pipeline(&manifest).unwrap().into_iter().find(|d| d.name == NEWS).unwrap();
        let rows = (0..upto)
            .map(|i| {
                json!({ "story_id": format!("s{i:05}"), "digest": format!("d{i}"), "body": self.body(i), "published_at": day(i) })
                    .as_object()
                    .unwrap()
                    .clone()
            })
            .collect();
        let ctx = RunContext {
            node: NodeId::parse("ingest-a").unwrap(),
            injection: Injection { run_id: "run-0001".into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, authored_agent: None, taint: None },
            committed_at: at("2030-01-10T00:00:00Z"),
        };
        let types = [("published_at".to_string(), ColumnType::Timestamp)].into_iter().collect();
        land(&r.store, &decl, &Batch { rows, types }, &ctx).unwrap();
        fold(&r.store, &decl, at("2030-01-11T00:00:00Z")).unwrap();
        r.face = Face::open(r.store.clone(), &manifest, pepper()).unwrap();
        r
    }
}

fn under(horizon: &str) -> Bounds {
    Bounds { available_as_of: Some(Bound::parse(horizon).unwrap()), ..Bounds::default() }
}

/// A session under `horizon`, opened by a grant carrying it, and the bounds it reads under.
fn session_under(r: &Reads, horizon: Option<&str>) -> (Session, Bounds) {
    let grant = Grant { horizon: horizon.map(at), ..read(&["lab/*"], None) };
    let bounds = horizon.map(under).unwrap_or_default();
    (r.face.session(&r.authority(loop_subject("agent://research-loop"), vec![grant]), &Request::default(), bounds).unwrap(), bounds)
}

/// A top-10 ranked read of `query` on `session`, asked at the horizon or after every row.
fn rank_on(r: &Reads, session: &Session, bounds: Bounds, query: &str, horizon: Option<&str>) -> Result<Response, ReadFault> {
    let anchor = at(horizon.unwrap_or("2030-02-01T00:00:00Z"));
    let request = RetrieveRequest { limit: Some(10), ..RetrieveRequest::new(NEWS, query, anchor) };
    r.face.retrieve(session, &request, bounds)
}

/// A top-10 ranked read of `query` under `horizon`, through a session the horizon's grant opens.
fn ranked(r: &Reads, query: &str, horizon: Option<&str>) -> Result<Response, ReadFault> {
    let (session, bounds) = session_under(r, horizon);
    rank_on(r, &session, bounds, query, horizon)
}

fn story_ids(r: &Response) -> Vec<String> {
    column(r, "_row").iter().map(|row| row["story_id"].as_str().unwrap().to_string()).collect()
}

/// The ranked identifiers and their scores, in rank order.
fn ranking(r: &Response) -> Vec<(String, Value)> {
    story_ids(r).into_iter().zip(column(r, "_score")).collect()
}

/// Whether `result` answered, refusing only as `ReadIndexBeyondHorizon` otherwise.
fn answered(result: Result<Response, ReadFault>, horizon: &str) -> Option<Response> {
    match result {
        Ok(response) => Some(response),
        Err(ReadFault::Refused(refusal)) if refusal.identifier() == "ReadIndexBeyondHorizon" => None,
        Err(other) => panic!("horizon {horizon}: {other:?}"),
    }
}

/// Ranked reads under past horizons over a table of one row a day: the share of horizons
/// answered (ledger `horizon-answered`), the share whose top 10 hold the needle 500 rows
/// behind the horizon, past the 200-row window (`horizon-deep-recall`), and the share whose
/// ranking equals the one a store holding only the rows available by then returns
/// (`horizon-rank-equivalence`). Under `contextful-ci measure` the table holds 4,000 rows
/// under 100 horizons and 20 compared stores; elsewhere 1,000 rows, 20 horizons and 3 stores.
#[test]
fn a_hundred_past_horizons_over_4000_dated_rows_rank_through_the_index() {
    let (rows, horizons, compared) = if measuring() { (4_000, 100, 20) } else { (1_000, 20, 3) };
    let late = rows * 4 / 5;
    let step = (rows - 1 - FIRST_HORIZON) / horizons;
    let horizon_rows: Vec<usize> = (0..horizons).map(|k| FIRST_HORIZON + k * step).collect();
    let needles = horizon_rows.iter().enumerate().map(|(k, h)| (h - NEEDLE_DEPTH, needle(k))).collect();
    let dated = Dated { rows, late, needles };
    let r = dated.land(dated.rows);

    // The index serves every needle without a horizon and under one past every row.
    let last = day(rows - 1);
    for horizon in [None, Some(last.as_str())] {
        let found = story_ids(&ranked(&r, &needle(0), horizon).unwrap());
        assert_eq!(found.first().map(String::as_str), Some(format!("s{:05}", horizon_rows[0] - NEEDLE_DEPTH).as_str()), "{horizon:?}");
    }

    let (mut answers, mut recalled) = (0, 0);
    for (k, &h) in horizon_rows.iter().enumerate() {
        let horizon = day(h);
        let Some(response) = answered(ranked(&r, &needle(k), Some(&horizon)), &horizon) else { continue };
        answers += 1;
        if story_ids(&response).contains(&format!("s{:05}", h - NEEDLE_DEPTH)) {
            recalled += 1;
        }
    }

    let mut equal = 0;
    for j in 0..compared {
        let h = FIRST_HORIZON + j * (late - FIRST_HORIZON) / compared;
        let horizon = day(h);
        // A store holding only rows up to the horizon answers under it.
        let bounded = dated.land(h + 1);
        let reference = ranking(&ranked(&bounded, "widens margin", Some(&horizon)).unwrap());
        assert!(!reference.is_empty(), "horizon {horizon}");
        if answered(ranked(&r, "widens margin", Some(&horizon)), &horizon).is_some_and(|whole| ranking(&whole) == reference) {
            equal += 1;
        }
    }

    let share = |n: usize, of: usize| n as f64 / of as f64;
    eprintln!("{horizons} horizons: {answers} answered, {recalled} recalled; {equal} of {compared} rankings equal the bounded store's");
    emit("horizon-answered", share(answers, horizons), horizons as u64, SEED);
    emit("horizon-deep-recall", share(recalled, horizons), horizons as u64, SEED);
    emit("horizon-rank-equivalence", share(equal, compared), compared as u64, SEED);
}

/// Warm p95 of an unbounded ranked read over 10,000 dated rows, the median of 5 batches of
/// 200 reads after 20 warm-up (ledger `unbounded-retrieve-latency`). Elsewhere 1,000 rows
/// and 5 reads after 1 exercise the path.
#[test]
fn retrieve_latency_at_10k_chunks() {
    let (rows, warm, repeats, batches) = if measuring() { (10_000, 20, 200, 5) } else { (1_000, 1, 5, 1) };
    let needles = [(rows / 2, needle(0))].into_iter().collect();
    let dated = Dated { rows, late: rows * 4 / 5, needles };
    let r = dated.land(rows);
    let target = format!("s{:05}", rows / 2);
    let (session, bounds) = session_under(&r, None);
    let timed: Vec<Vec<f64>> = (0..batches)
        .map(|_| {
            (0..warm + repeats)
                .filter_map(|i| {
                    let started = Clock::now();
                    let found = story_ids(&rank_on(&r, &session, bounds, &needle(0), None).unwrap());
                    let ms = started.elapsed().as_secs_f64() * 1e3;
                    assert_eq!(found.first(), Some(&target));
                    (i >= warm).then_some(ms)
                })
                .collect()
        })
        .collect();
    let (p50, p95) = median_batch(&timed).unwrap();
    eprintln!("unbounded retrieve over {rows} rows: p50 {p50:.2} ms, p95 {p95:.2} ms");
    assert!(p95 > 0.0);
    emit("unbounded-retrieve-latency", p95, repeats as u64, 0);
}
