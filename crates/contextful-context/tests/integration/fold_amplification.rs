//! `store.fold.pass` and `store.fold.triggers`: the bytes a pass writes per byte its runs
//! landed, and the rows its sidecars index per row landed, as the table's history grows.
//! Landed bytes are the part files a run's manifest names; written bytes are the files
//! under the snapshot directory a pass publishes. Each figure is today's value, recorded
//! for the ledger.
//!
//! Under `contextful-ci measure` the fixtures run at full size; elsewhere, the workspace
//! stage among them, a scaled-down history exercises the same path.

use crate::support::{at, decl, Fixture};
use contextful_context::fold::fold;
use contextful_context::land::{land, Batch, LandSession, Position, RunContext, SessionLanding};
use contextful_core::store::declare::TableDecl;
use contextful_core::store::fold::FoldOutcome;
use contextful_core::store::index::IndexEntry;
use contextful_core::store::lay_out::{NodeId, RunManifest};
use contextful_core::store::reconcile::{ColumnType, FloatItem};
use contextful_core::store::reserve::Injection;
use contextful_core::time::Instant;
use contextful_eval::record::{emit, MEASURE_DIR_VAR};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::Path;

const NODE: &str = "ingest-a";

fn measuring() -> bool {
    std::env::var_os(MEASURE_DIR_VAR).is_some()
}

fn object(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn injection(run: &str) -> Injection {
    Injection { run_id: run.into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, authored_agent: None, taint: None }
}

/// Land `rows` as run `run`, committed at `now`, outside any session.
fn land_at(f: &Fixture, d: &TableDecl, run: &str, rows: Vec<Map<String, Value>>, types: &HashMap<String, ColumnType>, now: Instant) {
    let ctx = RunContext { node: NodeId::parse(NODE).unwrap(), injection: injection(run), committed_at: now };
    land(&f.store, d, &Batch { rows, types: types.clone() }, &ctx).unwrap();
}

/// Bytes of every file under `dir`.
fn tree_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap())
        .map(|e| if e.file_type().unwrap().is_dir() { tree_bytes(&e.path()) } else { e.metadata().unwrap().len() })
        .sum()
}

/// Bytes of the part files `m` names.
fn landed_bytes(f: &Fixture, m: &RunManifest) -> u64 {
    let dir = f.table_dir(&m.table).join("data/runs").join(&m.run_id).join(&m.node_id);
    m.parts.iter().map(|p| std::fs::metadata(dir.join(&p.name)).unwrap().len()).sum()
}

/// Bytes under the directory of the table's current snapshot.
fn snapshot_bytes(f: &Fixture, table: &str) -> u64 {
    let (chain, _) = f.store.chain(table).unwrap();
    tree_bytes(&f.store.snapshot_dir(table, &chain[0].snapshot_id).unwrap())
}

/// What a sequence of session landings cost: bytes their runs landed, bytes their passes
/// wrote, and the passes that fired.
#[derive(Default)]
struct Cost {
    landed: u64,
    written: u64,
    passes: usize,
}

impl Cost {
    fn add(&mut self, f: &Fixture, table: &str, landing: &SessionLanding) {
        self.landed += landed_bytes(f, &landing.manifest);
        match &landing.fold {
            Some(FoldOutcome::Folded { .. }) => {
                self.written += snapshot_bytes(f, table);
                self.passes += 1;
            }
            None => {}
            Some(other) => panic!("a fired pass folds: {other:?}"),
        }
    }

    fn amplification(&self) -> f64 {
        self.written as f64 / self.landed as f64
    }
}

/// A seeded stream of words and floats.
struct Draw(u64);

impl Draw {
    fn next(&mut self) -> u64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn word(&mut self) -> String {
        const SYLLABLES: [&str; 16] = ["ka", "ri", "mo", "ten", "sa", "lu", "vor", "en", "pi", "dor", "na", "qu", "zel", "o", "ash", "bri"];
        (0..2 + self.next() % 3).map(|_| SYLLABLES[(self.next() % 16) as usize]).collect()
    }

    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    }
}

