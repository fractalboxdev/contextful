//! One worksheet of an Office Open XML workbook, landed as rows under its header row.
//!
//! The read opens four parts by exact name — `xl/workbook.xml`, its relationships, the
//! shared-string part those relationships name (`xl/sharedStrings.xml` when they name
//! none), and the one chosen worksheet — so charts, pivot caches, macros and styles are
//! never opened (`connector.source.office-part-selection`).
//!
//! A worksheet is sparse: an absent cell emits no `<c>`, so a cell lands by the column its
//! `r="B7"` reference declares, and a row omits the key of a cell it does not carry. A cell
//! within the header's width that exists and holds no value lands null; past the width it
//! lands nothing. A gap between row indices lands nothing, and no declared index sizes an
//! allocation.
//!
//! Values are indirect: a `t="s"` cell indexes the shared-string table, so one stored
//! string can resolve into every cell. The landing budget is therefore charged on resolved
//! text as each cell lands, again for the header name each landed cell is keyed by, and a
//! fixed [`ENTRY_CHARGE`] for every shared string and stored cell, since `<c/>` inflates
//! from 4 bytes into a map entry (`connector.source.worksheet-landing`).

use crate::ooxml::{cdata, in_ns, text, Archive};
use crate::unreadable;
use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::Row;
use contextful_core::run::{Failure, FailureTag};
use quick_xml::events::attributes::Attribute;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{NsReader, Reader};
use serde_json::Value;

/// Bytes one worksheet read holds — resolved cell text, keys and entry charges: 64 MiB
/// (`connector.source.worksheet-landing`).
pub const TEXT_BUDGET: u64 = 64 * 1024 * 1024;
/// Bytes charged for each shared string and each stored cell beside its text: 32
/// (`connector.source.worksheet-landing`).
pub const ENTRY_CHARGE: u64 = 32;
/// Rows one worksheet lands: 1048576 (`connector.source.worksheet-landing`).
pub const ROW_CAP: usize = 1_048_576;

/// SpreadsheetML in its transitional and strict namespaces.
const NS: &[&str] = &["http://schemas.openxmlformats.org/spreadsheetml/2006/main", "http://purl.oclc.org/ooxml/spreadsheetml/main"];

const PART_WORKBOOK: &str = "xl/workbook.xml";
const PART_RELS: &str = "xl/_rels/workbook.xml.rels";
const PART_SHARED: &str = "xl/sharedStrings.xml";
/// Where a workbook caches another workbook's cells.
const PREFIX_EXTERNAL_LINKS: &str = "xl/externalLinks/";
/// The relationship-type suffixes shared by the transitional and strict namespaces.
const REL_EXTERNAL_LINK: &str = "/externalLink";
const REL_SHARED_STRINGS: &str = "/sharedStrings";

fn external(input: &str, why: impl std::fmt::Display) -> Failure {
    Failure::deterministic(FailureTag::Permanent, ConnectorError::ConnectorExternalReference(format!("`{input}`: {why}; a workbook lands only what its own archive holds")).to_string())
}

