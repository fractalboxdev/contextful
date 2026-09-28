use contextful_eval::record::*;

#[test]
fn a_record_round_trips_and_carries_the_runner_stamp() {
    let dir = tempfile::tempdir().unwrap();
    let r = Record::new("journal-effect-once", 1.0, 100, 0x5eed_0001);
    assert!(r.run.nproc >= 1);
    assert!(!r.run.processor.is_empty());
    let p = write(dir.path(), &r).unwrap();
    assert_eq!(p, dir.path().join("journal-effect-once.json"));
    let text = std::fs::read_to_string(&p).unwrap();
    for field in ["\"id\"", "\"value\"", "\"n\"", "\"seed\"", "\"processor\"", "\"nproc\"", "\"memory_limit\""] {
        assert!(text.contains(field), "{field}: {text}");
    }
    assert_eq!(read(dir.path(), "journal-effect-once").unwrap(), r);
}

#[test]
fn an_absent_malformed_or_foreign_record_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let code = |id: &str| read(dir.path(), id).unwrap_err().code();
    assert_eq!(code("guard-catalogue"), "MeasureRecordMissing");

    std::fs::write(path(dir.path(), "guard-catalogue"), "{\"id\": \"guard-catalogue\"}").unwrap();
    assert_eq!(code("guard-catalogue"), "MeasureRecordMissing");

    write(dir.path(), &Record::new("guard-catalogue", 10.0, 16, 0)).unwrap();
    std::fs::copy(path(dir.path(), "guard-catalogue"), path(dir.path(), "policy-no-jwt")).unwrap();
    let e = read(dir.path(), "policy-no-jwt").unwrap_err();
    assert!(e.to_string().contains("names entry `guard-catalogue`"), "{e}");
}

#[test]
fn emit_writes_only_under_a_measure_run() {
    let dir = tempfile::tempdir().unwrap();
    std::env::remove_var(MEASURE_DIR_VAR);
    assert_eq!(emit("eval-record-probe", 0.0, 1, 0), None, "no measure run collects records");
    std::env::set_var(MEASURE_DIR_VAR, dir.path());
    let written = emit("eval-record-probe", 3.0, 4, 5);
    std::env::remove_var(MEASURE_DIR_VAR);
    assert_eq!(written, Some(path(dir.path(), "eval-record-probe")));
    let r = read(dir.path(), "eval-record-probe").unwrap();
    assert_eq!((r.value, r.n, r.seed), (3.0, 4, 5));
}
