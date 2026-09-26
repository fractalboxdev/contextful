//! `run.seed`: the seed block, the seeded-table declaration and the ordering-stamp ceiling.

use contextful_core::pipeline::declare::{read_manifest, ManifestFile, PipelineSpec};
use contextful_core::pipeline::seed::{Ceiling, SeedCeiling};
use contextful_core::run::ports::Row;
use contextful_core::run::RunError;
use serde_json::{json, Value};

fn spec(text: &str) -> PipelineSpec {
    read_manifest(&ManifestFile { path: "contextful.toml".into(), text: text.into() }).unwrap().remove(0).spec
}

/// A seeded pipeline whose one table carries `table` keys.
fn seeded(below: &str, table: &str) -> String {
    format!(
        "[[pipeline]]\nid = \"orders\"\n[pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"https://api.example.test/v1\" }}\n\
         [[pipeline.tables]]\nname = \"items\"\n{table}\
         [pipeline.seed]\nbelow = {below}\n[pipeline.seed.source]\nname = \"file\"\nconfig = {{ path = \"exports/items.jsonl\" }}\n"
    )
}

const KEYED: &str = "primary_key = [\"id\"]\norder_by = \"updated_at\"\n";

fn rows(stamps: &[Value]) -> Vec<Row> {
    stamps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let mut r = Row::new();
            r.insert("id".into(), json!(i));
            r.insert("updated_at".into(), s.clone());
            r
        })
        .collect()
}

fn instant_gate() -> SeedCeiling {
    SeedCeiling::new("updated_at", Ceiling::from_value(&json!("2030-01-01T00:00:00Z")).unwrap())
}

/// `[pipeline.seed]` attaches a bulk-load source with a ceiling `below` expressed on the seeded table's `order_by` scale.
// spec: run.seed.block@d465fb73
#[test]
fn a_seed_block_names_a_source_and_a_ceiling_on_the_ordering_scale() {
    let s = spec(&seeded("\"2030-01-01T00:00:00Z\"", KEYED));
    let block = s.seed_block().unwrap().expect("a declared seed block");
    assert_eq!(block.source.name, "file");
    assert_eq!(block.source.config, json!({"path": "exports/items.jsonl"}));
    assert!(matches!(block.below, Ceiling::Instant { fraction_digits: 0, .. }));
    assert!(s.validate().is_ok());

    let numeric = spec(&seeded("1700000000", KEYED)).seed_block().unwrap().unwrap();
    assert_eq!(numeric.below, Ceiling::Number(1_700_000_000.into()));

    let unseeded = spec(seeded("1", KEYED).split("[pipeline.seed]").next().unwrap());
    assert!(unseeded.seed_block().unwrap().is_none());

    for (text, part) in [
        ("[[pipeline]]\nid = \"o\"\nseed = { below = 5 }\n[pipeline.source]\nname = \"http\"\n[[pipeline.tables]]\nname = \"t\"\nprimary_key = [\"id\"]\norder_by = \"at\"\n", "source"),
        (&seeded("true", KEYED)[..], "below"),
        (&seeded("\"next tuesday\"", KEYED)[..], "below"),
        (&seeded("1", KEYED).replace("[pipeline.seed]\n", "[pipeline.seed]\nbackfill = { max_chunks = 4 }\n")[..], "seed.backfill"),
    ] {
        match spec(text).validate() {
            Err(RunError::PipelineSpecInvalid(m)) => assert!(m.contains(part), "{part}: {m}"),
            other => panic!("{part}: {other:?}"),
        }
    }
}

/// A seeded table without `primary_key`, or with `order_by` left at the ingest stamp, raises
/// `PipelineSeedDeclarationMissing` at plan and validate.
// spec: run.seed.declaration-missing@661f2232
#[test]
fn a_seeded_table_needs_a_key_and_an_event_time_ordering() {
    for (table, missing) in [
        ("order_by = \"updated_at\"\n", "primary_key"),
        ("primary_key = [\"id\"]\n", "order_by"),
        ("primary_key = [\"id\"]\norder_by = \"_ingested_at\"\n", "order_by"),
        ("", "primary_key"),
    ] {
        match spec(&seeded("\"2030-01-01T00:00:00Z\"", table)).validate() {
            Err(RunError::PipelineSeedDeclarationMissing(m)) => {
                assert!(m.contains("`items`") && m.contains(missing), "{missing}: {m}");
            }
            other => panic!("{table:?}: {other:?}"),
        }
    }
    let live = spec(seeded("1", "").split("[pipeline.seed]").next().unwrap());
    assert!(live.validate().is_ok(), "an unseeded table orders by the ingest stamp");
}

