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
