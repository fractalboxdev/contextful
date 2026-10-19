//! The decoders the sources share.

use contextful_core::run::Fault;
use contextful_decode::{decode, decode_delimited, decode_with_encoding, Delimited, Format};
use serde_json::{json, Value};

/// A delimited source lands every cell as a string and an empty unquoted field as null.
// spec: connector.source.delimited-cell@9812b915
#[test]
fn delimited_cells_land_as_strings_and_empty_unquoted_fields_as_null() {
    let body = b"id,amount,note,flag\n7,12.50,\"a, b\",true\n8,,\"\",\n";
    let (rows, _) = decode(Format::Csv, body, None, "orders.csv").unwrap();
    assert_eq!(Value::Object(rows[0].clone()), json!({"id": "7", "amount": "12.50", "note": "a, b", "flag": "true"}));
    assert_eq!(Value::Object(rows[1].clone()), json!({"id": "8", "amount": null, "note": "", "flag": null}));
}

#[test]
fn json_lines_and_json_decode_to_records_and_refuse_whole_on_a_bad_record() {
    let (rows, _) = decode(Format::Jsonl, b"{\"id\":1}\n\n{\"id\":2}\n", None, "x").unwrap();
    assert_eq!(rows.len(), 2);
    let f = decode(Format::Jsonl, b"{\"id\":1}\n[2]\n", None, "feed.jsonl").unwrap_err();
    assert!(f.message.contains("feed.jsonl") && f.message.contains("line 2"), "{f}");
    let f = decode(Format::Json, b"{\"data\":{}}", Some("/data"), "api").unwrap_err();
    assert!(f.message.starts_with("PipelineUnreadableInput"), "{f}");
    let f = decode(Format::Csv, b"a,b\n1,2,3\n", None, "wide.csv").unwrap_err();
    assert!(f.message.contains("line 2"), "{f}");
}

#[test]
fn a_quoted_field_may_span_lines_and_a_byte_order_mark_is_no_part_of_a_name() {
    let body = "\u{feff}id,note\r\n1,\"first line\r\nsecond, with \"\"quotes\"\"\"\r\n2,plain\r\n".as_bytes();
    let (rows, _) = decode(Format::Csv, body, None, "notes.csv").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(Value::Object(rows[0].clone()), json!({"id": "1", "note": "first line\r\nsecond, with \"quotes\""}));
    assert_eq!(Value::Object(rows[1].clone()), json!({"id": "2", "note": "plain"}));
    let f = decode(Format::Csv, b"id,note\n1,\"never closed\n2,x\n", None, "broken.csv").unwrap_err();
    assert!(f.message.contains("broken.csv") && f.message.contains("line 2"), "{f}");
}

#[test]
fn a_declared_csv_encoding_preserves_text_and_refuses_invalid_input() {
    let (rows, _) = decode_with_encoding(Format::Csv, b"name\ncaf\xe9\n", None, "names.csv", Some("windows-1252")).unwrap();
    assert_eq!(rows[0]["name"], "café");
    let failure = decode_with_encoding(Format::Csv, b"name\n\x81\n", None, "names.csv", Some("shift_jis")).unwrap_err();
    assert!(failure.message.contains("ConnectorEncodingInvalid"), "{failure}");
    assert_eq!(failure.fault, Some(Fault::Input), "bytes invalid under the declared encoding are the input's fault");
    let unknown = decode_with_encoding(Format::Csv, b"name\n", None, "names.csv", Some("no-such-encoding")).unwrap_err();
    assert_eq!(unknown.fault, Some(Fault::Configuration), "an unknown declared encoding is the declaration's fault: {unknown}");
}


#[test]
fn json_object_selects_one_object_and_retains_the_pagination_root() {
    let format = Format::parse("json-object").unwrap();
    assert_eq!(format.name(), "json-object");
    for (body, pointer, expected) in [
        (json!({"items":[]}), None, json!({"items":[]})),
        (json!({}), None, json!({})),
        (json!({"data":{"items":[]},"next":"second"}), Some("/data"), json!({"items":[]})),
    ] {
        let (rows, root) = decode(format, &serde_json::to_vec(&body).unwrap(), pointer, "snapshot").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(Value::Object(rows[0].clone()), expected);
        assert_eq!(root, Some(body));
    }
}

#[test]
fn json_object_refuses_nonobjects_and_bad_input_without_relaxing_json_arrays() {
    let format = Format::parse("json-object").unwrap();
    for (body, pointer) in [
        ("[]", None), ("[{}]", None), ("null", None), ("7", None), ("true", None),
        ("\"text\"", None), ("{", None), ("{}", Some("/missing")),
        ("{\"data\":[]}", Some("/data")), ("{\"data\":null}", Some("/data")),
    ] {
        let failure = decode(format, body.as_bytes(), pointer, "snapshot").unwrap_err();
        assert!(failure.deterministic && failure.message.contains("PipelineUnreadableInput") && failure.message.contains("snapshot"), "{failure}");
    }
    let array = Format::parse("json").unwrap();
    assert!(decode(array, b"{}", None, "snapshot").is_err());
    assert!(decode(array, b"[]", None, "snapshot").unwrap().0.is_empty());
    assert_eq!(decode(array, b"[{\"id\":1}]", None, "snapshot").unwrap().0.len(), 1);
}

/// `delimiter` names the one field delimiter: a tab splits fields, and a quoted field holds it.
#[test]
fn a_declared_tab_delimiter_splits_fields_and_a_quoted_field_holds_one() {
    let tab = Delimited { delimiter: '\t', columns: None };
    let (rows, _) = decode_delimited(Format::Csv, b"id\tnote\n1\ta,b\n2\t\"x\ty\"\n", None, "tabs.tsv", None, &tab).unwrap();
    assert_eq!(Value::Object(rows[0].clone()), json!({"id": "1", "note": "a,b"}));
    assert_eq!(Value::Object(rows[1].clone()), json!({"id": "2", "note": "x\ty"}));
    assert_eq!(Delimited::default().delimiter, ',');
}

/// `columns` names a headerless body's columns in order. A first record whose field count differs from the list is a
/// configuration fault, and a later one unreadable input, each naming its line.
// spec: connector.source.declared-columns@1b6aaa73
#[test]
fn declared_columns_name_a_headerless_body_and_a_count_mismatch_refuses_naming_the_line() {
    let declared = Delimited { delimiter: '\t', columns: Some(vec!["id".into(), "day".into(), "actor".into()]) };
    let (rows, _) = decode_delimited(Format::Csv, b"1\t20250101\tUSA\n2\t20250102\t\n", None, "events.CSV", None, &declared).unwrap();
    assert_eq!(rows.len(), 2, "the first line is a record, not a header");
    assert_eq!(Value::Object(rows[0].clone()), json!({"id": "1", "day": "20250101", "actor": "USA"}));
    assert_eq!(Value::Object(rows[1].clone()), json!({"id": "2", "day": "20250102", "actor": null}));
    for (body, line, fault) in [(&b"1\t2\t3\n4\t5\n"[..], "line 2", Fault::Input), (&b"1\t2\t3\t4\n"[..], "line 1", Fault::Configuration)] {
        let f = decode_delimited(Format::Csv, body, None, "events.CSV", None, &declared).unwrap_err();
        assert!(f.message.starts_with("PipelineUnreadableInput") && f.message.contains("events.CSV") && f.message.contains(line), "{f}");
        assert_eq!(f.fault, Some(fault), "{f}");
    }
}