/// The rows of worksheet `sheet`, or of the first sheet in workbook order. `skip_rows`
/// discards sheet rows 1 through `skip_rows` by row number, so the header is the first
/// stored row past it whether or not a blank preamble row is stored.
pub fn rows(body: &[u8], sheet: Option<&str>, skip_rows: usize, input: &str) -> Result<Vec<Row>, Failure> {
    let mut archive = Archive::open(body, input)?;
    if archive.has_prefix(PREFIX_EXTERNAL_LINKS) {
        return Err(external(input, format!("the archive carries an external-links part under `{PREFIX_EXTERNAL_LINKS}`")));
    }
    let book = workbook_xml(&archive.require(PART_WORKBOOK)?, input)?;
    if book.external_references {
        return Err(external(input, format!("`{PART_WORKBOOK}` declares external references")));
    }
    let rels = relationships(&archive.require(PART_RELS)?, input)?;
    if let Some(r) = rels.iter().find(|r| r.external) {
        return Err(external(input, format!("relationship `{}` in `{PART_RELS}` targets `{}` outside the archive", r.id, r.target)));
    }
    if let Some(r) = rels.iter().find(|r| r.kind.ends_with(REL_EXTERNAL_LINK)) {
        return Err(external(input, format!("relationship `{}` in `{PART_RELS}` names `{}` as an external-links part", r.id, r.target)));
    }
    let chosen = match sheet {
        None => book.sheets.first().ok_or_else(|| unreadable(input, format!("part `{PART_WORKBOOK}`"), "the workbook declares no sheet"))?,
        Some(want) => book.sheets.iter().find(|s| s.name == want).ok_or_else(|| {
            let names: Vec<&str> = book.sheets.iter().map(|s| s.name.as_str()).collect();
            unreadable(input, format!("part `{PART_WORKBOOK}`"), format!("no sheet is named `{want}`; the workbook carries {}", names.join(", ")))
        })?,
    };
    let rel = chosen
        .rel_id
        .as_deref()
        .and_then(|id| rels.iter().find(|r| r.id == id))
        .ok_or_else(|| unreadable(input, format!("part `{PART_RELS}`"), format!("sheet `{}` names no declared relationship", chosen.name)))?;
    let part = target_part(&rel.target).ok_or_else(|| unreadable(input, format!("part `{PART_RELS}`"), format!("sheet `{}` targets `{}` outside the archive root", chosen.name, rel.target)))?;
    // The relationships name the shared-string part; a workbook of numbers and inline
    // strings carries none.
    let shared_part = match rels.iter().find(|r| r.kind.ends_with(REL_SHARED_STRINGS)) {
        Some(r) => target_part(&r.target).ok_or_else(|| unreadable(input, format!("part `{PART_RELS}`"), format!("the shared strings target `{}` outside the archive root", r.target)))?,
        None => PART_SHARED.to_string(),
    };
    let mut budget = Budget { bytes: 0, input: input.to_string(), sheet: chosen.name.clone() };
    let shared = match archive.part(&shared_part)? {
        Some(xml) => shared_strings(&xml, &shared_part, &mut budget, input)?,
        None => Vec::new(),
    };
    worksheet(&archive.require(&part)?, &shared, budget, skip_rows, input)
}

/// The part ending with elements still open: a truncated part is unreadable, never short.
fn unclosed(input: &str, part: &str, depth: usize) -> Failure {
    unreadable(input, format!("part `{part}`"), format!("the part ends with {depth} element(s) open"))
}

struct SheetRef {
    name: String,
    /// The relationship naming the worksheet part.
    rel_id: Option<String>,
}

struct Book {
    sheets: Vec<SheetRef>,
    external_references: bool,
}

fn workbook_xml(xml: &str, input: &str) -> Result<Book, Failure> {
    let mut reader = NsReader::from_str(xml);
    reader.config_mut().check_end_names = true;
    let mut book = Book { sheets: Vec::new(), external_references: false };
    let mut depth = 0usize;
    loop {
        let (ns, event) = reader.read_resolved_event().map_err(|e| malformed(input, PART_WORKBOOK, e))?;
        if matches!(event, Event::Start(_)) {
            depth += 1;
        }
        match event {
            Event::Eof if depth > 0 => return Err(unclosed(input, PART_WORKBOOK, depth)),
            Event::Eof => return Ok(book),
            Event::End(_) => depth = depth.saturating_sub(1),
            Event::Start(ref e) | Event::Empty(ref e) if in_ns(&ns, NS) => match e.local_name().as_ref() {
                // `r:id` is the one attribute of `<sheet>` whose local name is `id`.
                b"sheet" => book.sheets.push(SheetRef { name: attr(e, b"name").unwrap_or_default(), rel_id: attr(e, b"id") }),
                b"externalReferences" | b"externalReference" => book.external_references = true,
                _ => {}
            },
            _ => {}
        }
    }
}

