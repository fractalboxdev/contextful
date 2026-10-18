//! `store.index.fulltext-by-fold`: the rows each pass's full-text build covers when every
//! pass lands one fixed-size increment.

use crate::support::{at, decl, Fixture};
use contextful_context::fold::fold;
use contextful_core::store::fold::FoldOutcome;
use contextful_core::store::index::{own_rows, IndexEntry};
use serde_json::{json, Value};

const SEED: u64 = 0x5eed_0605;
const FOLDS: usize = 10;

/// Ten passes, each folding one run of new keys, report the rows the last pass's sidecar
/// build covered (ledger `fold-fulltext-docs-rebuilt`). Under `contextful-ci measure` a run
/// holds 1,000 rows; elsewhere 100.
#[test]
fn ten_folds_of_a_thousand_rows_reindex_only_the_increment() {
    let measuring = std::env::var_os(contextful_eval::record::MEASURE_DIR_VAR).is_some();
    let increment = if measuring { 1_000 } else { 100 };
    let f = Fixture::new();
    let d = decl("name = \"passages\"\nprimary_key = [\"passage_id\"]\n[[pipeline.tables.indexes]]\nkind = \"fulltext\"\ncolumn = \"body\"\n");
    let mut built = Vec::new();
    for pass in 0..FOLDS {
        let rows: Vec<Value> = (0..increment)
            .map(|i| json!({ "passage_id": format!("p{pass:02}-{i:05}"), "body": format!("filing {i} of pass {pass} widens the margin") }))
            .collect();
        f.land(&d, &format!("run-{pass:02}"), Value::Array(rows), "2030-01-01T00:00:00Z").unwrap();
        let outcome = fold(&f.store, &d, at(&format!("2030-01-01T{:02}:00:00Z", pass + 1))).unwrap();
        assert!(matches!(outcome, FoldOutcome::Folded { runs: 1, .. }), "pass {pass}: {outcome:?}");
        let (chain, _) = f.store.chain("passages").unwrap();
        let entry = chain[0]
            .indexes
            .iter()
            .find_map(|e| match e {
                IndexEntry::Fulltext(e) => Some(e.clone()),
                _ => None,
            })
            .expect("each pass records a full-text entry");
        // Each pass indexes the increment it lands, in its own segment, and answers for every row.
        let rows = own_rows(entry.row_count, &entry.segments);
        assert_eq!(rows, increment as u64, "pass {pass} indexed {rows} rows");
        assert_eq!(entry.row_count, ((pass + 1) * increment) as u64);
        built.push(rows);
    }
    let last = *built.last().unwrap();
    eprintln!("full-text rows built per pass: {built:?}");
    assert!(last <= increment as u64, "the last pass rebuilt {last} documents");
    contextful_eval::record::emit("fold-fulltext-docs-rebuilt", last as f64, FOLDS as u64, SEED);
}
