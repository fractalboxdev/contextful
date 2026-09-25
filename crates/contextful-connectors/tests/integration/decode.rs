//! The decoders the sources share.

use contextful_connectors::decode::{decode, Format};
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
