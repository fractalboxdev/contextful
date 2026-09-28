//! A workbook served over HTTP: the source's refusals around the workbook decoder.

use crate::support::{request, source, Never, Response, Server};
use contextful_connectors::http::{ConfigError, HttpConfig};
use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::Source;
use serde_json::{json, Value};
use std::io::Write;

const SS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// A one-sheet workbook named `Data` holding `sheet` rows over `shared` strings.
fn book(sheet: &str, shared: &[&str]) -> Vec<u8> {
    let wb = format!(r#"<?xml version="1.0"?><workbook xmlns="{SS}" xmlns:r="{REL_NS}"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>"#);
    let rels = format!(
        r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL_NS}/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#
    );
    let ws = format!(r#"<?xml version="1.0"?><worksheet xmlns="{SS}"><sheetData>{sheet}</sheetData></worksheet>"#);
    let list: String = shared.iter().map(|s| format!("<si><t>{s}</t></si>")).collect();
    let ss = format!(r#"<?xml version="1.0"?><sst xmlns="{SS}">{list}</sst>"#);
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in [("xl/workbook.xml", wb), ("xl/_rels/workbook.xml.rels", rels), ("xl/worksheets/sheet1.xml", ws), ("xl/sharedStrings.xml", ss)] {
        w.start_file(name, options).unwrap();
        w.write_all(bytes.as_bytes()).unwrap();
    }
    w.finish().unwrap().into_inner()
}

/// An incremental position against a workbook raises `ConnectorIncrementalUnsupported`.
// spec: connector.source.workbook-incremental@d265a2e0
#[test]
fn an_incremental_position_against_a_workbook_is_refused() {
    let bytes = book(r#"<row r="1"><c r="A1" t="s"><v>0</v></c></row><row r="2"><c r="A2"><v>1</v></c></row>"#, &["id"]);
    let server = Server::start(move |_| Response { status: 200, headers: vec![], body: bytes.clone() });
    let mut s = source(json!({"endpoint": server.url("/book.xlsx"), "format": "xlsx"}), vec![]);
    let pulled: Value = serde_json::from_slice(&s.pull(&request(None), &Never).unwrap()).unwrap();
    assert_eq!(pulled["rows"], json!([{"id": "1"}]));

    let f = s.pull(&request(Some(json!({"field": "id", "at": "1"}))), &Never).unwrap_err();
    assert!(f.message.starts_with("ConnectorIncrementalUnsupported"), "{f}");
    assert_eq!(server.received("/book.xlsx").len(), 1, "the refusal precedes the request");

    match HttpConfig::parse(&json!({"endpoint": "https://example.test/b.xlsx", "format": "xlsx", "since_param": "since"})) {
        Err(ConfigError::Connector(ConnectorError::ConnectorIncrementalUnsupported(m))) => assert!(m.contains("since_param"), "{m}"),
        other => panic!("expected ConnectorIncrementalUnsupported, got {other:?}"),
    }
}

#[test]
fn a_workbook_key_on_another_format_is_refused_at_build() {
    for (key, value) in [("sheet", json!("Data")), ("skip_rows", json!(2))] {
        match HttpConfig::parse(&json!({"endpoint": "https://example.test/x.csv", "format": "csv", key: value})) {
            Err(ConfigError::Connector(ConnectorError::ConnectorFormatKeyRejected(m))) => assert!(m.contains(key) && m.contains("csv"), "{m}"),
            other => panic!("expected ConnectorFormatKeyRejected for `{key}`, got {other:?}"),
        }
    }
    let c = HttpConfig::parse(&json!({"endpoint": "https://example.test/b.xlsx", "format": "xlsx", "sheet": "Data", "skip_rows": 2})).unwrap();
    assert_eq!((c.sheet.as_deref(), c.skip_rows), (Some("Data"), 2));
}
