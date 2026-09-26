//! The workbook decoder: one worksheet of an Office Open XML archive under its header row.

use crate::support::{request, source, Never, Response, Server};
use contextful_connectors::decode::workbook::{rows, ROW_CAP};
use contextful_connectors::decode::{decode, Format};
use contextful_connectors::http::{ConfigError, HttpConfig};
use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::Source;
use serde_json::{json, Value};
use std::io::Write;

const SS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// Deflate `(name, bytes)` entries into one archive.
fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in entries {
        w.start_file(*name, options).unwrap();
        w.write_all(bytes).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn workbook_xml(sheets: &[&str], extra: &str) -> String {
    let list: String = sheets.iter().enumerate().map(|(i, n)| format!(r#"<sheet name="{n}" sheetId="{}" r:id="rId{}"/>"#, i + 1, i + 1)).collect();
    format!(r#"<?xml version="1.0"?><workbook xmlns="{SS}" xmlns:r="{REL_NS}"><sheets>{list}</sheets>{extra}</workbook>"#)
}

fn rels_xml(count: usize, extra: &str) -> String {
    let list: String = (1..=count).map(|i| format!(r#"<Relationship Id="rId{i}" Type="{REL_NS}/worksheet" Target="worksheets/sheet{i}.xml"/>"#)).collect();
    format!(r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{list}{extra}</Relationships>"#)
}

fn sheet_xml(rows: &str) -> String {
    format!(r#"<?xml version="1.0"?><worksheet xmlns="{SS}"><sheetData>{rows}</sheetData></worksheet>"#)
}

fn shared_xml(strings: &[&str]) -> String {
    let list: String = strings.iter().map(|s| format!("<si><t>{s}</t></si>")).collect();
    format!(r#"<?xml version="1.0"?><sst xmlns="{SS}">{list}</sst>"#)
}

/// A one-sheet workbook over `sheet` rows, with `shared` strings and any further entries.
fn book(sheet: &str, shared: &[&str], extra: &[(&str, &[u8])]) -> Vec<u8> {
    let (wb, rels, ws, ss) = (workbook_xml(&["Data"], ""), rels_xml(1, ""), sheet_xml(sheet), shared_xml(shared));
    let mut entries: Vec<(&str, &[u8])> =
        vec![("xl/workbook.xml", wb.as_bytes()), ("xl/_rels/workbook.xml.rels", rels.as_bytes()), ("xl/worksheets/sheet1.xml", ws.as_bytes()), ("xl/sharedStrings.xml", ss.as_bytes())];
    entries.extend_from_slice(extra);
    zip_of(&entries)
}

fn landed(bytes: &[u8]) -> Vec<Value> {
    rows(bytes, None, 0, "book.xlsx").unwrap().into_iter().map(Value::Object).collect()
}

fn refusal(bytes: &[u8]) -> String {
    rows(bytes, None, 0, "book.xlsx").unwrap_err().message
}

/// A workbook cell lands as a string and a date cell as its serial number. No formula is
/// evaluated; a formula cell lands its cached value.
// spec: connector.source.workbook-cell-typing@a2c68944
#[test]
fn cells_land_as_strings_a_date_as_its_serial_and_a_formula_as_its_cached_value() {
    let sheet = r#"
        <row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c><c r="C1" t="s"><v>2</v></c><c r="D1" t="s"><v>3</v></c><c r="E1" t="inlineStr"><is><t>flag</t></is></c></row>
        <row r="2"><c r="A2"><v>7</v></c><c r="B2" s="1"><v>45292</v></c><c r="C2"><f>A2*2</f><v>14</v></c><c r="D2" t="s"><v>4</v></c><c r="E2" t="b"><v>1</v></c></row>
        <row r="3"><c r="A3"><v>8.5</v></c><c r="C3"/><c r="E3" t="b"><v>0</v></c></row>"#;
    let bytes = book(sheet, &["id", "opened", "double", "note", "a &amp; b"], &[]);
    assert_eq!(
        landed(&bytes),
        vec![
            json!({"id": "7", "opened": "45292", "double": "14", "note": "a & b", "flag": "true"}),
            json!({"id": "8.5", "double": null, "flag": "false"}),
        ]
    );
    let (via_decode, _) = decode(Format::parse("xlsx").unwrap(), &bytes, None, "book.xlsx").unwrap();
    assert_eq!(via_decode.len(), 2);
}

#[test]
fn a_named_sheet_and_a_preamble_select_the_header_row() {
    let (wb, rels) = (workbook_xml(&["Notes", "Series"], ""), rels_xml(2, ""));
    let notes = sheet_xml(r#"<row r="1"><c r="A1" t="inlineStr"><is><t>n</t></is></c></row>"#);
    let series = sheet_xml(
        r#"<row r="1"><c r="A1" t="inlineStr"><is><t>Published by an agency</t></is></c></row>
           <row r="3"><c r="A3" t="inlineStr"><is><t>year</t></is></c><c r="B3" t="inlineStr"><is><t>value</t></is></c></row>
           <row r="4"><c r="A4"><v>2024</v></c><c r="B4"><v>3.1</v></c></row>"#,
    );
    let bytes = zip_of(&[
        ("xl/workbook.xml", wb.as_bytes()),
        ("xl/_rels/workbook.xml.rels", rels.as_bytes()),
        ("xl/worksheets/sheet1.xml", notes.as_bytes()),
        ("xl/worksheets/sheet2.xml", series.as_bytes()),
    ]);
    let got: Vec<Value> = rows(&bytes, Some("Series"), 1, "series.xlsx").unwrap().into_iter().map(Value::Object).collect();
    assert_eq!(got, vec![json!({"year": "2024", "value": "3.1"})]);
    let f = rows(&bytes, Some("Missing"), 0, "series.xlsx").unwrap_err();
    assert!(f.message.starts_with("PipelineUnreadableInput") && f.message.contains("Notes, Series"), "{f}");
}

/// A cell past the header's width raises `ConnectorCellOutOfRange`.
// spec: connector.source.cell-out-of-range@d228a88c
#[test]
fn a_cell_past_the_header_width_is_refused_rather_than_dropped() {
    let sheet = r#"<row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row>
                   <row r="2"><c r="A2"><v>1</v></c><c r="D2"><v>9</v></c></row>"#;
    let m = refusal(&book(sheet, &["a", "b"], &[]));
    assert!(m.starts_with("ConnectorCellOutOfRange") && m.contains("D2") && m.contains("2 column"), "{m}");
}

/// An external-reference declaration, an external-links part, or a relationship marked
/// external raises `ConnectorExternalReference`.
// spec: connector.source.external-reference@2d61aa05
#[test]
fn every_form_of_external_reference_is_refused() {
    let (ws, ss) = (sheet_xml(r#"<row r="1"><c r="A1" t="s"><v>0</v></c></row>"#), shared_xml(&["a"]));
    let declared = workbook_xml(&["Data"], r#"<externalReferences><externalReference r:id="rId9"/></externalReferences>"#);
    let external_rel = rels_xml(1, &format!(r#"<Relationship Id="rId9" Type="{REL_NS}/hyperlink" Target="https://example.test/other.xlsx" TargetMode="External"/>"#));
    let (wb, rels) = (workbook_xml(&["Data"], ""), rels_xml(1, ""));
    type Entries<'a> = Vec<(&'a str, &'a [u8])>;
    let cases: [(&str, Entries); 3] = [
        ("declaration", vec![("xl/workbook.xml", declared.as_bytes()), ("xl/_rels/workbook.xml.rels", rels.as_bytes())]),
        ("part", vec![("xl/workbook.xml", wb.as_bytes()), ("xl/_rels/workbook.xml.rels", rels.as_bytes()), ("xl/externalLinks/externalLink1.xml", b"<externalLink/>")]),
        ("relationship", vec![("xl/workbook.xml", wb.as_bytes()), ("xl/_rels/workbook.xml.rels", external_rel.as_bytes())]),
    ];
    for (case, mut entries) in cases {
        entries.push(("xl/worksheets/sheet1.xml", ws.as_bytes()));
        entries.push(("xl/sharedStrings.xml", ss.as_bytes()));
        let m = refusal(&zip_of(&entries));
        assert!(m.starts_with("ConnectorExternalReference"), "{case}: {m}");
    }
}

/// An office container is read by exact part name. No other part is opened and no
/// external entity is resolved.
// spec: connector.source.office-part-selection@0c92c165
#[test]
fn only_the_named_parts_open_and_an_entity_stays_literal() {
    // A part past the whole inflate budget refuses only if it is opened.
    let drawing = vec![b' '; 65 * 1024 * 1024];
    let sheet = r#"<row r="1"><c r="A1" t="s"><v>0</v></c></row><row r="2"><c r="A2" t="s"><v>1</v></c></row>"#;
    let bytes = book(sheet, &["note", "&xxe;"], &[("xl/drawings/drawing1.xml", &drawing), ("xl/styles.xml", b"<not-xml")]);
    assert_eq!(landed(&bytes), vec![json!({"note": "&xxe;"})]);

    let doctype = format!(
        r#"<?xml version="1.0"?><!DOCTYPE sst [<!ENTITY xxe SYSTEM "file:///etc/hostname">]><sst xmlns="{SS}"><si><t>note</t></si><si><t>&xxe;</t></si></sst>"#
    );
    let (wb, rels, ws) = (workbook_xml(&["Data"], ""), rels_xml(1, ""), sheet_xml(sheet));
    let bytes = zip_of(&[
        ("xl/workbook.xml", wb.as_bytes()),
        ("xl/_rels/workbook.xml.rels", rels.as_bytes()),
        ("xl/worksheets/sheet1.xml", ws.as_bytes()),
        ("xl/sharedStrings.xml", doctype.as_bytes()),
    ]);
    assert_eq!(landed(&bytes), vec![json!({"note": "&xxe;"})]);
}

/// Rewrite every recorded uncompressed size of the archive's entries to `size`, in the
/// local headers and the central directory alike.
fn misreport_sizes(mut bytes: Vec<u8>, size: u32) -> Vec<u8> {
    let mut i = 0;
    while i + 4 <= bytes.len() {
        let at = match &bytes[i..i + 4] {
            [0x50, 0x4b, 0x03, 0x04] => Some(i + 22),
            [0x50, 0x4b, 0x01, 0x02] => Some(i + 24),
            _ => None,
        };
        if let Some(at) = at {
            bytes[at..at + 4].copy_from_slice(&size.to_le_bytes());
        }
        i += 1;
    }
    bytes
}

/// A multi-part office read decompresses at most 64 MiB in total, judged against the
/// archive directory's claim and against the bytes that arrive.
// spec: connector.source.decompression-budget@274f558d
#[test]
fn an_office_read_decompresses_at_most_64_mib_by_claim_and_by_arrival() {
    let pad = |n: usize| {
        let mut s = shared_xml(&["a"]).into_bytes();
        s.extend(std::iter::repeat_n(b' ', n));
        s
    };
    let sheet = r#"<row r="1"><c r="A1" t="s"><v>0</v></c></row>"#;
    // Parts summing to exactly the budget land.
    let wb = workbook_xml(&["Data"], "");
    let rels = rels_xml(1, "");
    let ws = sheet_xml(sheet);
    let fixed = wb.len() + rels.len() + ws.len() + shared_xml(&["a"]).len();
    let at_budget = pad(64 * 1024 * 1024 - fixed);
    let entries = |shared: &[u8]| {
        zip_of(&[
            ("xl/workbook.xml", wb.as_bytes()),
            ("xl/_rels/workbook.xml.rels", rels.as_bytes()),
            ("xl/sharedStrings.xml", shared),
            ("xl/worksheets/sheet1.xml", ws.as_bytes()),
        ])
    };
    assert!(rows(&entries(&at_budget), None, 0, "book.xlsx").is_ok());
    // One byte more refuses on the directory's claim, before the part inflates.
    let m = refusal(&entries(&pad(64 * 1024 * 1024 - fixed + 1)));
    assert!(m.contains("declares") && m.contains("67108864"), "{m}");
    // An archive claiming small sizes refuses on the bytes that arrive.
    let m = refusal(&misreport_sizes(entries(&pad(65 * 1024 * 1024)), 100));
    assert!(m.contains("inflates past") && m.contains("67108864"), "{m}");
}

/// One worksheet lands at most 64 MiB of resolved cell text, counted as cells land, and
/// at most 1048576 rows.
// spec: connector.source.worksheet-landing@a1a4c97a
#[test]
fn a_worksheet_lands_at_most_64_mib_of_resolved_text_and_1048576_rows() {
    // One 1 MiB shared string referenced by 70 cells resolves to 70 MiB from a small part.
    let big = "x".repeat(1024 * 1024);
    let mut sheet = String::from(r#"<row r="1"><c r="A1" t="inlineStr"><is><t>v</t></is></c></row>"#);
    for r in 2..72 {
        sheet.push_str(&format!(r#"<row r="{r}"><c r="A{r}" t="s"><v>0</v></c></row>"#));
    }
    let m = refusal(&book(&sheet, &[&big], &[]));
    assert!(m.contains("67108864 bytes of cell text"), "{m}");

    let header = r#"<row r="1"><c r="A1" t="inlineStr"><is><t>v</t></is></c></row>"#;
    let at_cap = format!("{header}{}", "<row></row>".repeat(ROW_CAP));
    assert_eq!(rows(&book(&at_cap, &[], &[]), None, 0, "book.xlsx").unwrap().len(), ROW_CAP);
    let past_cap = format!("{header}{}", "<row></row>".repeat(ROW_CAP + 1));
    let m = refusal(&book(&past_cap, &[], &[]));
    assert!(m.contains("more than 1048576 rows"), "{m}");
}

/// An incremental position against a workbook raises `ConnectorIncrementalUnsupported`.
// spec: connector.source.workbook-incremental@d265a2e0
#[test]
fn an_incremental_position_against_a_workbook_is_refused() {
    let bytes = book(r#"<row r="1"><c r="A1" t="s"><v>0</v></c></row><row r="2"><c r="A2"><v>1</v></c></row>"#, &["id"], &[]);
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