/// The gate refuses a batch holding a stamp at or past `below`, naming the value, and leaves
/// every row and stamp as it found them. `run.seed.ceiling-breached` also requires the chunk
/// to land nothing, which holds once the land path calls the gate before the write.
#[test]
fn a_stamp_at_or_past_the_ceiling_refuses_the_whole_batch() {
    let gate = instant_gate();
    assert!(gate.check("orders_items", &rows(&[json!("2029-12-31T23:59:59Z"), json!("2029-06-01T00:00:00Z")])).is_ok());

    for stamp in ["2030-01-01T00:00:00Z", "2031-02-03T00:00:00Z"] {
        let batch = rows(&[json!("2020-01-01T00:00:00Z"), json!(stamp)]);
        let before = batch.clone();
        match gate.check("orders_items", &batch) {
            Err(RunError::PipelineSeedCeilingBreached(m)) => {
                assert!(m.contains(stamp) && m.contains("orders_items") && m.contains("updated_at"), "{m}");
            }
            other => panic!("{stamp}: {other:?}"),
        }
        assert_eq!(batch, before, "the refused batch keeps every row and stamp");
    }

    let numeric = SeedCeiling::new("updated_at", Ceiling::from_value(&json!(1000)).unwrap());
    assert!(numeric.check("t", &rows(&[json!(999), json!(999.5)])).is_ok());
    for stamp in [json!(1000), json!(1000.5), json!(u64::MAX)] {
        assert!(matches!(numeric.check("t", &rows(std::slice::from_ref(&stamp))), Err(RunError::PipelineSeedCeilingBreached(m)) if m.contains(&stamp.to_string())));
    }
}

/// A batch missing the ordering column, or a stamp unorderable against the ceiling, raises
/// `PipelineSeedCeilingUnevaluable`; a text stamp orders only when `Z`-suffixed at the ceiling's fractional width.
// spec: run.seed.ceiling-unevaluable@aa21d6f5
#[test]
fn a_missing_column_or_an_off_scale_stamp_is_unevaluable() {
    let gate = instant_gate();
    let mut missing = rows(&[json!("2020-01-01T00:00:00Z")]);
    missing[0].remove("updated_at");
    match gate.check("orders_items", &missing) {
        Err(RunError::PipelineSeedCeilingUnevaluable(m)) => assert!(m.contains("updated_at"), "{m}"),
        other => panic!("{other:?}"),
    }
    for stamp in [json!(null), json!(true), json!({"at": 1}), json!([1]), json!("yesterday"), json!(1_700_000_000)] {
        assert!(
            matches!(gate.check("orders_items", &rows(std::slice::from_ref(&stamp))), Err(RunError::PipelineSeedCeilingUnevaluable(_))),
            "{stamp}"
        );
    }
    // A text column lands as text and ranks byte-wise, so a stamp orders only in the
    // ceiling's spelling: `Z`-suffixed at the ceiling's fractional width. `+09:00` at 14:00Z
    // outranks a later `20:00:00Z` correction lexically, and `59.5Z` sorts below `59Z`.
    for stamp in [
        "2029-12-31T23:00:00+09:00",
        "2029-06-01T09:00:00+09:00",
        "2030-01-01T09:00:00+09:00",
        "2029-12-31T23:59:59.5Z",
        "2029-12-31T23:59:59.000000000Z",
        "2029-12-31t23:59:59Z",
        "2029-12-31T23:59:59z",
        "2029-12-31T23:59:59-00:00",
    ] {
        match gate.check("orders_items", &rows(&[json!("2020-01-01T00:00:00Z"), json!(stamp)])) {
            Err(RunError::PipelineSeedCeilingUnevaluable(m)) => assert!(m.contains(stamp), "{stamp}: {m}"),
            other => panic!("{stamp}: {other:?}"),
        }
    }
    let fractional = SeedCeiling::new("updated_at", Ceiling::from_value(&json!("2030-01-01T09:00:00.000+09:00")).unwrap());
    assert!(fractional.check("t", &rows(&[json!("2029-12-31T23:59:59.500Z"), json!("2029-12-31T23:59:59.999Z")])).is_ok());
    assert!(matches!(fractional.check("t", &rows(&[json!("2030-01-01T00:00:00.000Z")])), Err(RunError::PipelineSeedCeilingBreached(_))));
    for stamp in ["2029-12-31T23:59:59Z", "2029-12-31T23:59:59.5Z", "2029-12-31T23:59:59.500000Z"] {
        assert!(matches!(fractional.check("t", &rows(&[json!(stamp)])), Err(RunError::PipelineSeedCeilingUnevaluable(_))), "{stamp}");
    }

    // Text never orders against a numeric ceiling: "999" sorts after "1000" byte-wise.
    let numeric = SeedCeiling::new("updated_at", Ceiling::from_value(&json!(1000)).unwrap());
    for stamp in [json!("999"), json!("2020-01-01T00:00:00Z")] {
        assert!(matches!(numeric.check("t", &rows(&[stamp])), Err(RunError::PipelineSeedCeilingUnevaluable(_))));
    }
}