// ---------------------------------------------------------------- events

/// Unkeyed news events, partitioned by day.
const EVENTS: &str = "name = \"events\"\npartition_by = [\"day\"]";

fn event(draw: &mut Draw, id: &str, day: u64) -> Map<String, Value> {
    object(json!({
        "event_id": id,
        "day": format!("2030-{:02}-{:02}", 1 + day / 28, 1 + day % 28),
        "actor": draw.word(),
        "action": draw.word(),
        "tone": (draw.next() % 2000) as f64 / 100.0 - 10.0,
        "url": format!("https://news.example/{}/{}", draw.word(), draw.next() % 100_000),
    }))
}

/// A folded events history of `history` rows over 100 days.
fn events_history(history: usize, draw: &mut Draw, folded_at: Instant) -> (Fixture, TableDecl) {
    let f = Fixture::new();
    let d = decl(EVENTS);
    let types = HashMap::new();
    let chunk = 10_000.min(history);
    for (run, first) in (0..history).step_by(chunk).enumerate() {
        let rows = (first..(first + chunk).min(history)).map(|i| event(draw, &format!("h{i}"), (i % 100) as u64)).collect();
        land_at(&f, &d, &format!("history-{run:03}"), rows, &types, folded_at.minus_secs(60));
    }
    assert!(matches!(fold(&f.store, &d, folded_at).unwrap(), FoldOutcome::Folded { .. }));
    (f, d)
}

/// `runs` session landings of `rows` events each, `every` seconds apart from the history's
/// pass, all on day 100.
fn land_events(f: &Fixture, d: &TableDecl, draw: &mut Draw, start: Instant, runs: usize, rows: usize, every: u64) -> Cost {
    let mut session = LandSession::open(&f.store, d.clone(), NodeId::parse(NODE).unwrap()).unwrap();
    let mut cost = Cost::default();
    for run in 1..=runs {
        let batch = Batch { rows: (0..rows).map(|i| event(draw, &format!("r{run}-{i}"), 100)).collect(), types: HashMap::new() };
        let landing = session.land(&batch, injection(&format!("live-{run:04}")), start.plus_secs(run as u64 * every), &Position::default(), &|| Ok(())).unwrap();
        cost.add(f, &d.name, &landing);
    }
    cost
}

/// A day of 96 quarter-hour runs over a folded history of `history` events: the 6 h
/// trigger fires 4 passes.
fn quarter_hour_day(history: usize, rows: usize, seed: u64) -> Cost {
    let mut draw = Draw(seed);
    let start = at("2030-04-10T00:00:00Z");
    let (f, d) = events_history(history, &mut draw, start);
    let cost = land_events(&f, &d, &mut draw, start, 96, rows, 15 * 60);
    assert_eq!(cost.passes, 4, "the 6 h trigger fires every 24 quarter hours");
    cost
}

/// 96 quarter-hour runs of 260 events over a folded history of 10⁵: bytes the 4 passes
/// write per byte the runs land (ledger `fold-events-write-amp`), and that figure at a
/// history of 8 × 10⁴ over the figure at 2 × 10⁴ (ledger `fold-events-write-amp-growth`).
#[test]
fn a_day_of_quarter_hour_landings_writes_bounded_bytes_per_byte_landed() {
    const SEED: u64 = 0x5eed_00f1;
    let (history, rows, sweep) = if measuring() { (100_000, 260, [20_000, 80_000]) } else { (2_000, 26, [500, 2_000]) };
    let day = quarter_hour_day(history, rows, SEED);
    eprintln!("events at {history}: {} B landed, {} B written, {:.2} B per B", day.landed, day.written, day.amplification());
    emit("fold-events-write-amp", day.amplification(), day.passes as u64, SEED);
    let [small, large] = sweep.map(|h| quarter_hour_day(h, rows, SEED).amplification());
    let growth = large / small;
    eprintln!("events write amplification {small:.2} at {} and {large:.2} at {}: {growth:.2}×", sweep[0], sweep[1]);
    emit("fold-events-write-amp-growth", growth, 2, SEED);
}

