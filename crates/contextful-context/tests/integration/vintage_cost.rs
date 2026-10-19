//! `store.available-time.vintage-scope`: what a vintage landing reads of the table's stored
//! vintages, counted by `Store::vintage_rows_read`, and what one commit costs in time and
//! memory as the table grows.
//!
//! Under `contextful-ci measure` the fixtures run at full size; elsewhere, the workspace
//! stage among them, a scaled-down fixture exercises the same path.

use crate::support::{at, decl, Fixture};
use contextful_context::land::{land, Batch, RunContext};
use contextful_context::{fold, Store};
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reconcile::ColumnType;
use contextful_core::store::reserve::Injection;
use contextful_core::time::Instant;
use contextful_eval::record::{emit, MEASURE_DIR_VAR};
use serde_json::{json, Map, Value};

/// Daily bars: one vintage per symbol, trading day and publication instant.
const BARS: &str = "name = \"bars\"\nprimary_key = [\"symbol\"]\ncontent_hash_column = \"digest\"\n[pipeline.tables.valid_time]\nfrom = \"day\"\n[pipeline.tables.available_time]\ncolumn = \"published_at\"";

/// Filings keyed by a random document id, with no valid time.
const FILINGS: &str = "name = \"filings\"\nprimary_key = [\"doc_id\"]\ncontent_hash_column = \"digest\"\n[pipeline.tables.available_time]\ncolumn = \"filed_at\"";

fn measuring() -> bool {
    std::env::var_os(MEASURE_DIR_VAR).is_some()
}

fn epoch() -> Instant {
    at("2030-01-01T00:00:00Z")
}

/// Midnight UTC of trading day `day`.
fn day_at(day: usize) -> Instant {
    epoch().plus_secs(day as u64 * 86_400)
}

/// The instant bars of `day` publish: 21:00 UTC that day, plus `late` hours for a later re-pull.
fn published(day: usize, late: u64) -> Instant {
    day_at(day).plus_secs(21 * 3_600 + late * 3_600)
}

fn bars_types() -> std::collections::HashMap<String, ColumnType> {
    [("day", ColumnType::Timestamp), ("published_at", ColumnType::Timestamp)].into_iter().map(|(c, t)| (c.to_string(), t)).collect()
}

fn filing_types() -> std::collections::HashMap<String, ColumnType> {
    [("filed_at".to_string(), ColumnType::Timestamp)].into_iter().collect()
}

fn ctx(run: &str, committed_at: Instant) -> RunContext {
    RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: run.into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, authored_agent: None, taint: None },
        committed_at,
    }
}