struct Rel {
    id: String,
    /// The relationship's `Type` URI.
    kind: String,
    target: String,
    external: bool,
}

/// Every relationship the workbook declares. The relationship vocabulary is fixed and
/// namespace-free, so local names suffice.
fn relationships(xml: &str, input: &str) -> Result<Vec<Rel>, Failure> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = true;
    let mut out = Vec::new();
    let mut depth = 0usize;
    loop {
        let event = reader.read_event().map_err(|e| malformed(input, PART_RELS, e))?;
        if matches!(event, Event::Start(_)) {
            depth += 1;
        }
        match event {
            Event::Eof if depth > 0 => return Err(unclosed(input, PART_RELS, depth)),
            Event::Eof => return Ok(out),
            Event::End(_) => depth = depth.saturating_sub(1),
            Event::Start(ref e) | Event::Empty(ref e) if e.local_name().as_ref() == b"Relationship" => out.push(Rel {
                id: attr(e, b"Id").unwrap_or_default(),
                kind: attr(e, b"Type").unwrap_or_default(),
                target: attr(e, b"Target").unwrap_or_default(),
                external: attr(e, b"TargetMode").is_some_and(|m| m.eq_ignore_ascii_case("external")),
            }),
            _ => {}
        }
    }
}

/// A relationship target as an entry name: root-absolute after a leading `/`, otherwise
/// relative to `xl/`. `None` when it climbs out of the archive root.
fn target_part(target: &str) -> Option<String> {
    let joined = match target.strip_prefix('/') {
        Some(rest) => rest.to_string(),
        None => format!("xl/{target}"),
    };
    let mut segments: Vec<&str> = Vec::new();
    for s in joined.split('/') {
        match s {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            s => segments.push(s),
        }
    }
    Some(segments.join("/"))
}

