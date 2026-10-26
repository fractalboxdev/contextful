//! `run.export`: the `[[export]]` block, the batch statement over the commit sequence, and
//! the OTLP log record each landed row becomes.

use contextful_core::export::{change_events, change_state, log_records, parse_exports, typed_batch_len, Change, ChangeEvent, ExportCursor, ExportError, Signal, EXPORT_BATCH_ROWS, TYPED_REQUEST_BYTES};
use contextful_core::store::reconcile::ColumnType;
use serde_json::{json, Value};
use std::collections::BTreeMap;

const BLOCK: &str = "[[export]]\nname = \"spans-mirror\"\ntable = \"spans\"\nendpoint = \"https://otel.example.com/v1/logs\"\nsignal = \"logs\"\nheaders = { Authorization = \"Bearer ${secret://otel-token}\" }\n";

// spec: run.export.group-identity@9042f9d5
#[test]
fn publication_groups_bind_the_base_members_and_empty_outputs() {
    contextful_core::pipeline::model::check_blocks("contextful.toml", &toml::from_str("[[publication]]\nname='projection'\njob='producer'\nmembers=['copy']\n").unwrap()).unwrap();
    use contextful_core::export::publication::{publication, GroupChange, PublicationPlan};
    let plan = PublicationPlan {
        name: "projection".into(), incarnation: "store-one".into(), namespace: "epoch-one".into(),
        destination:"receiver".into(),read_identity:"reader".into(),
        base: None, order: 1, input: json!({"source":"snapshot-one"}),
        members: [("items".into(), "items".into()), ("empty".into(), "empty".into())].into(),
    };
    let states = [("items".into(), [("{\"id\":1}".into(), json!({"id":1}))].into()), ("empty".into(), BTreeMap::new())].into();
    let first = publication(plan.clone(), &BTreeMap::new(), &states).unwrap();
    assert_eq!(first.events.len(), 4);
    assert!(matches!(first.events[0].change, GroupChange::MemberComplete { changes:0 }));
    assert!(matches!(first.events.last().unwrap().change, GroupChange::GroupComplete));
    assert_eq!(first, publication(plan.clone(), &BTreeMap::new(), &states).unwrap());
    let mut changed = plan.clone();
    changed.base = Some("other-base".into());
    assert_ne!(first.group.id, publication(changed, &BTreeMap::new(), &states).unwrap().group.id);
    let mut changed = plan;
    changed.incarnation = "other-store".into();
    assert_ne!(first.group.id, publication(changed, &BTreeMap::new(), &states).unwrap().group.id);
    assert!(publication(first.group.plan.clone(), &BTreeMap::new(), &BTreeMap::new()).is_err());
    let mut unknown = serde_json::to_value(&first.events[0]).unwrap();
    unknown["unrecognized"] = json!("different bytes");
    assert!(serde_json::from_value::<contextful_core::export::publication::GroupEvent>(unknown).is_err());
}

// spec: run.export.typed-block@901de8c0
#[test]
fn a_typed_export_declares_a_key_and_schedule() {
    let block = "[[export]]\nname = \"state-copy\"\ntable = \"items\"\nendpoint = \"https://receiver.example.com/events\"\nformat = \"changes-v1\"\nkey = [\"id\"]\nschedule = \"every 30s\"\n";
    let exports = parse_exports(block).unwrap();
    assert_eq!(exports.len(), 1);
    assert_eq!(exports[0].name, "state-copy");
    assert_eq!(exports[0].signal, Signal::ChangesV1);
    assert_eq!(exports[0].key, ["id"]);
    assert!(exports[0].schedule.is_some());
    assert!(parse_exports(&block.replace("key = [\"id\"]", "key = []")).is_err());
    assert!(parse_exports(&block.replace("schedule = \"every 30s\"", "schedule = \"every 0s\"")).is_err());
}