fn object(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

/// One bar of `symbol` on `day`, its digest `digest`, published `late` hours after 21:00.
fn bar(symbol: usize, day: usize, digest: &str, late: u64) -> Map<String, Value> {
    object(json!({
        "symbol": format!("S{symbol:04}"),
        "day": day_at(day).to_rfc3339(),
        "close": format!("{}.{:02}", 100 + symbol, day % 100),
        "digest": digest,
        "published_at": published(day, late).to_rfc3339(),
    }))
}

/// Land the bars of `day` for every symbol as one run.
fn land_bars(store: &Store, d: &TableDecl, run: &str, day: usize, rows: Vec<Map<String, Value>>) {
    land(store, d, &Batch { rows, types: bars_types() }, &ctx(run, published(day, 1))).unwrap();
}

fn day_rows(symbols: usize, day: usize) -> Vec<Map<String, Value>> {
    (0..symbols).map(|s| bar(s, day, &format!("d{day}-{s}"), 0)).collect()
}

/// A bars table holding `days` daily runs of `symbols` rows, landed one day at a time.
fn backfilled(symbols: usize, days: usize) -> (Fixture, TableDecl) {
    let f = Fixture::new();
    let d = decl(BARS);
    for day in 0..days {
        land_bars(&f.store, &d, &format!("day-{day:04}"), day, day_rows(symbols, day));
    }
    (f, d)
}

/// The rows the vintage comparison reads while `step` runs.
fn read_during(store: &Store, step: impl FnOnce()) -> u64 {
    let before = store.vintage_rows_read();
    step();
    store.vintage_rows_read() - before
}

fn stored(f: &Fixture, d: &TableDecl, column: &str) -> usize {
    contextful_context::rows::table_rows(&f.store, d, &[column]).unwrap().len()
}

/// Symbols and days of the bars fixtures: 100 × 100 under measure, 10 × 10 elsewhere.
fn bars_size() -> (usize, usize) {
    if measuring() { (100, 100) } else { (10, 10) }
}

/// A day appended to 100 symbols × 100 daily runs: the rows its vintage comparison reads
/// (ledger `vintage-append-day-scan`).
#[test]
fn a_one_day_append_reads_no_stored_vintage() {
    const SEED: u64 = 0x5eed_7101;
    let (symbols, days) = bars_size();
    let (f, d) = backfilled(symbols, days);
    let read = read_during(&f.store, || land_bars(&f.store, &d, "append", days, day_rows(symbols, days)));
    assert_eq!(stored(&f, &d, "symbol"), symbols * (days + 1));
    eprintln!("a one-day append over {} stored vintages read {read} rows", symbols * days);
    assert_eq!(read, 0, "a day past every stored valid time reads no stored vintage");
    emit("vintage-append-day-scan", read as f64, symbols as u64, SEED);
}

/// Day 50 restated for every symbol: half with a new digest, half re-pulling the stored
/// digest under a later publication instant. The rows its vintage comparison reads
/// (ledger `vintage-restate-day-scan`).
// spec: store.available-time.vintage-scope@10ef927d
#[test]
fn a_one_day_restatement_reads_only_that_days_vintages() {
    const SEED: u64 = 0x5eed_7101;
    let (symbols, days) = bars_size();
    let (f, d) = backfilled(symbols, days);
    let day = days / 2;
    let rows = (0..symbols).map(|s| if s % 2 == 0 { bar(s, day, &format!("r{day}-{s}"), 24) } else { bar(s, day, &format!("d{day}-{s}"), 24) }).collect();
    let read = read_during(&f.store, || land_bars(&f.store, &d, "restate", day, rows));
    assert_eq!(stored(&f, &d, "symbol"), symbols * days + symbols.div_ceil(2), "only a changed digest lands a vintage");
    eprintln!("a one-day restatement over {} stored vintages read {read} rows", symbols * days);
    assert!(read <= symbols as u64, "a restatement reads only its own day's {symbols} vintages, read {read}");
    emit("vintage-restate-day-scan", read as f64, symbols as u64, SEED);
}

/// 100 daily runs of 100 symbols landed into an empty table: the rows every vintage
/// comparison of the backfill reads in total (ledger `vintage-backfill-scan`).
#[test]
fn a_daily_backfill_reads_no_stored_vintage() {
    const SEED: u64 = 0x5eed_7101;
    let (symbols, days) = bars_size();
    let f = Fixture::new();
    let d = decl(BARS);
    let read = read_during(&f.store, || {
        for day in 0..days {
            land_bars(&f.store, &d, &format!("day-{day:04}"), day, day_rows(symbols, day));
        }
    });
    assert_eq!(stored(&f, &d, "symbol"), symbols * days);
    eprintln!("a backfill of {days} runs × {symbols} rows read {read} stored rows");
    assert_eq!(read, 0, "each day of a backfill lands past every stored valid time");
    emit("vintage-backfill-scan", read as f64, days as u64, SEED);
}

/// A seeded stream of 16-hex document ids.
struct Ids(u64);

impl Ids {
    fn next(&mut self) -> String {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        format!("{:016x}", z ^ (z >> 31))
    }
}

fn filings(ids: &mut Ids, n: usize, run: usize) -> Vec<Map<String, Value>> {
    let filed = day_at(run).plus_secs(9 * 3_600).to_rfc3339();
    (0..n).map(|_| object(json!({ "doc_id": ids.next(), "digest": format!("f{run}"), "filed_at": filed }))).collect()
}

/// The rows a run of fresh random document ids reads after `runs` runs of `per_run`.
fn fresh_filing_scan(toml: &str, runs: usize, per_run: usize, seed: u64) -> u64 {
    let f = Fixture::new();
    let d = decl(toml);
    let mut ids = Ids(seed);
    let land_run = |run: usize, ids: &mut Ids| {
        let rows = filings(ids, per_run, run);
        land(&f.store, &d, &Batch { rows, types: filing_types() }, &ctx(&format!("run-{run:04}"), day_at(run + 1))).unwrap();
    };
    for run in 0..runs {
        land_run(run, &mut ids);
    }
    let read = read_during(&f.store, || land_run(runs, &mut ids));
    assert_eq!(stored(&f, &d, "doc_id"), (runs + 1) * per_run, "every fresh id lands a vintage");
    read
}

/// A run of 100 fresh random filing ids after 100 such runs, the key named in
/// `bloom_filter` and left out: the rows its vintage comparison reads. The ledger records the
/// table naming it (ledger `vintage-random-key-scan`).
#[test]
fn a_fresh_filing_id_reads_no_stored_vintage() {
    const SEED: u64 = 0x5eed_7106;
    let (runs, per_run) = bars_size();
    let plain = fresh_filing_scan(FILINGS, runs, per_run, SEED);
    let bloom = fresh_filing_scan(FILINGS_KEYED_BLOOM, runs, per_run, SEED);
    eprintln!("fresh ids over {} stored filings read {plain} rows, {bloom} with a key bloom filter", runs * per_run);
    assert_eq!(bloom, 0, "a key bloom filter excludes every stored part holding none of the fresh ids");
    assert_eq!(plain, 0, "a vintage key carries a bloom filter undeclared");
    emit("vintage-random-key-scan", bloom as f64, per_run as u64, SEED);
}

/// [`FILINGS`] with a bloom filter declared on its key.
const FILINGS_KEYED_BLOOM: &str = "name = \"filings\"\nprimary_key = [\"doc_id\"]\ncontent_hash_column = \"digest\"\nbloom_filter = [\"doc_id\"]\n[pipeline.tables.available_time]\ncolumn = \"filed_at\"";

/// A day appended after the 100 × 100 bars fold into one snapshot: the rows its vintage
/// comparison reads (ledger `vintage-after-fold-scan`).
#[test]
fn an_append_after_a_fold_reads_no_stored_vintage() {
    const SEED: u64 = 0x5eed_7107;
    let (symbols, days) = bars_size();
    let (f, d) = backfilled(symbols, days);
    fold::fold(&f.store, &d, published(days, 0)).unwrap();
    let read = read_during(&f.store, || land_bars(&f.store, &d, "append", days, day_rows(symbols, days)));
    assert_eq!(stored(&f, &d, "symbol"), symbols * (days + 1));
    eprintln!("a one-day append after a fold over {} vintages read {read} rows", symbols * days);
    assert_eq!(read, 0, "the snapshot's footer bounds exclude a day past every stored valid time");
    emit("vintage-after-fold-scan", read as f64, symbols as u64, SEED);
}

/// The rows every vintage comparison reads in total while `writers` threads each land `runs`
/// runs of `per_run` never-seen bars, run `r` on day `r`, writer `w`'s `i`th key named
/// `key(w, r, i)`.
fn concurrent_scan(writers: usize, runs: usize, per_run: usize, key: impl Fn(usize, usize, usize) -> String + Sync) -> u64 {
    let f = Fixture::new();
    let d = decl(BARS);
    let key = &key;
    let outcomes: Vec<contextful_context::Result<()>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..writers)
            .map(|w| {
                let (store, d) = (f.store.clone(), d.clone());
                scope.spawn(move || -> Vec<contextful_context::Result<()>> {
                    (0..runs)
                        .map(|r| {
                            let rows = (0..per_run)
                                .map(|i| {
                                    let mut row = bar(i, r, &format!("w{w}-{r}-{i}"), 0);
                                    row.insert("symbol".into(), json!(key(w, r, i)));
                                    row
                                })
                                .collect();
                            land(&store, &d, &Batch { rows, types: bars_types() }, &ctx(&format!("w{w}-run-{r:03}"), published(r, 1))).map(drop)
                        })
                        .collect()
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    let timeouts = outcomes.iter().filter(|o| o.as_ref().is_err_and(|e| e.to_string().contains("held by another process"))).count();
    assert_eq!(timeouts, 0, "no landing outwaits a lock: {outcomes:?}");
    assert_eq!(outcomes.len(), writers * runs, "every landing reports an outcome");
    assert!(outcomes.iter().all(Result::is_ok), "{outcomes:?}");
    assert_eq!(stored(&f, &d, "symbol"), writers * runs * per_run);
    f.store.vintage_rows_read()
}

/// 8 writers, 25 runs and 100 keys a run under measure; 4, 5 and 20 elsewhere.
fn concurrent_size() -> (usize, usize, usize) {
    if measuring() { (8, 25, 100) } else { (4, 5, 20) }
}

/// 8 threads each landing 25 runs of 100 never-seen keys, each writer's keys under its own
/// prefix, into one bars table: the rows every vintage comparison reads in total (ledger
/// `vintage-concurrent-scan`).
#[test]
fn eight_writers_landing_new_keys_scan_no_history() {
    const SEED: u64 = 0x5eed_7108;
    let (writers, runs, per_run) = concurrent_size();
    let read = concurrent_scan(writers, runs, per_run, |w, r, i| format!("W{w}-R{r}-{i}"));
    assert_eq!(read, 0, "no commit reads another writer's keys or another day's vintages");
    eprintln!("{writers} writers × {runs} runs × {per_run} new keys read {read} stored rows");
    emit("vintage-concurrent-scan", read as f64, (writers * runs) as u64, SEED);
}

/// 8 threads each landing 25 runs of 100 never-seen keys into one bars table, the keys of a
/// run dealt round-robin across the writers, so every writer's part of a day spans the
/// others' key range: the rows every vintage comparison reads in total (ledger
/// `vintage-interleaved-scan`).
#[test]
fn eight_writers_landing_interleaved_keys_scan_no_history() {
    const SEED: u64 = 0x5eed_7109;
    let (writers, runs, per_run) = concurrent_size();
    let read = concurrent_scan(writers, runs, per_run, |w, r, i| format!("S{r:03}-{:05}", i * writers + w));
    eprintln!("{writers} writers × {runs} runs × {per_run} interleaved new keys read {read} stored rows");
    assert_eq!(read, 0, "no commit reads another writer's keys, though their key ranges overlap");
    emit("vintage-interleaved-scan", read as f64, (writers * runs) as u64, SEED);
}

/// One commit re-pulling 10 unchanged bars over a table of 2 × 10⁴ vintages, in
/// milliseconds: p50 and p95 of 200 timed commits after 20 warm-up, the median of 5 batches.
/// An unchanged re-pull lands no vintage, so the table's rows stay fixed across the commits;
/// each commit still adds one run manifest holding no row (ledger
/// `vintage-commit-latency-p50`, `vintage-commit-latency-p95`).
#[test]
fn vintage_commit_latency_at_twenty_thousand_rows() {
    const SEED: u64 = 0x5eed_7104;
    let (symbols, days, warm_up, repeats, batches) = if measuring() { (200, 100, 20, 200, 5) } else { (20, 10, 1, 5, 1) };
    let f = Fixture::new();
    let d = decl(BARS);
    // Ten runs of ten days each hold the base rows.
    for chunk in 0..10 {
        let rows = (chunk * days / 10..(chunk + 1) * days / 10).flat_map(|day| day_rows(symbols, day)).collect();
        land_bars(&f.store, &d, &format!("base-{chunk:02}"), days, rows);
    }
    let base = stored(&f, &d, "symbol");
    assert_eq!(base, symbols * days);
    let day = days - 1;
    let mut commit = 0usize;
    let mut timed = |n: usize| -> Vec<f64> {
        (0..n)
            .map(|_| {
                commit += 1;
                let rows = (0..10).map(|s| bar(s, day, &format!("d{day}-{s}"), 1 + commit as u64)).collect();
                let started = std::time::Instant::now();
                land_bars(&f.store, &d, &format!("repull-{commit:05}"), day, rows);
                started.elapsed().as_secs_f64() * 1e3
            })
            .collect()
    };
    timed(warm_up);
    let samples: Vec<Vec<f64>> = (0..batches).map(|_| timed(repeats)).collect();
    let (p50, p95) = contextful_eval::trend::median_batch(&samples).unwrap();
    assert_eq!(stored(&f, &d, "symbol"), base, "an unchanged re-pull lands no vintage");
    eprintln!("a vintage commit over {base} rows: p50 {p50:.2} ms, p95 {p95:.2} ms");
    emit("vintage-commit-latency-p50", p50, repeats as u64, SEED);
    emit("vintage-commit-latency-p95", p95, repeats as u64, SEED);
}

/// A re-pull spelling its valid time another way, with an unchanged digest, lands no
/// vintage, though the table names the valid-time column in `bloom_filter`.
#[test]
fn a_repull_respelling_its_valid_time_lands_no_vintage() {
    let f = Fixture::new();
    let d = decl("name = \"bars\"\nprimary_key = [\"symbol\"]\ncontent_hash_column = \"digest\"\nbloom_filter = [\"day\"]\n[pipeline.tables.valid_time]\nfrom = \"day\"\n[pipeline.tables.available_time]\ncolumn = \"published_at\"");
    let types = bars_types();
    let pull = |run: &str, day: &str, late: u64| {
        let row = object(json!({"symbol": "S0001", "day": day, "digest": "same", "published_at": published(0, late).to_rfc3339()}));
        land(&f.store, &d, &Batch { rows: vec![row], types: types.clone() }, &ctx(run, published(0, late + 1))).unwrap();
    };
    pull("first", "2030-01-01T00:00:00Z", 0);
    pull("offset", "2030-01-01T00:00:00+00:00", 2);
    pull("fraction", "2030-01-01T00:00:00.000Z", 4);
    assert_eq!(stored(&f, &d, "symbol"), 1, "each spelling of one valid time matches the stored vintage");
}

/// Bars landed newest day first fold into a snapshot ordered by valid time, then symbol.
// spec: store.available-time.vintage-fold-order@3f46959d
#[test]
fn a_vintage_fold_orders_its_snapshot_by_valid_time() {
    let f = Fixture::new();
    let d = decl(BARS);
    for day in (0..4).rev() {
        land_bars(&f.store, &d, &format!("day-{day:04}"), day, day_rows(3, day));
    }
    fold::fold(&f.store, &d, published(4, 0)).unwrap();
    let scan = contextful_context::scan::scan(&f.store, &d, Default::default()).unwrap();
    assert_eq!(scan.files.len(), 1, "one snapshot part: {:?}", scan.files);
    let path = f.store.logical_path(&scan.files[0]).unwrap();
    let file = std::fs::File::open(&path).unwrap();
    let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file).unwrap().build().unwrap();
    let mut order = Vec::new();
    for batch in reader {
        order.extend(contextful_context::rows::batch_rows(&batch.unwrap(), &["day", "symbol"]).unwrap().into_iter().map(|row| (row["day"].as_str().unwrap().to_string(), row["symbol"].as_str().unwrap().to_string())));
    }
    assert_eq!(order.len(), 12);
    let mut sorted = order.clone();
    sorted.sort();
    assert_eq!(order, sorted, "the snapshot holds its rows in valid-time order");
}

/// Set in the re-executed child to the project directory it commits into.
const MEMORY_CHILD_VAR: &str = "CONTEXTFUL_VINTAGE_MEMORY_CHILD";

/// This process's peak resident set in bytes: `ru_maxrss` counts KiB on Linux and bytes on
/// macOS.
#[cfg(unix)]
fn peak_rss_bytes() -> u64 {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) }, 0);
    let raw = usage.ru_maxrss as u64;
    if cfg!(target_os = "macos") { raw } else { raw * 1024 }
}

