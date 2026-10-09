//! The decoders the sources share.

use contextful_decode::{decode, decode_with_encoding, Format};
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
