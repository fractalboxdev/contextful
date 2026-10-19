//! The stores the fold probes measure: a history of `held` rows folded into one snapshot,
//! then one quarter-hour landing of 260 rows the measured pass folds.

#![allow(dead_code)]

use crate::measure::Draw;
use contextful_context::fold::fold;
use contextful_context::land::{land, Batch, RunContext};
use contextful_context::Store;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::fold::FoldOutcome;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reconcile::{ColumnType, FloatItem};
use contextful_core::store::reserve::Injection;
use contextful_core::time::Instant;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::Path;

/// Rows one landing of the increment carries.
pub const INCREMENT: usize = 260;
/// Rows one history run carries.
const CHUNK: usize = 10_000;
/// Days the event history spreads over, one `day=` partition each.
const DAYS: usize = 100;
const DIM: u32 = 16;
const WORDS: [&str; 16] = [
    "solar", "battery", "storage", "grid", "tariff", "export", "import", "wind", "turbine", "hydro", "price", "demand", "supply", "carbon",
    "permit", "auction",
];

#[derive(Clone, Copy)]
pub enum Table {
    /// Unkeyed events, one `day=` partition per day.
    Events,
    /// Keyed passages with a full-text and a 16-dimension vector sidecar.
    Passages,
}

impl Table {
    pub fn parse(s: &str) -> Table {
        match s {
            "events" => Table::Events,
            "passages" => Table::Passages,
            other => panic!("unknown table {other}"),
        }
    }

    pub fn decl(self) -> TableDecl {
        let block = match self {
            Table::Events => "name = \"events\"\npartition_by = [\"day\"]\n".to_string(),
            Table::Passages => format!(
                "name = \"passages\"\nprimary_key = [\"passage_id\"]\n[[pipeline.tables.indexes]]\nkind = \"fulltext\"\ncolumn = \"body\"\n[[pipeline.tables.indexes]]\nkind = \"vector\"\ncolumn = \"embedding\"\nmodel = \"e5\"\ndim = {DIM}\n"
            ),
        };
        TableDecl::parse_pipeline(&format!("[[pipeline.tables]]\n{block}")).unwrap().remove(0)
    }

    fn types(self) -> HashMap<String, ColumnType> {
        match self {
            Table::Events => HashMap::new(),
            Table::Passages => HashMap::from([("embedding".to_string(), ColumnType::FixedSizeList(FloatItem::Float32, DIM))]),
        }
    }

    fn row(self, i: usize, draw: &mut Draw) -> Map<String, Value> {
        let row = match self {
            Table::Events => json!({
                "event_id": i,
                "day": format!("2030-{:02}-{:02}", 1 + (i % DAYS) / 28, 1 + (i % DAYS) % 28),
                "actor": format!("actor-{}", draw.next() % 5000),
                "tone": draw.unit(),
            }),
            Table::Passages => json!({
                "passage_id": format!("p{i}"),
                "body": (0..12).map(|_| WORDS[(draw.next() % WORDS.len() as u64) as usize]).collect::<Vec<_>>().join(" "),
                "embedding": (0..DIM).map(|_| draw.unit()).collect::<Vec<_>>(),
            }),
        };
        row.as_object().unwrap().clone()
    }
}

fn at(minute: usize) -> Instant {
    Instant::from_unix_secs(1_893_456_000 + 60 * minute as i64).unwrap()
}

fn land_run(store: &Store, table: Table, decl: &TableDecl, run: usize, rows: std::ops::Range<usize>, draw: &mut Draw) {
    let batch = Batch { rows: rows.map(|i| table.row(i, draw)).collect(), types: table.types() };
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: format!("run-{run:05}"), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, authored_agent: None, taint: None },
        committed_at: at(run),
    };
    land(store, decl, &batch, &ctx).unwrap();
}

/// Land `held` rows of `table` in runs of 10,000 under `dir`, fold them into one snapshot,
/// then land one 260-row increment the next pass folds.
pub fn build(dir: &Path, table: Table, held: usize, seed: u64) {
    let store = Store::open(dir, "research").unwrap();
    let decl = table.decl();
    let mut draw = Draw::new(seed);
    let mut run = 0;
    for start in (0..held).step_by(CHUNK) {
        land_run(&store, table, &decl, run, start..(start + CHUNK).min(held), &mut draw);
        run += 1;
    }
    assert!(matches!(fold(&store, &decl, at(run)).unwrap(), FoldOutcome::Folded { .. }));
    land_run(&store, table, &decl, run + 1, held..held + INCREMENT, &mut draw);
}

/// Fold the increment [`build`] left under `dir`.
pub fn fold_increment(dir: &Path, table: Table) {
    let store = Store::open(dir, "research").unwrap();
    let outcome = fold(&store, &table.decl(), at(1_000_000)).unwrap();
    assert!(matches!(outcome, FoldOutcome::Folded { .. }), "{outcome:?}");
}