/// A one-day commit of 1000 symbols over a vintage table of 10⁶ rows, run in a re-executed
/// child of this test binary that reports its own peak resident set in MiB (ledger
/// `vintage-commit-memory`). The parent builds the base through the same table declared
/// without availability time, so building it compares no vintage. The child inherits the
/// measure variable, so it sizes its day as the parent sized the table, and records nothing.
#[cfg(unix)]
#[test]
fn a_one_day_commit_on_a_million_vintages_holds_bounded_memory() {
    const SEED: u64 = 0x5eed_7105;
    const NAME: &str = "vintage_cost::a_one_day_commit_on_a_million_vintages_holds_bounded_memory";
    let (symbols, days) = if measuring() { (1000, 1000) } else { (20, 50) };
    if let Some(dir) = std::env::var_os(MEMORY_CHILD_VAR) {
        let store = Store::open(std::path::Path::new(&dir), "research").unwrap();
        land_bars(&store, &decl(BARS), "one-day", days, day_rows(symbols, days));
        println!("peak_rss_bytes={}", peak_rss_bytes());
        return;
    }
    let f = Fixture::new();
    let plain = decl("name = \"bars\"\nprimary_key = [\"symbol\"]\ncontent_hash_column = \"digest\"\n[pipeline.tables.valid_time]\nfrom = \"day\"");
    let per_run = (100_000 / symbols).max(1);
    for first in (0..days).step_by(per_run) {
        let rows = (first..(first + per_run).min(days)).flat_map(|day| day_rows(symbols, day)).collect();
        land_bars(&f.store, &plain, &format!("base-{first:04}"), first, rows);
    }
    let project = f.store.root().ancestors().nth(3).expect("the store root sits at `.contextful/context/<project>` under its project");
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
        .env(MEMORY_CHILD_VAR, project)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&child.stdout);
    assert!(child.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&child.stderr));
    let bytes: u64 = stdout.lines().find_map(|l| l.split_once("peak_rss_bytes=").map(|(_, v)| v)).and_then(|v| v.trim().parse().ok()).unwrap_or_else(|| panic!("no peak in {stdout}"));
    assert_eq!(stored(&f, &decl(BARS), "symbol"), symbols * (days + 1));
    let mib = bytes as f64 / (1024.0 * 1024.0);
    eprintln!("a one-day commit over {} vintages peaked at {mib:.1} MiB", symbols * days);
    assert!(mib <= 256.0, "a one-day commit holds only its own keys' history, peaked at {mib:.1} MiB");
    emit("vintage-commit-memory", mib, 1, SEED);
}