/// 500 runs of 10 events, 7 s apart, over a folded history of 10⁵: the 50-run trigger fires
/// 10 passes. Bytes they write per byte the runs land (ledger `fold-events-write-amp-by-count`).
#[test]
fn many_small_commits_fold_by_run_count_with_bounded_amplification() {
    const SEED: u64 = 0x5eed_00f7;
    let (history, runs) = if measuring() { (100_000, 500) } else { (2_000, 100) };
    let mut draw = Draw(SEED);
    let start = at("2030-04-10T00:00:00Z");
    let (f, d) = events_history(history, &mut draw, start);
    let cost = land_events(&f, &d, &mut draw, start, runs, 10, 7);
    assert_eq!(cost.passes, runs / contextful_core::store::fold::COMPACTION_RUN_COUNT);
    eprintln!("{runs} small runs over {history}: {} B landed, {} B written, {:.2} B per B", cost.landed, cost.written, cost.amplification());
    emit("fold-events-write-amp-by-count", cost.amplification(), cost.passes as u64, SEED);
}

// ---------------------------------------------------------------- bars

/// Daily bars keyed by symbol and day, partitioned by year.
const BARS: &str = "name = \"bars\"\nprimary_key = [\"symbol\", \"day\"]\npartition_by = [\"year\"]";

fn bar(symbol: usize, day: u64, close: f64) -> Map<String, Value> {
    let at = at("2027-01-01T00:00:00Z").plus_secs(day * 86_400).to_rfc3339();
    object(json!({ "symbol": format!("S{symbol:04}"), "day": &at[..10], "year": &at[..4], "close": close, "volume": 1_000 + day }))
}

/// 100 symbols × 1000 days of folded bars, then 20 daily runs each landing a day for every
/// symbol plus a restatement of the prior day for a tenth of them; a run a day fires the
/// 6 h trigger every run. Bytes the passes write per byte the runs land (ledger
/// `fold-bars-write-amp`).
#[test]
fn daily_keyed_bars_with_restatements_write_bounded_bytes_per_byte_landed() {
    const SEED: u64 = 0x5eed_00f2;
    let (symbols, days, live) = if measuring() { (100, 1000, 20) } else { (10, 100, 4) };
    let f = Fixture::new();
    let d = decl(BARS);
    let types = HashMap::new();
    let start = at("2030-01-01T00:00:00Z");
    for (run, first) in (0..days).step_by(100).enumerate() {
        let rows = (first..(first + 100).min(days)).flat_map(|day| (0..symbols).map(move |s| bar(s, day as u64, 100.0))).collect();
        land_at(&f, &d, &format!("history-{run:03}"), rows, &types, start.minus_secs(60));
    }
    assert!(matches!(fold(&f.store, &d, start).unwrap(), FoldOutcome::Folded { .. }));
    let mut session = LandSession::open(&f.store, d.clone(), NodeId::parse(NODE).unwrap()).unwrap();
    let mut cost = Cost::default();
    for k in 1..=live {
        let day = (days + k - 1) as u64;
        let mut rows: Vec<_> = (0..symbols).map(|s| bar(s, day, 101.0)).collect();
        rows.extend((0..symbols).step_by(10).map(|s| bar(s, day - 1, 102.5)));
        let batch = Batch { rows, types: HashMap::new() };
        let landing = session.land(&batch, injection(&format!("day-{k:03}")), start.plus_secs(k as u64 * 86_400), &Position::default(), &|| Ok(())).unwrap();
        cost.add(&f, "bars", &landing);
    }
    assert_eq!(cost.passes, live, "a run a day fires the 6 h trigger every run");
    let held = contextful_context::rows::table_rows(&f.store, &d, &["symbol"]).unwrap().len();
    assert_eq!(held, symbols * (days + live), "a restatement replaces its key's row");
    eprintln!("bars over {}: {} B landed, {} B written, {:.2} B per B", symbols * days, cost.landed, cost.written, cost.amplification());
    emit("fold-bars-write-amp", cost.amplification(), cost.passes as u64, SEED);
}

