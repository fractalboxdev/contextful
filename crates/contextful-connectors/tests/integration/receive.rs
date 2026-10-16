//! The `receive` source: its declared ceilings and the delivered body's decode.

use contextful_connectors::receive::{decode, ReceiveConfig, NAME};
use serde_json::json;

/// A `receive` source declares `format = "jsonl"`, a positive `max_batch_bytes`, a positive `max_queue_depth` and a
/// positive `key_retention_secs`, none defaulted.
// spec: connector.source.receive-form@ddcc77a1
#[test]
fn a_receive_source_declares_both_ceilings_and_the_jsonl_format() {
    assert_eq!(NAME, "receive");
    assert!(contextful_connectors::BUILT_IN.contains(&NAME));
    let config = ReceiveConfig::parse(&json!({ "format": "jsonl", "max_batch_bytes": 4096, "max_queue_depth": 2, "key_retention_secs": 600 })).unwrap();
    assert_eq!((config.max_batch_bytes, config.max_queue_depth, config.key_retention_secs), (4096, 2, 600));
    for (config, named) in [
        (json!({ "format": "jsonl", "max_batch_bytes": 4096, "key_retention_secs": 600 }), "max_queue_depth"),
        (json!({ "format": "jsonl", "max_queue_depth": 2, "key_retention_secs": 600 }), "max_batch_bytes"),
        (json!({ "format": "jsonl", "max_batch_bytes": 4096, "max_queue_depth": 2 }), "key_retention_secs"),
        (json!({ "format": "jsonl", "max_batch_bytes": 0, "max_queue_depth": 2, "key_retention_secs": 600 }), "max_batch_bytes"),
        (json!({ "format": "jsonl", "max_batch_bytes": 4096, "max_queue_depth": 0, "key_retention_secs": 600 }), "max_queue_depth"),
        (json!({ "format": "jsonl", "max_batch_bytes": 4096, "max_queue_depth": 2, "key_retention_secs": 0 }), "key_retention_secs"),
        (json!({ "max_batch_bytes": 4096, "max_queue_depth": 2, "key_retention_secs": 600 }), "format"),
        (json!({ "format": "csv", "max_batch_bytes": 4096, "max_queue_depth": 2, "key_retention_secs": 600 }), "format"),
    ] {
        let e = ReceiveConfig::parse(&config).unwrap_err().to_string();
        assert!(e.contains(named), "{config}: {e}");
    }
    let unknown = ReceiveConfig::parse(&json!({ "format": "jsonl", "max_batch_bytes": 1, "max_queue_depth": 1, "key_retention_secs": 1, "endpoint": "x" })).unwrap_err();
    assert!(unknown.to_string().starts_with("PipelineUnknownConfigKey"), "{unknown}");
}

/// A body holds one JSON object per line, `table` naming a declared table and `row` its row object; another shape
/// fails the whole batch as unreadable input, naming the line.
// spec: connector.source.receive-body@0ce22373
#[test]
fn a_receive_body_splits_by_declared_table_and_fails_whole_on_a_bad_line() {
    let body = b"{\"table\":\"spans\",\"row\":{\"id\":\"s1\"}}\n{\"table\":\"events\",\"row\":{\"id\":\"e1\"}}\n\n{\"table\":\"spans\",\"row\":{\"id\":\"s2\"}}\n";
    let split = decode(body, &["spans", "events"]).unwrap();
    assert_eq!(serde_json::to_value(&split).unwrap(), json!({ "events": [{ "id": "e1" }], "spans": [{ "id": "s1" }, { "id": "s2" }] }));
    for bad in [
        &b"{\"table\":\"spans\",\"row\":{\"id\":\"s1\"}}\n{\"table\":\"logs\",\"row\":{\"id\":\"l1\"}}\n"[..],
        b"{\"table\":\"spans\",\"row\":{\"id\":\"s1\"}}\n{\"table\":\"spans\",\"row\":[1]}\n",
        b"{\"table\":\"spans\",\"row\":{\"id\":\"s1\"}}\nnot json\n",
        b"{\"table\":\"spans\",\"row\":{\"id\":\"s1\"}}\n{\"row\":{\"id\":\"s1\"}}\n",
    ] {
        let e = decode(bad, &["spans", "events"]).unwrap_err().to_string();
        assert!(e.starts_with("PipelineUnreadableInput") && e.contains("line 2"), "{e}");
    }
}
