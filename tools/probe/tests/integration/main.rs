use contextful_eval::record;
use std::process::Command;

/// The variable a probe reads to build its workspace-stage fixture instead of the ledger's.
const SCALE_VAR: &str = "CONTEXTFUL_PROBE_SCALE";

/// Outside a measure run a probe builds its small fixture.
fn small() -> bool {
    std::env::var_os(record::MEASURE_DIR_VAR).is_none()
}

fn probe(name: &str, id: &str) -> record::Record {
    let dir = tempfile::tempdir().unwrap();
    let bin = match name {
        "audit-open-bounded-heap" => env!("CARGO_BIN_EXE_audit-open-bounded-heap"),
        "audit-query-digest-keyed" => env!("CARGO_BIN_EXE_audit-query-digest-keyed"),
        "fold-peak-heap" => env!("CARGO_BIN_EXE_fold-peak-heap"),
        "fold-events-peak-rss" => env!("CARGO_BIN_EXE_fold-events-peak-rss"),
        "fold-passages-peak-rss" => env!("CARGO_BIN_EXE_fold-passages-peak-rss"),
        "backfill-decode-peak-heap" => env!("CARGO_BIN_EXE_backfill-decode-peak-heap"),
        "backfill-walk-peak-rss" => env!("CARGO_BIN_EXE_backfill-walk-peak-rss"),
        _ => panic!("unknown probe {name}"),
    };
    let mut command = Command::new(bin);
    command.env(record::MEASURE_DIR_VAR, dir.path());
    if small() {
        command.env(SCALE_VAR, "small");
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    record::read(dir.path(), id).unwrap()
}

#[test]
fn an_audit_open_retains_less_than_64_kib_more_heap_at_100k_entries() {
    let result = probe("audit-open-bounded-heap", "audit-open-bounded-heap");
    assert_eq!(result.n, 100_000);
    assert!(result.value < 64.0, "{} KiB", result.value);
}

#[test]
fn a_keyed_query_digest_matches_no_unkeyed_dictionary_entry() {
    let result = probe("audit-query-digest-keyed", "audit-query-digest-keyed");
    assert_eq!(result.value, 0.0);
    assert!(result.n >= 100);
}

// The arms below hold a record's presence, sample count and seed, not a target.

#[test]
fn a_fold_peak_heap_probe_records_the_growth_across_held_sizes() {
    let result = probe("fold-peak-heap", "fold-peak-heap-growth");
    assert_eq!(result.n, if small() { 20_000 } else { 200_000 });
    assert_eq!(result.seed, 0x5eed_00f4);
    assert!(result.value > 0.0, "{}", result.value);
}

#[test]
fn a_fold_events_peak_rss_probe_records_the_median_child() {
    let result = probe("fold-events-peak-rss", "fold-events-peak-rss");
    assert_eq!(result.n, if small() { 20_000 } else { 1_000_000 });
    assert_eq!(result.seed, 0x5eed_00f5);
    assert!(result.value > 0.0, "{} MiB", result.value);
}

#[test]
fn a_fold_passages_peak_rss_probe_records_the_median_child() {
    let result = probe("fold-passages-peak-rss", "fold-passages-peak-rss");
    assert_eq!(result.n, if small() { 500 } else { 20_000 });
    assert_eq!(result.seed, 0x5eed_00f5);
    assert!(result.value > 0.0, "{} MiB", result.value);
}

#[test]
fn a_backfill_decode_probe_records_the_worst_of_three_formats() {
    let result = probe("backfill-decode-peak-heap", "backfill-decode-peak-heap");
    assert_eq!(result.n, 3);
    assert_eq!(result.seed, 0x5eed_0b03);
    assert!(result.value > 0.0, "{}", result.value);
}

#[test]
fn a_backfill_walk_probe_records_the_median_child() {
    let result = probe("backfill-walk-peak-rss", "backfill-walk-peak-rss");
    assert_eq!(result.n, if small() { 5_000 } else { 99_900 });
    assert_eq!(result.seed, 0x5eed_0b04);
    assert!(result.value > 0.0, "{} MiB", result.value);
}