/// The shared-string table in part `part`. A rich-text entry concatenates its runs; a
/// phonetic guide (`<rPh>`) is no part of the text. Each entry is charged to `budget`.
fn shared_strings(xml: &str, part: &str, budget: &mut Budget, input: &str) -> Result<Vec<String>, Failure> {
    let mut reader = NsReader::from_str(xml);
    reader.config_mut().check_end_names = true;
    let (mut out, mut current, mut in_text) = (Vec::new(), None::<String>, false);
    let (mut depth, mut skip_from) = (0usize, None::<usize>);
    loop {
        let (ns, event) = reader.read_resolved_event().map_err(|e| malformed(input, part, e))?;
        let ss = in_ns(&ns, NS);
        match event {
            Event::Eof if depth > 0 => return Err(unclosed(input, part, depth)),
            Event::Eof => return Ok(out),
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(event, Event::Empty(_));
                if !empty {
                    depth += 1;
                }
                if skip_from.is_some() || !ss {
                    continue;
                }
                match e.local_name().as_ref() {
                    b"si" => {
                        budget.charge(ENTRY_CHARGE)?;
                        if empty {
                            out.push(String::new());
                        } else {
                            current = Some(String::new());
                        }
                    }
                    b"rPh" | b"phoneticPr" if !empty => skip_from = Some(depth),
                    b"t" => in_text = !empty,
                    _ => {}
                }
            }
            Event::Text(ref t) if in_text && skip_from.is_none() => {
                if let Some(buf) = current.as_mut() {
                    buf.push_str(&text(t));
                }
            }
            Event::CData(ref t) if in_text && skip_from.is_none() => {
                if let Some(buf) = current.as_mut() {
                    buf.push_str(&cdata(t));
                }
            }
            Event::End(ref e) => {
                let closing = depth;
                depth = depth.saturating_sub(1);
                if let Some(from) = skip_from {
                    if closing <= from {
                        skip_from = None;
                    }
                    continue;
                }
                if !ss {
                    continue;
                }
                match e.local_name().as_ref() {
                    b"t" => in_text = false,
                    b"si" => out.extend(current.take()),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// One cell as the walk accumulates it.
#[derive(Default)]
struct Cell {
    column: usize,
    /// The `r` attribute as written.
    reference: String,
    kind: Vec<u8>,
    /// `<v>`, for every type but an inline string.
    value: Option<String>,
    /// `<is><t>`, for an inline string.
    inline: Option<String>,
}

/// One row's cells: column, reference and resolved text.
type Cells = Vec<(usize, String, Option<String>)>;

/// The bytes one worksheet read holds so far, charged as each entry is stored.
struct Budget {
    bytes: u64,
    input: String,
    sheet: String,
}

impl Budget {
    fn charge(&mut self, bytes: u64) -> Result<(), Failure> {
        self.bytes = self.bytes.saturating_add(bytes);
        if self.bytes > TEXT_BUDGET {
            return Err(unreadable(
                &self.input,
                format!("sheet `{}`", self.sheet),
                format!(
                    "the worksheet lands more than {TEXT_BUDGET} bytes of cell text, keys and {ENTRY_CHARGE}-byte entry charges; a shared string is stored once and may resolve into every cell"
                ),
            ));
        }
        Ok(())
    }
}

struct Walk<'a> {
    shared: &'a [String],
    input: &'a str,
    sheet: &'a str,
    budget: Budget,
}

impl Walk<'_> {
    fn at(&self, row: &str) -> String {
        format!("sheet `{}` row {row}", self.sheet)
    }

    /// Resolve a cell and append it to the row in progress, charging its entry and text
    /// first.
    fn push(&mut self, row: Option<&mut Cells>, cell: Cell, row_ref: &str) -> Result<(), Failure> {
        let Some(row) = row else { return Ok(()) };
        let value = self.resolve(&cell, row_ref)?;
        self.budget.charge(ENTRY_CHARGE + value.as_ref().map_or(0, String::len) as u64)?;
        let reference = if cell.reference.is_empty() { column_ref(cell.column) } else { cell.reference };
        row.push((cell.column, reference, value));
        Ok(())
    }

    /// A row carrying column `column` twice: the later cell would overwrite the earlier.
    fn twice(&self, column: usize, reference: &str, row_ref: &str) -> Failure {
        unreadable(self.input, self.at(row_ref), format!("cell `{reference}` puts the row in column {} twice", column_ref(column)))
    }

    /// A cell's text, or `None` when the cell exists and holds no value. Every stored form
    /// lands as the text the part holds: a number as written, a date as its serial, a
    /// formula as its cached value. A boolean's stored `0`/`1` reads `false`/`true`.
    fn resolve(&self, cell: &Cell, row_ref: &str) -> Result<Option<String>, Failure> {
        match cell.kind.as_slice() {
            b"s" => {
                let Some(raw) = cell.value.as_deref() else { return Ok(None) };
                let idx: usize = raw.trim().parse().map_err(|_| unreadable(self.input, self.at(row_ref), format!("cell `{}` indexes shared strings with `{raw}`", cell.reference)))?;
                let s = self.shared.get(idx).ok_or_else(|| {
                    unreadable(self.input, self.at(row_ref), format!("cell `{}` indexes shared string {idx} of {}", cell.reference, self.shared.len()))
                })?;
                Ok(Some(s.clone()))
            }
            b"inlineStr" => Ok(cell.inline.clone()),
            b"b" => Ok(cell.value.as_deref().map(|v| match v.trim() {
                "0" => "false".to_string(),
                "1" => "true".to_string(),
                other => other.to_string(),
            })),
            _ => Ok(cell.value.clone()),
        }
    }

    /// The header row names the columns: a gap or a repeated name names no column. The
    /// width ends at the last cell holding text, so a formatted blank past it widens nothing.
    fn header(&self, cells: Cells, row_ref: &str) -> Result<Vec<String>, Failure> {
        let width = cells.iter().filter(|(.., v)| v.as_deref().is_some_and(|v| !v.is_empty())).map(|(c, ..)| c + 1).max().unwrap_or(0);
        let mut names: Vec<Option<String>> = vec![None; width];
        let mut seen: Vec<bool> = vec![false; width];
        for (column, reference, value) in cells {
            if column >= width {
                continue;
            }
            if std::mem::replace(&mut seen[column], true) {
                return Err(self.twice(column, &reference, row_ref));
            }
            names[column] = value.filter(|v| !v.is_empty());
        }
        let mut out: Vec<String> = Vec::with_capacity(width);
        for (i, name) in names.into_iter().enumerate() {
            let name = name.ok_or_else(|| unreadable(self.input, self.at(row_ref), format!("the header leaves column {} unnamed", column_ref(i))))?;
            if out.contains(&name) {
                return Err(unreadable(self.input, self.at(row_ref), format!("the header names column `{name}` twice")));
            }
            out.push(name);
        }
        Ok(out)
    }

    /// One data row keyed by the header. A cell holding a value past the header's width
    /// refuses; a valueless one there lands nothing.
    fn data(&mut self, cells: Cells, header: &[String], row_ref: &str) -> Result<Row, Failure> {
        let mut row = Row::new();
        for (column, reference, value) in cells {
            let Some(name) = header.get(column) else {
                if value.is_none() {
                    continue;
                }
                return Err(Failure::deterministic(
                    FailureTag::Permanent,
                    ConnectorError::ConnectorCellOutOfRange(format!("`{}` at {}: cell `{reference}` lies past the header's {} column(s)", self.input, self.at(row_ref), header.len())).to_string(),
                ));
            };
            // The key is a fresh copy of the header cell's text on every row.
            self.budget.charge(name.len() as u64)?;
            if row.insert(name.clone(), value.map_or(Value::Null, Value::String)).is_some() {
                return Err(self.twice(column, &reference, row_ref));
            }
        }
        Ok(row)
    }
}

fn worksheet(xml: &str, shared: &[String], budget: Budget, skip_rows: usize, input: &str) -> Result<Vec<Row>, Failure> {
    let sheet = budget.sheet.clone();
    let part = format!("sheet `{sheet}`");
    let mut walk = Walk { shared, input, sheet: &sheet, budget };
    let mut reader = NsReader::from_str(xml);
    reader.config_mut().check_end_names = true;

    let (mut depth, mut skip_from) = (0usize, None::<usize>);
    // `row_number` is the sheet row number: the row's `r`, or the one after the row before.
    let (mut row_number, mut row_ref, mut row, mut next_column) = (0usize, String::new(), None::<Cells>, 0usize);
    let (mut cell, mut in_value, mut in_inline) = (None::<Cell>, false, false);
    let mut header: Option<Vec<String>> = None;
    let mut out: Vec<Row> = Vec::new();

    loop {
        let (ns, event) = reader.read_resolved_event().map_err(|e| malformed(input, &part, e))?;
        let ss = in_ns(&ns, NS);
        match event {
            Event::Eof if depth > 0 => return Err(unreadable(input, part, format!("the part ends with {depth} element(s) open"))),
            Event::Eof => return Ok(out),
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(event, Event::Empty(_));
                if !empty {
                    depth += 1;
                }
                if skip_from.is_some() || !ss {
                    continue;
                }
                match e.local_name().as_ref() {
                    // A formula's expression is never evaluated; its cached `<v>` lands.
                    b"f" if !empty => skip_from = Some(depth),
                    // A phonetic guide is no part of an inline string's text.
                    b"rPh" | b"phoneticPr" if !empty => skip_from = Some(depth),
                    b"row" => {
                        let declared = attr(e, b"r");
                        row_number = declared.as_deref().and_then(|r| r.trim().parse().ok()).unwrap_or(row_number + 1);
                        row_ref = declared.unwrap_or_else(|| row_number.to_string());
                        next_column = 0;
                        row = (!empty).then(Vec::new);
                    }
                    b"c" => {
                        let reference = attr(e, b"r").unwrap_or_default();
                        // A cell with no usable reference takes the next column.
                        let column = column_index(&reference).unwrap_or(next_column);
                        next_column = column + 1;
                        let c = Cell { column, reference, kind: attr(e, b"t").unwrap_or_default().into_bytes(), ..Cell::default() };
                        if empty {
                            walk.push(row.as_mut(), c, &row_ref)?;
                        } else {
                            cell = Some(c);
                        }
                    }
                    b"v" => in_value = !empty,
                    b"t" => in_inline = !empty,
                    _ => {}
                }
            }
            Event::Text(ref t) if skip_from.is_none() => {
                if let Some(c) = cell.as_mut() {
                    if in_value {
                        c.value.get_or_insert_with(String::new).push_str(&text(t));
                    } else if in_inline {
                        c.inline.get_or_insert_with(String::new).push_str(&text(t));
                    }
                }
            }
            Event::CData(ref t) if skip_from.is_none() => {
                if let Some(c) = cell.as_mut() {
                    if in_value {
                        c.value.get_or_insert_with(String::new).push_str(&cdata(t));
                    } else if in_inline {
                        c.inline.get_or_insert_with(String::new).push_str(&cdata(t));
                    }
                }
            }
            Event::End(ref e) => {
                let closing = depth;
                depth = depth.saturating_sub(1);
                if let Some(from) = skip_from {
                    if closing <= from {
                        skip_from = None;
                    }
                    continue;
                }
                if !ss {
                    continue;
                }
                match e.local_name().as_ref() {
                    b"v" => in_value = false,
                    b"t" => in_inline = false,
                    b"c" => {
                        if let Some(c) = cell.take() {
                            walk.push(row.as_mut(), c, &row_ref)?;
                        }
                    }
                    b"row" => {
                        let Some(cells) = row.take() else { continue };
                        if row_number <= skip_rows {
                            continue;
                        }
                        match &header {
                            None => header = Some(walk.header(cells, &row_ref)?),
                            Some(names) => {
                                if out.len() == ROW_CAP {
                                    return Err(unreadable(input, walk.at(&row_ref), format!("the worksheet lands more than {ROW_CAP} rows")));
                                }
                                out.push(walk.data(cells, names, &row_ref)?);
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// The zero-based column a reference names: `A1` is 0, `XFD1` is 16383. `None` when the
/// reference opens with no letter or with more than three.
fn column_index(reference: &str) -> Option<usize> {
    let letters: Vec<u8> = reference.bytes().take_while(u8::is_ascii_alphabetic).map(|b| b.to_ascii_uppercase()).collect();
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    Some(letters.iter().fold(0usize, |acc, b| acc * 26 + (b - b'A' + 1) as usize) - 1)
}

/// The column letters for a zero-based index, the inverse of [`column_index`].
fn column_ref(mut index: usize) -> String {
    let mut out = Vec::new();
    loop {
        out.push(b'A' + (index % 26) as u8);
        if index < 26 {
            break;
        }
        index = index / 26 - 1;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

fn attr(e: &BytesStart<'_>, name: &[u8]) -> Option<String> {
    e.attributes().flatten().find(|a| a.key.local_name().as_ref() == name).map(|a: Attribute<'_>| {
        a.unescape_value().map(|v| v.into_owned()).unwrap_or_else(|_| String::from_utf8_lossy(&a.value).into_owned())
    })
}

fn malformed(input: &str, part: &str, e: quick_xml::Error) -> Failure {
    unreadable(input, format!("part `{part}`"), format!("not well-formed XML: {e}"))
}
