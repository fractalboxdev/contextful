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
    for measure_dir in [None, Some(dir.path())] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child.args(["--ignored", "--exact", "record::emit_probe_child"]);
        match measure_dir {
            Some(path) => {
                child.env(MEASURE_DIR_VAR, path);
            }
            None => {
                child.env_remove(MEASURE_DIR_VAR);
            }
        }
        let output = child.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success() && stdout.contains("1 passed; 0 failed"), "stdout: {stdout}\nstderr: {}", String::from_utf8_lossy(&output.stderr));
    }
}

#[test]
#[ignore = "the parent test runs this case in an isolated process"]
fn emit_probe_child() {
    let written = emit("eval-record-probe", 3.0, 4, 5);
    if let Some(dir) = std::env::var_os(MEASURE_DIR_VAR) {
        let dir = std::path::PathBuf::from(dir);
        assert_eq!(written, Some(path(&dir, "eval-record-probe")));
        let r = read(&dir, "eval-record-probe").unwrap();
        assert_eq!((r.value, r.n, r.seed), (3.0, 4, 5));
    } else {
        assert_eq!(written, None, "no measure run collects records");
    }
}
