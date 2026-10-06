use contextful_eval::record;
use std::process::Command;

fn probe(name: &str, id: &str) -> record::Record {
    let dir = tempfile::tempdir().unwrap();
    let bin = match name {
        "audit-open-bounded-heap" => env!("CARGO_BIN_EXE_audit-open-bounded-heap"),
        "audit-query-digest-keyed" => env!("CARGO_BIN_EXE_audit-query-digest-keyed"),
        _ => panic!("unknown probe {name}"),
    };
    let output = Command::new(bin)
        .env(record::MEASURE_DIR_VAR, dir.path())
        .output()
        .unwrap();
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