// ---------------------------------------------------------------- sidecars

const DIM: u32 = 16;

/// Passages carrying a 16-dimension vector sidecar and a full-text sidecar.
fn passages() -> TableDecl {
    decl(&format!(
        "name = \"passages\"\nprimary_key = [\"passage_id\"]\n[[pipeline.tables.indexes]]\nkind = \"vector\"\ncolumn = \"embedding\"\nmodel = \"e5\"\ndim = {DIM}\n[[pipeline.tables.indexes]]\nkind = \"fulltext\"\ncolumn = \"body\"\n"
    ))
}

fn passage(draw: &mut Draw, id: &str) -> Map<String, Value> {
    let body: Vec<String> = (0..12).map(|_| draw.word()).collect();
    let embedding: Vec<f32> = (0..DIM).map(|_| draw.unit()).collect();
    object(json!({ "passage_id": id, "body": body.join(" "), "embedding": embedding }))
}

/// Rows the pass publishing `entry` indexed: every row it holds after a full build, the
/// rows added since `prior` after an in-place extension (`store.index.graph-extensions`).
fn indexed(entry: &IndexEntry, prior: Option<&IndexEntry>) -> u64 {
    match (entry, prior) {
        (IndexEntry::Vector(e), Some(IndexEntry::Vector(p))) if e.extensions > 0 => e.row_count - p.row_count,
        (IndexEntry::Vector(e), _) => e.row_count,
        (IndexEntry::Fulltext(e), _) => e.row_count,
        (other, _) => panic!("a declared sidecar reads as {other:?}"),
    }
}

/// 2 × 10⁴ folded passages, then 4 passes each folding a run of 1200: rows each sidecar
/// indexes per row landed (ledger `fold-vector-rows-indexed`, `fold-fulltext-rows-indexed`).
#[test]
fn sidecars_indexed_per_quarter_hour_landing_stay_bounded() {
    const SEED: u64 = 0x5eed_00f3;
    let (history, per_pass, passes) = if measuring() { (20_000, 1_200, 4) } else { (400, 60, 4) };
    let f = Fixture::new();
    let d = passages();
    let types: HashMap<String, ColumnType> = [("embedding".to_string(), ColumnType::FixedSizeList(FloatItem::Float32, DIM))].into_iter().collect();
    let mut draw = Draw(SEED);
    let start = at("2030-04-10T00:00:00Z");
    let rows = (0..history).map(|i| passage(&mut draw, &format!("h{i}"))).collect();
    land_at(&f, &d, "history", rows, &types, start.minus_secs(60));
    fold(&f.store, &d, start).unwrap();
    let sidecars = |f: &Fixture| f.store.chain("passages").unwrap().0.remove(0).indexes;
    let mut prior = sidecars(&f);
    let (mut vector, mut fulltext) = (0u64, 0u64);
    for pass in 1..=passes {
        let now = start.plus_secs(pass as u64 * 15 * 60);
        let rows = (0..per_pass).map(|i| passage(&mut draw, &format!("p{pass}-{i}"))).collect();
        land_at(&f, &d, &format!("live-{pass}"), rows, &types, now);
        assert!(matches!(fold(&f.store, &d, now).unwrap(), FoldOutcome::Folded { .. }));
        let current = sidecars(&f);
        for entry in &current {
            let same = prior.iter().find(|p| p.kind() == entry.kind());
            match entry {
                IndexEntry::Vector(_) => vector += indexed(entry, same),
                _ => fulltext += indexed(entry, same),
            }
        }
        prior = current;
    }
    let landed = (per_pass * passes) as f64;
    let (vector, fulltext) = (vector as f64 / landed, fulltext as f64 / landed);
    assert!(vector >= 1.0 && fulltext >= 1.0, "each pass indexes at least the rows it folds");
    eprintln!("over {history} passages: vector indexes {vector:.2} and full text {fulltext:.2} rows per row landed");
    emit("fold-vector-rows-indexed", vector, passes as u64, SEED);
    emit("fold-fulltext-rows-indexed", fulltext, passes as u64, SEED);
}
