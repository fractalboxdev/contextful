//! The workbook decoder: one worksheet of an Office Open XML archive under its header row.

use contextful_decode::workbook::{rows, ROW_CAP};
use contextful_decode::{decode, Format};
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
    let got: Vec<Value> = rows(&bytes, Some("Series"), 2, "series.xlsx").unwrap().into_iter().map(Value::Object).collect();
    assert_eq!(got, vec![json!({"year": "2024", "value": "3.1"})]);
    let f = rows(&bytes, Some("Missing"), 0, "series.xlsx").unwrap_err();
    assert!(f.message.starts_with("PipelineUnreadableInput") && f.message.contains("Notes, Series"), "{f}");
}

/// A cell holding a value past the header's width raises `ConnectorCellOutOfRange`; a cell
/// holding none widens nothing.
// spec: connector.source.cell-out-of-range@8ee3b542
#[test]
fn a_cell_past_the_header_width_is_refused_rather_than_dropped() {
    let sheet = r#"<row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row>
                   <row r="2"><c r="A2"><v>1</v></c><c r="D2"><v>9</v></c></row>"#;
    let m = refusal(&book(sheet, &["a", "b"], &[]));
    assert!(m.starts_with("ConnectorCellOutOfRange") && m.contains("D2") && m.contains("2 column"), "{m}");
    // A formatted cell holding no value neither widens the header nor lies out of range.
    let styled = r#"<row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c><c r="C1" s="3"/></row>
                    <row r="2"><c r="A2"><v>1</v></c><c r="B2"><v>2</v></c><c r="C2" s="3"/><c r="F2" s="3"></c></row>"#;
    assert_eq!(landed(&book(styled, &["a", "b"], &[])), vec![json!({"a": "1", "b": "2"})]);
}