// spec: run.export.typed-complete@cab79bfd
// spec: run.export.typed-events@871e84c3
// spec: run.export.typed-order@2eb57e17
#[test]
fn typed_changes_include_deletions_and_a_final_publication_marker() {
    let export = parse_exports("[[export]]\nname = \"copy\"\ntable = \"items\"\nendpoint = \"https://receiver.example.com/events\"\nformat = \"changes-v1\"\nkey = [\"id\"]\nschedule = \"every 30s\"\n").unwrap().remove(0);
    let before = change_state(&export, &[json!({"id":"b","title":"old"}), json!({"id":"a","title":"gone"})]).unwrap();
    let after = change_state(&export, &[json!({"id":"b","title":"new"}), json!({"id":"c","title":"added"})]).unwrap();
    let events = change_events(&export, "pub-2", 7, &before, &after).unwrap();
    assert_eq!(events.iter().map(|e| e.sequence).collect::<Vec<_>>(), [7, 8, 9, 10]);
    assert_eq!(events.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["copy:pub-2:7", "copy:pub-2:8", "copy:pub-2:9", "copy:pub-2:10"]);
    assert_eq!(events[0].change, Change::Delete { key: json!({"id":"a"}) });
    assert_eq!(events[1].change, Change::Upsert { key: json!({"id":"b"}), row: json!({"id":"b","title":"new"}) });
    assert_eq!(events[2].change, Change::Upsert { key: json!({"id":"c"}), row: json!({"id":"c","title":"added"}) });
    assert_eq!(events[3].change, Change::PublicationComplete { changes: 3 });
    assert_eq!(serde_json::to_value(&events[3]).unwrap()["version"], 1);
    assert!(change_state(&export, &[json!({"id":"a"}), json!({"id":"a"})]).is_err());
    assert!(change_state(&export, &[json!({"title":"missing"})]).is_err());
}

#[test]
fn typed_request_respects_its_byte_limit_and_refuses_a_single_oversized_event() {
    let event = |sequence, size| ChangeEvent {
        version: 1, id: format!("copy:pub:{sequence}"), publication: "pub".into(), sequence,
        table: "items".into(), change: Change::Upsert { key: json!({"id":sequence}), row: json!({"id":sequence,"body":"x".repeat(size)}) },
    };
    let events = vec![event(0, 40_000), event(1, 40_000)];
    assert_eq!(typed_batch_len(&events).unwrap(), 1);
    let bytes = serde_json::to_vec(&json!({"version":1,"events":&events[..1]})).unwrap();
    assert!(bytes.len() <= TYPED_REQUEST_BYTES);
    assert!(serde_json::to_vec(&json!({"version":1,"events":events})).unwrap().len() > TYPED_REQUEST_BYTES);
    assert!(matches!(typed_batch_len(&[event(2, TYPED_REQUEST_BYTES)]), Err(ExportError::ExportEventTooLarge(_))));
}

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

/// If `signal` is not `logs`, the exporter raises `ExportSignalUnknown` before reading any row.
// spec: run.export.signal-unknown@b7256307
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
    let types: BTreeMap<String, ColumnType> = [("duration_ms", ColumnType::Int64), ("score", ColumnType::Float64)].iter().map(|(c, t)| (c.to_string(), t.clone())).collect();
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

fn group_plan(input: Value) -> contextful_core::export::publication::PublicationPlan {
    contextful_core::export::publication::PublicationPlan {
        name: "projection".into(), incarnation: "store-one".into(), namespace: "epoch-one".into(),
        destination: "receiver".into(), read_identity: "reader".into(), base: None, order: 1, input,
        members: [("items".into(), "items".into())].into(),
    }
}

fn group_states(rows: &[Value]) -> contextful_core::export::publication::GroupState {
    [("items".into(), rows.iter().map(|row| (json!({"id": row["id"]}).to_string(), row.clone())).collect())].into()
}

/// A checkpoint holding `pending` exactly as an earlier sender staged it, `offset` events acknowledged.
fn sender_with(pending: &contextful_core::export::publication::GroupBatch, next: &contextful_core::export::publication::GroupState, offset: usize) -> contextful_core::export::publication::GroupSender {
    let bytes = serde_json::to_vec(&json!([1, "projection", "epoch-one", {
        "identity": "reader", "incarnation": "store-one", "active": null, "state": {},
        "pending": pending, "next_state": next, "offset": offset,
    }])).unwrap();
    contextful_core::export::publication::GroupSender::restore(&bytes, "projection", "epoch-one", "store-one", "reader", None).unwrap()
}

#[test]
fn a_group_header_carries_a_digest_of_any_producer_input() {
    use contextful_core::export::publication::{GroupSender, SourcePublication};
    let runs: Vec<String> = (0..5000).map(|i| format!("run-{i:032}")).collect();
    let source = SourcePublication {
        run: "build-1".into(),
        input: json!({"at": null, "inputs": {"events": {"snapshot_id": "snapshot-one", "runs": runs}}}),
        frontiers: [("items".into(), "frontier-one".into())].into(),
    };
    assert!(serde_json::to_vec(&source).unwrap().len() > TYPED_REQUEST_BYTES);
    let digest = source.digest().unwrap();
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    let mut advanced = source.clone();
    advanced.frontiers.insert("items".into(), "frontier-two".into());
    assert_ne!(digest, advanced.digest().unwrap());
    let mut sender = GroupSender::default();
    assert!(sender.stage(&group_plan(json!(digest)), &group_states(&[json!({"id": 1})]), "reader").unwrap());
    let batch = sender.batch().unwrap().unwrap();
    assert_eq!(batch.events.len(), 3);
    assert_eq!(batch.group.plan.input, json!(digest));
    assert!(serde_json::to_vec(&batch).unwrap().len() < 1024);
}

