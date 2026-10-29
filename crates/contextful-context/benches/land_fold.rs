//! `run.land.session` and `store.fold.pass` at N unfolded single-row runs on one table: land
//! latency per run through one landing session, the latency of a read over every unfolded
//! run, and the bytes the pass folding them writes.
//!
//! `cargo bench -p contextful-context --bench land_fold` measures 1,000 runs, the size the
//! ledger records under `contextful-ci measure`. Each numeric argument after `--` names
//! another size, measured in order: `cargo bench -p contextful-context --bench land_fold --
//! 1000 10000` adds 10,000 runs, about an hour on a 28-core workstation. Every size prints
//! its figures; the workspace test stage builds no bench target.

// The session's precommit closure returns the store's own error type.
#![allow(clippy::result_large_err)]

use contextful_context::fold::fold;
use contextful_context::land::{Batch, LandSession, Position};
use contextful_context::read::{operator_query, ReadOptions};
use contextful_context::scan::scan;
use contextful_context::Store;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::fold::FoldOutcome;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reserve::Injection;
use contextful_core::time::Instant;
use contextful_eval::record::emit;
use contextful_eval::trend::percentile;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use std::time::Instant as Clock;

/// The unfolded-run count measured when no argument names one.
const DEFAULT_RUNS: usize = 1_000;
/// Timed reads per size, after one untimed read.
const READS: usize = 5;
/// A run-count trigger above every size, so each run stays unfolded until the measured pass.
const TABLE: &str = "[[pipeline.tables]]\nname = \"events\"\nfold_max_unfolded_runs = 1000000\n";

/// Bytes of every file under `dir`.
fn tree_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap())
        .map(|e| if e.file_type().unwrap().is_dir() { tree_bytes(&e.path()) } else { e.metadata().unwrap().len() })
        .sum()
}

fn single_row(i: usize) -> Batch {
    Batch { rows: vec![json!({ "id": i as i64, "kind": "open" }).as_object().unwrap().clone()], types: HashMap::new() }
}

fn injection(i: usize) -> Injection {
    Injection { run_id: format!("run-{i:05}"), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, authored_agent: None, taint: None }
}

/// One size's figures.
struct Figures {
    land_p50_us: f64,
    land_p95_us: f64,
    landed_bytes: u64,
    schema_reads: usize,
    read_ms: f64,
    fold_ms: f64,
    fold_bytes: u64,
}

fn measure(runs: usize) -> Figures {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), "bench").unwrap();
    let decl = TableDecl::parse_pipeline(TABLE).unwrap().remove(0);
    let start = Instant::parse("2030-01-01T00:00:00Z").unwrap();

    let mut session = LandSession::open(&store, decl.clone(), NodeId::parse("ingest-a").unwrap()).unwrap();
    let mut land_us = Vec::with_capacity(runs);
    let mut landed_bytes = 0;
    for i in 0..runs {
        let started = Clock::now();
        let landing = session.land(&single_row(i), injection(i), start.plus_secs(i as u64), &Position::default(), &|| Ok(())).unwrap();
        land_us.push(started.elapsed().as_secs_f64() * 1e6);
        assert!(landing.fold.is_none(), "run {i} fired a pass");
        landed_bytes += landing.manifest.and_then(|m| m.part_bytes).unwrap_or(0);
    }
    assert_eq!(session.unfolded(), runs);
    let schema_reads = session.schema_reads();

    let count = |expected: usize| {
        let started = Clock::now();
        let relation = scan(&store, &decl, Bounds::default()).unwrap().relation;
        let r = operator_query(&format!("WITH t AS ({relation}) SELECT count(*) AS n FROM t"), ReadOptions::default()).unwrap();
        let ms = started.elapsed().as_secs_f64() * 1e3;
        assert_eq!(r.rows[0][0].to_string().trim_matches('"'), expected.to_string(), "every landed row reads");
        ms
    };
    count(runs);
    let reads: Vec<f64> = (0..READS).map(|_| count(runs)).collect();

    let started = Clock::now();
    let outcome = fold(&store, &decl, start.plus_secs(runs as u64 + 60)).unwrap();
    let fold_ms = started.elapsed().as_secs_f64() * 1e3;
    let FoldOutcome::Folded { runs: folded, .. } = outcome else { panic!("the pass folds: {outcome:?}") };
    assert_eq!(folded, runs);
    let (chain, _) = store.chain("events").unwrap();
    let fold_bytes = tree_bytes(&store.snapshot_dir("events", &chain[0].snapshot_id).unwrap());

    Figures {
        land_p50_us: percentile(&land_us, 50.0),
        land_p95_us: percentile(&land_us, 95.0),
        landed_bytes,
        schema_reads,
        read_ms: percentile(&reads, 50.0),
        fold_ms,
        fold_bytes,
    }
}

fn main() {
    // `cargo bench` passes `--bench` ahead of the caller's arguments; only sizes count.
    let mut sizes: Vec<usize> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    if sizes.is_empty() {
        sizes.push(DEFAULT_RUNS);
    }
    for runs in sizes {
        let f = measure(runs);
        println!(
            "land-fold at {runs} unfolded runs: land p50 {:.0} µs, p95 {:.0} µs; schema.json parsed {}×; read p50 {:.1} ms; fold {:.0} ms, {} B written for {} B landed",
            f.land_p50_us, f.land_p95_us, f.schema_reads, f.read_ms, f.fold_ms, f.fold_bytes, f.landed_bytes
        );
        assert_eq!(f.schema_reads, 1, "one session parses schema.json once while its version holds");
        // The ledger records the default size alone.
        if runs == DEFAULT_RUNS {
            emit("land-session-p50-1k", f.land_p50_us, runs as u64, 0);
            emit("land-session-p95-1k", f.land_p95_us, runs as u64, 0);
            emit("land-session-schema-reads-1k", f.schema_reads as f64, runs as u64, 0);
            emit("read-unfolded-p50-1k", f.read_ms, READS as u64, 0);
            emit("fold-unfolded-bytes-1k", f.fold_bytes as f64, runs as u64, 0);
        }
    }
}
