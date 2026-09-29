//! `read.cache`: cold and warm `Face::query` latency at 1, 50 and 500 unfolded runs, and the
//! share of a warm call that session and engine setup take, the figure the session-pool
//! gate in `A-read` reads.

use super::*;
use contextful_eval::trend::{median_batch, percentile};
use std::time::Instant as Clock;

/// The table every measured read touches, landed one run at a time and never folded.
const EVENTS: &str = "bench/events";
const COUNT: &str = r#"SELECT count(*) AS n FROM "bench/events""#;
/// A statement over the relation that reads no row: its latency is the call's setup.
const EMPTY: &str = r#"SELECT count(*) AS n FROM "bench/events" WHERE false"#;

fn land_run(store: &Store, i: usize) {
    let rows = (0..4)
        .map(|k| json!({ "event_id": format!("e{i}-{k}"), "kind": "open", "body": format!("event {k} of run {i}") }))
        .map(|r| r.as_object().unwrap().clone())
        .collect();
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: format!("run-{i:04}"), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, taint: None },
        committed_at: at("2030-01-10T00:00:00Z"),
    };
    land(store, &TableDecl::named(EVENTS), &Batch { rows, types: HashMap::new() }, &ctx).unwrap();
}

/// Milliseconds of `n` timed session-and-statement calls after `warm` untimed ones; a cold
/// call opens a fresh face, a warm one reuses `shared`.
fn timed(open: &dyn Fn() -> Face, shared: &Face, authority: &AdmittedAuthority, sql: &str, cold: bool, warm: usize, n: usize) -> Vec<f64> {
    (0..warm + n)
        .filter_map(|i| {
            let own = cold.then(open);
            let face = own.as_ref().unwrap_or(shared);
            let started = Clock::now();
            let session = face.session(authority, &Request::default(), Bounds::default()).unwrap();
            std::hint::black_box(face.query(&session, sql, ReadOptions::default()).unwrap());
            let ms = started.elapsed().as_secs_f64() * 1e3;
            (i >= warm).then_some(ms)
        })
        .collect()
}

/// Cold and warm `Face::query` p50 and p95, one statement, 200 timed calls after 20
/// warm-up (`assurance.measure.timing-iterations`); the warm figure is the median of 5
/// batches (`assurance.measure.timing-batches`). Setup share is the warm p95 of a call
/// reading no row over the warm p95 of the counted statement. The ledger records warm p95
/// at 50 runs. Under `contextful-ci measure` the sizes are 1, 50 and 500 runs; elsewhere,
/// the workspace stage among them, one run with 5 calls after 1 exercises the path.
#[test]
fn session_and_statement_latency_at_one_fifty_and_five_hundred_runs() {
    let measuring = std::env::var_os(contextful_eval::record::MEASURE_DIR_VAR).is_some();
    let (sizes, warm_up, repeats, batches): (&[usize], _, _, _) = if measuring { (&[1, 50, 500], 20, 200, 5) } else { (&[1], 1, 5, 1) };
    let r = Reads::new();
    let authority = r.authority(loop_subject("agent://research-loop"), vec![read(&[EVENTS], None)]);
    let open = || Face::open(r.store.clone(), MANIFEST, pepper()).unwrap();
    let mut landed = 0;
    for &runs in sizes {
        while landed < runs {
            land_run(&r.store, landed);
            landed += 1;
        }
        let shared = open();
        let cold = timed(&open, &shared, &authority, COUNT, true, warm_up, repeats);
        let warm: Vec<Vec<f64>> = (0..batches).map(|_| timed(&open, &shared, &authority, COUNT, false, warm_up, repeats)).collect();
        let (warm_p50, warm_p95) = median_batch(&warm).unwrap();
        let setup: Vec<Vec<f64>> = (0..batches).map(|_| timed(&open, &shared, &authority, EMPTY, false, warm_up, repeats)).collect();
        let (_, setup_p95) = median_batch(&setup).unwrap();
        eprintln!(
            "read at {runs} run(s): cold p50 {:.2} ms, p95 {:.2} ms; warm p50 {warm_p50:.2} ms, p95 {warm_p95:.2} ms; setup p95 {setup_p95:.2} ms, {:.0}% of warm p95",
            percentile(&cold, 50.0),
            percentile(&cold, 95.0),
            setup_p95 / warm_p95 * 100.0,
        );
        assert!(warm_p95 > 0.0 && setup_p95 > 0.0);
        if runs == 50 {
            contextful_eval::record::emit("read-session-latency", warm_p95, repeats as u64, 0);
        }
    }
}