// spec: run.export.group-request-bound@44480894
#[test]
fn staging_refuses_a_group_whose_requests_exceed_the_ceiling() {
    use contextful_core::export::publication::GroupSender;
    let mut sender = GroupSender::default();
    let oversized = json!("x".repeat(TYPED_REQUEST_BYTES));
    let refused = sender.stage(&group_plan(oversized), &group_states(&[json!({"id": 1})]), "reader").unwrap_err();
    assert!(matches!(refused, ExportError::PublicationHeaderTooLarge(_)), "{refused}");
    assert_eq!(sender, GroupSender::default());
    let row = json!({"id": 1, "body": "y".repeat(TYPED_REQUEST_BYTES)});
    let refused = sender.stage(&group_plan(json!("digest")), &group_states(&[row]), "reader").unwrap_err();
    assert!(matches!(refused, ExportError::ExportEventTooLarge(_)), "{refused}");
    assert_eq!(sender, GroupSender::default());
}

// spec: run.export.group-stranded@9d45f5f1
#[test]
fn a_never_offered_stranded_group_yields_to_the_next_stage() {
    use contextful_core::export::publication::publication;
    let states = group_states(&[json!({"id": 1})]);
    let stranded = publication(group_plan(json!({"runs": "x".repeat(TYPED_REQUEST_BYTES)})), &BTreeMap::new(), &states).unwrap();
    let mut sender = sender_with(&stranded, &states, 0);
    assert!(matches!(sender.batch(), Err(ExportError::ExportEventTooLarge(_))));
    assert_eq!(sender.pending_identity(), None);
    assert!(sender.stage(&group_plan(json!("digest")), &states, "reader").unwrap());
    let batch = sender.batch().unwrap().unwrap();
    assert_ne!(batch.group.id, stranded.group.id);
    assert!(sender.acknowledge(&batch.group.id, batch.events.last().unwrap().sequence).unwrap());

    // A group with an acknowledged prefix has reached the receiver and stays immutable.
    let rows = group_states(&[json!({"id": 1}), json!({"id": 2, "body": "y".repeat(TYPED_REQUEST_BYTES)})]);
    let partial = publication(group_plan(json!("digest")), &BTreeMap::new(), &rows).unwrap();
    let mut sender = sender_with(&partial, &rows, 1);
    assert_eq!(sender.pending_identity(), Some("reader"));
    assert!(!sender.stage(&group_plan(json!("later")), &rows, "reader").unwrap());
    assert!(matches!(sender.batch(), Err(ExportError::ExportEventTooLarge(_))));
}

#[test]
fn a_sender_resumes_after_an_acknowledged_sequence_whatever_its_batch_boundaries() {
    use contextful_core::export::publication::publication;
    let rows: Vec<Value> = (0..1200).map(|id| json!({"id": id})).collect();
    let states = group_states(&rows);
    let pending = publication(group_plan(json!("digest")), &BTreeMap::new(), &states).unwrap();
    let group = pending.group.id.clone();
    let sender = sender_with(&pending, &states, 0);
    let first = sender.batch().unwrap().unwrap();
    let through = first.events.last().unwrap().sequence;

    // A sequence inside this binary's first batch, as a binary cutting shorter batches acknowledged.
    let mut resumed = sender.clone();
    resumed.resume(&group, through - 7).unwrap();
    let next = resumed.batch().unwrap().unwrap();
    assert_eq!(next.events[0].sequence, through - 6);
    assert!(!resumed.acknowledge(&group, next.events.last().unwrap().sequence).unwrap());

    // Another group, an unknown sequence or the completing one refuses.
    assert!(sender.clone().resume("another", through).is_err());
    assert!(sender.clone().resume(&group, u64::MAX).is_err());
    assert!(sender.clone().resume(&group, pending.events.last().unwrap().sequence).is_err());

    // A sequence already behind the outbox's own offset refuses; one past it advances.
    let mut advanced = sender_with(&pending, &states, first.events.len());
    assert!(advanced.clone().resume(&group, through).is_err());
    advanced.resume(&group, through + 1).unwrap();
    assert_eq!(advanced.batch().unwrap().unwrap().events[0].sequence, through + 2);
}
