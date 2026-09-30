//! `run.export`: the `[[export]]` block, the batch statement over the commit sequence, and
//! the OTLP log record each landed row becomes.

use contextful_core::export::{log_records, parse_exports, ExportCursor, ExportError, Signal, EXPORT_BATCH_ROWS};
use contextful_core::store::reconcile::ColumnType;
use serde_json::{json, Value};
use std::collections::BTreeMap;

const BLOCK: &str = "[[export]]\nname = \"spans-mirror\"\ntable = \"spans\"\nendpoint = \"https://otel.example.com/v1/logs\"\nsignal = \"logs\"\nheaders = { Authorization = \"Bearer ${secret://otel-token}\" }\n";

/// A manifest `[[export]]` block declares `name`, one landed `table`, an OTLP/HTTP `endpoint`, `signal` and optional
/// `headers`; `contextful export run <name>` delivers every row its cursor has not passed.
#[test]
fn an_export_block_names_its_table_endpoint_signal_and_header_templates() {
    let exports = parse_exports(BLOCK).unwrap();
    assert_eq!(exports.len(), 1);
    let e = &exports[0];
    assert_eq!((e.name.as_str(), e.table.as_str(), e.endpoint.as_str(), e.signal), ("spans-mirror", "spans", "https://otel.example.com/v1/logs", Signal::Logs));
    assert!(e.headers["Authorization"].has_reference());

    let unknown_key = parse_exports(&BLOCK.replace("signal = \"logs\"", "signal = \"logs\"\nretries = 3")).unwrap_err();
    assert!(unknown_key.to_string().contains("`retries`"), "{unknown_key}");
    let twice = parse_exports(&format!("{BLOCK}{BLOCK}")).unwrap_err();
    assert!(twice.to_string().contains("more than once"), "{twice}");
    let not_http = parse_exports(&BLOCK.replace("https://otel.example.com/v1/logs", "file:///tmp/out")).unwrap_err();
    assert!(not_http.to_string().contains("http"), "{not_http}");
    let bad_name = parse_exports(&BLOCK.replace("spans-mirror", "../escape")).unwrap_err();
    assert!(bad_name.to_string().contains("`../escape`"), "{bad_name}");
}

/// A `signal` other than `logs` raises `ExportSignalUnknown` before any row is read.
// spec: run.export.signal-unknown@ca031f70
#[test]
fn a_signal_other_than_logs_is_refused() {
    for signal in ["traces", "metrics", "Logs"] {
        match parse_exports(&BLOCK.replace("signal = \"logs\"", &format!("signal = \"{signal}\""))) {
            Err(ExportError::ExportSignalUnknown(m)) => assert!(m.contains(signal) && m.contains("spans-mirror"), "{m}"),
            other => panic!("expected ExportSignalUnknown for `{signal}`, got {other:?}"),
        }
    }
    let missing = parse_exports(&BLOCK.replace("signal = \"logs\"\n", "")).unwrap_err();
    assert!(matches!(missing, ExportError::ExportSignalUnknown(_)), "{missing:?}");
}

/// The batch statement reads past the cursor in commit order, one batch at a time.
#[test]
fn the_batch_statement_reads_past_the_cursor_in_commit_order() {
    let e = &parse_exports(BLOCK).unwrap()[0];
    let sql = e.batch_statement(&ExportCursor { commit_seq: 7, row_seq: 41 });
    assert!(sql.contains("FROM \"spans\""), "{sql}");
    assert!(sql.contains("\"_commit_seq\" > 7 OR (\"_commit_seq\" = 7 AND \"_row_seq\" > 41)"), "{sql}");
    assert!(sql.contains("ORDER BY \"_commit_seq\", \"_row_seq\""), "{sql}");
    assert!(sql.ends_with(&format!("LIMIT {EXPORT_BATCH_ROWS}")), "{sql}");
    assert!(!sql.contains("_ingested_at"), "{sql}");
    assert_eq!(EXPORT_BATCH_ROWS, 500);
}

/// Each row becomes one OTLP log record: `timeUnixNano` from `_ingested_at`, each non-null column outside the `_`
/// namespace an attribute, beside `contextful.table`, `contextful.run_id`, `contextful.commit_seq` and
/// `contextful.row_seq`.
// spec: run.export.log-record@37a18489
#[test]
fn each_row_becomes_one_log_record_carrying_its_commit_position() {
    let e = &parse_exports(BLOCK).unwrap()[0];
    let columns: Vec<String> = ["span_id", "duration_ms", "ok", "score", "tags", "note", "_ingested_at", "_run_id", "_row_seq", "_commit_seq", "_site_id"]
        .iter()
        .map(|c| c.to_string())
        .collect();
    let rows = vec![
        vec![json!("s1"), json!("1200"), json!(true), json!(0.5), json!("{\"a\":1}"), Value::Null, json!("2030-01-01T00:00:00.5Z"), json!("run-1"), json!("0"), json!("3"), json!("site")],
        vec![json!("s2"), json!("15"), json!(false), json!(1.0), Value::Null, json!("late"), json!("2030-01-01T00:00:01Z"), json!("run-2"), json!("4"), json!("9"), json!("site")],
    ];
    let types: BTreeMap<String, ColumnType> = [("duration_ms", ColumnType::Int64), ("score", ColumnType::Float64)].iter().map(|(c, t)| (c.to_string(), *t)).collect();
    let (body, last) = log_records(e, &columns, &rows, &types).unwrap();
    assert_eq!(last, Some(ExportCursor { commit_seq: 9, row_seq: 4 }));

    let resource = &body["resourceLogs"][0];
    assert_eq!(resource["resource"]["attributes"], json!([{"key": "service.name", "value": {"stringValue": "contextful"}}, {"key": "contextful.export", "value": {"stringValue": "spans-mirror"}}]));
    let records = resource["scopeLogs"][0]["logRecords"].as_array().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["timeUnixNano"], json!("1893456000500000000"));
    assert_eq!(
        records[0]["attributes"],
        json!([
            {"key": "span_id", "value": {"stringValue": "s1"}},
            {"key": "duration_ms", "value": {"intValue": "1200"}},
            {"key": "ok", "value": {"boolValue": true}},
            {"key": "score", "value": {"doubleValue": 0.5}},
            {"key": "tags", "value": {"stringValue": "{\"a\":1}"}},
            {"key": "contextful.table", "value": {"stringValue": "spans"}},
            {"key": "contextful.run_id", "value": {"stringValue": "run-1"}},
            {"key": "contextful.commit_seq", "value": {"intValue": "3"}},
            {"key": "contextful.row_seq", "value": {"intValue": "0"}},
        ])
    );
    // A null column is absent, and no `_` column other than the four provenance attributes appears.
    let keys: Vec<&str> = records[1]["attributes"].as_array().unwrap().iter().map(|a| a["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["span_id", "duration_ms", "ok", "score", "note", "contextful.table", "contextful.run_id", "contextful.commit_seq", "contextful.row_seq"]);

    // A row without its commit position is no row export can place past a cursor.
    let unplaced = log_records(e, &columns[..9], &[rows[0][..9].to_vec()], &types).unwrap_err();
    assert!(unplaced.to_string().contains("_commit_seq"), "{unplaced}");
    assert_eq!(log_records(e, &columns, &[], &types).unwrap().1, None);
}