/// An external-reference declaration, an external-links part, or a relationship marked
/// external raises `ConnectorExternalReference`.
// spec: connector.source.external-reference@2d61aa05
#[test]
fn every_form_of_external_reference_is_refused() {
    let (ws, ss) = (sheet_xml(r#"<row r="1"><c r="A1" t="s"><v>0</v></c></row>"#), shared_xml(&["a"]));
    let declared = workbook_xml(&["Data"], r#"<externalReferences><externalReference r:id="rId9"/></externalReferences>"#);
    let external_rel = rels_xml(1, &format!(r#"<Relationship Id="rId9" Type="{REL_NS}/hyperlink" Target="https://example.test/other.xlsx" TargetMode="External"/>"#));
    let link_rel = rels_xml(1, &format!(r#"<Relationship Id="rId9" Type="{REL_NS}/externalLink" Target="links/l1.xml"/>"#));
    let (wb, rels) = (workbook_xml(&["Data"], ""), rels_xml(1, ""));
    type Entries<'a> = Vec<(&'a str, &'a [u8])>;
    let cases: [(&str, Entries); 5] = [
        ("declaration", vec![("xl/workbook.xml", declared.as_bytes()), ("xl/_rels/workbook.xml.rels", rels.as_bytes())]),
        ("part", vec![("xl/workbook.xml", wb.as_bytes()), ("xl/_rels/workbook.xml.rels", rels.as_bytes()), ("xl/externalLinks/externalLink1.xml", b"<externalLink/>")]),
        // Part names compare without regard to ASCII case.
        ("part cased", vec![("xl/workbook.xml", wb.as_bytes()), ("xl/_rels/workbook.xml.rels", rels.as_bytes()), ("xl/ExternalLinks/externalLink1.xml", b"<externalLink/>")]),
        // An external-links part is whatever part a relationship of that type names.
        ("part by type", vec![("xl/workbook.xml", wb.as_bytes()), ("xl/_rels/workbook.xml.rels", link_rel.as_bytes()), ("xl/links/l1.xml", b"<externalLink/>")]),
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

/// One worksheet read holds at most 64 MiB — resolved cell text and keys, plus 32 B for each
/// shared string and each stored cell, counted as they land — and lands at most 1048576 rows.
// spec: connector.source.worksheet-landing@5b81c75d
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

    // A valueless cell inflates from 4 bytes into a map entry, and `<si/>` from 5 into a
    // string: each is charged 32 bytes beside its text, so a sub-MiB body cannot fan out.
    let letters: Vec<String> = (b'a'..=b'z').map(|b| (b as char).to_string()).collect();
    let names: Vec<&str> = letters.iter().map(String::as_str).collect();
    let head: String = (0..26u8).map(|i| format!(r#"<c r="{}1" t="s"><v>{i}</v></c>"#, (b'A' + i) as char)).collect();
    let empties = format!(r#"<row r="1">{head}</row>{}"#, format!("<row>{}</row>", "<c/>".repeat(26)).repeat(100_000));
    let m = refusal(&book(&empties, &names, &[]));
    assert!(m.contains("67108864 bytes"), "{m}");
    let blank_strings = format!(r#"<?xml version="1.0"?><sst xmlns="{SS}">{}</sst>"#, "<si/>".repeat(2_200_000));
    let (wb, rels, ws) = (workbook_xml(&["Data"], ""), rels_xml(1, ""), sheet_xml(r#"<row r="1"><c r="A1" t="inlineStr"><is><t>v</t></is></c></row>"#));
    let m = refusal(&zip_of(&[
        ("xl/workbook.xml", wb.as_bytes()),
        ("xl/_rels/workbook.xml.rels", rels.as_bytes()),
        ("xl/worksheets/sheet1.xml", ws.as_bytes()),
        ("xl/sharedStrings.xml", blank_strings.as_bytes()),
    ]));
    assert!(m.contains("67108864 bytes"), "{m}");

    let header = r#"<row r="1"><c r="A1" t="inlineStr"><is><t>v</t></is></c></row>"#;
    let at_cap = format!("{header}{}", "<row></row>".repeat(ROW_CAP));
    assert_eq!(rows(&book(&at_cap, &[], &[]), None, 0, "book.xlsx").unwrap().len(), ROW_CAP);
    let past_cap = format!("{header}{}", "<row></row>".repeat(ROW_CAP + 1));
    let m = refusal(&book(&past_cap, &[], &[]));
    assert!(m.contains("more than 1048576 rows"), "{m}");
}

#[test]
fn skip_rows_counts_sheet_rows_whether_or_not_a_blank_row_is_stored() {
    let title = r#"<row r="1"><c r="A1" t="inlineStr"><is><t>Title</t></is></c></row>"#;
    let tail = r#"<row r="3"><c r="A3" t="inlineStr"><is><t>a</t></is></c><c r="B3" t="inlineStr"><is><t>b</t></is></c></row>
                  <row r="4"><c r="A4"><v>1</v></c><c r="B4"><v>2</v></c></row>
                  <row r="5"><c r="A5"><v>3</v></c><c r="B5"><v>4</v></c></row>"#;
    let want = vec![json!({"a": "1", "b": "2"}), json!({"a": "3", "b": "4"})];
    for (case, blank) in [("omitted", ""), ("formatted", r#"<row r="2" ht="20" customHeight="1"/>"#)] {
        let got: Vec<Value> = rows(&book(&format!("{title}{blank}{tail}"), &[], &[]), None, 2, "book.xlsx").unwrap().into_iter().map(Value::Object).collect();
        assert_eq!(got, want, "{case}");
    }
    // A row with no `r` takes the number after the row before it.
    let unnumbered = r#"<row><c t="inlineStr"><is><t>Title</t></is></c></row>
                        <row><c t="inlineStr"><is><t>a</t></is></c></row>
                        <row><c><v>1</v></c></row>"#;
    let got: Vec<Value> = rows(&book(unnumbered, &[], &[]), None, 1, "book.xlsx").unwrap().into_iter().map(Value::Object).collect();
    assert_eq!(got, vec![json!({"a": "1"})]);
}

#[test]
fn cdata_text_lands_as_written() {
    let sheet = r#"<row r="1"><c r="A1" t="inlineStr"><is><t><![CDATA[x<y]]></t></is></c><c r="B1" t="s"><v>0</v></c><c r="C1" t="inlineStr"><is><t>n</t></is></c></row>
                   <row r="2"><c r="A2"><v><![CDATA[42]]></v></c><c r="B2" t="s"><v>0</v></c><c r="C2" t="inlineStr"><is><t>z</t></is></c></row>"#;
    let shared = format!(r#"<?xml version="1.0"?><sst xmlns="{SS}"><si><t><![CDATA[sh<ared]]></t></si></sst>"#);
    let (wb, rels, ws) = (workbook_xml(&["Data"], ""), rels_xml(1, ""), sheet_xml(sheet));
    let bytes = zip_of(&[
        ("xl/workbook.xml", wb.as_bytes()),
        ("xl/_rels/workbook.xml.rels", rels.as_bytes()),
        ("xl/worksheets/sheet1.xml", ws.as_bytes()),
        ("xl/sharedStrings.xml", shared.as_bytes()),
    ]);
    assert_eq!(landed(&bytes), vec![json!({"x<y": "42", "sh<ared": "sh<ared", "n": "z"})]);
}

#[test]
fn a_phonetic_guide_is_no_part_of_an_inline_string() {
    let sheet = r#"<row r="1"><c r="A1" t="inlineStr"><is><t>city</t></is></c></row>
                   <row r="2"><c r="A2" t="inlineStr"><is><t>東京</t><rPh sb="0" eb="2"><t>トウキョウ</t></rPh><phoneticPr fontId="1"/></is></c></row>"#;
    assert_eq!(landed(&book(sheet, &[], &[])), vec![json!({"city": "東京"})]);
}

#[test]
fn a_truncated_part_is_unreadable_rather_than_short() {
    let (wb, rels) = (workbook_xml(&["Data"], ""), rels_xml(1, ""));
    let ws = sheet_xml(r#"<row r="1"><c r="A1" t="s"><v>0</v></c></row>"#);
    let (full_ss, cut_ss) = (shared_xml(&["a"]), format!(r#"<?xml version="1.0"?><sst xmlns="{SS}"><si><t>a</t></si><si><t>b"#));
    let (cut_wb, cut_rels) = (&wb[..wb.len() - "</workbook>".len()], &rels[..rels.len() - "</Relationships>".len()]);
    let cases = [("shared strings", wb.as_str(), rels.as_str(), cut_ss.as_str()), ("workbook", cut_wb, rels.as_str(), full_ss.as_str()), ("relationships", wb.as_str(), cut_rels, full_ss.as_str())];
    for (case, wb, rels, ss) in cases {
        let bytes = zip_of(&[
            ("xl/workbook.xml", wb.as_bytes()),
            ("xl/_rels/workbook.xml.rels", rels.as_bytes()),
            ("xl/worksheets/sheet1.xml", ws.as_bytes()),
            ("xl/sharedStrings.xml", ss.as_bytes()),
        ]);
        let m = refusal(&bytes);
        assert!(m.starts_with("PipelineUnreadableInput") && m.contains("open"), "{case}: {m}");
    }
}

#[test]
fn a_column_a_row_carries_twice_is_refused_rather_than_overwritten() {
    let header = r#"<row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row>"#;
    let cases = [
        ("repeated", format!(r#"{header}<row r="2"><c r="A2"><v>1</v></c><c r="A2"><v>2</v></c></row>"#)),
        ("implied", format!(r#"{header}<row r="2"><c r="B2"><v>1</v></c><c r="A2"><v>2</v></c><c><v>3</v></c></row>"#)),
        ("header", r#"<row r="1"><c r="A1" t="s"><v>0</v></c><c r="A1" t="s"><v>1</v></c></row>"#.to_string()),
    ];
    for (case, sheet) in cases {
        let m = refusal(&book(&sheet, &["a", "b"], &[]));
        assert!(m.starts_with("PipelineUnreadableInput") && m.contains("twice"), "{case}: {m}");
    }
}

#[test]
fn the_shared_string_part_is_the_one_the_workbook_relationships_name() {
    let wb = workbook_xml(&["Data"], "");
    let ws = sheet_xml(r#"<row r="1"><c r="A1" t="s"><v>0</v></c></row><row r="2"><c r="A2" t="s"><v>1</v></c></row>"#);
    let rels = rels_xml(1, &format!(r#"<Relationship Id="rId7" Type="{REL_NS}/sharedStrings" Target="strings/table.xml"/>"#));
    let ss = shared_xml(&["k", "v"]);
    let bytes = zip_of(&[
        ("xl/workbook.xml", wb.as_bytes()),
        ("xl/_rels/workbook.xml.rels", rels.as_bytes()),
        ("xl/worksheets/sheet1.xml", ws.as_bytes()),
        ("xl/strings/table.xml", ss.as_bytes()),
    ]);
    assert_eq!(landed(&bytes), vec![json!({"k": "v"})]);
}
