//! The feed decoder (`connector.source.feed-format`): an RSS 2.0 or 1.0 document or an
//! Atom 1.0 document, one row per item or entry. Elements match by local name, so a
//! namespace prefix such as `dc:` or `content:` reads as its bare name. No external entity
//! is resolved.

use crate::ooxml::{cdata, text};
use crate::unreadable;
use contextful_core::run::ports::Row;
use contextful_core::run::Failure;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use serde_json::Value;
use std::collections::BTreeMap;
use time::format_description::well_known::{Rfc2822, Rfc3339};
use time::{OffsetDateTime, UtcOffset};

/// The columns every feed row carries, `null` where the entry has no value.
pub const COLUMNS: [&str; 6] = ["entry_id", "title", "link", "published_at", "summary", "author"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Rss,
    Atom,
}

/// One item or entry as read: the first non-empty text of each direct child, the `href` links it
/// declares with their `rel`, and an Atom author's name.
#[derive(Default)]
struct Entry {
    fields: BTreeMap<String, String>,
    links: Vec<(Option<String>, String)>,
    author_name: Option<String>,
}

impl Entry {
    fn field(&self, names: &[&str]) -> Option<String> {
        names.iter().filter_map(|n| self.fields.get(*n)).map(|v| v.trim()).find(|v| !v.is_empty()).map(str::to_string)
    }

    /// The entry's page: an RSS `link` text, else an `href` link whose `rel` is absent or
    /// `alternate`.
    fn link(&self) -> Option<String> {
        self.field(&["link"]).or_else(|| {
            self.links.iter().find(|(rel, _)| rel.as_deref().is_none_or(|r| r == "alternate")).map(|(_, href)| href.trim().to_string()).filter(|h| !h.is_empty())
        })
    }

    fn row(self, kind: Kind, ordinal: usize, input: &str) -> Result<Row, Failure> {
        let at = || format!("entry {ordinal}");
        let link = self.link();
        let id = match kind {
            Kind::Atom => self.field(&["id"]),
            Kind::Rss => self.field(&["guid"]),
        };
        let entry_id = id.or_else(|| link.clone()).ok_or_else(|| unreadable(input, at(), "the entry carries neither an id nor a link"))?;
        let date = match kind {
            Kind::Atom => self.field(&["published", "updated"]),
            Kind::Rss => self.field(&["pubDate", "date"]),
        };
        let published_at = match date {
            None => None,
            Some(raw) => Some(instant(&raw).ok_or_else(|| unreadable(input, at(), format!("date `{raw}` is neither RFC 3339 nor RFC 2822")))?),
        };
        let summary = match kind {
            Kind::Atom => self.field(&["summary", "content"]),
            Kind::Rss => self.field(&["description", "encoded"]),
        };
        let author = match kind {
            Kind::Atom => self.author_name.as_ref().map(|a| a.trim().to_string()).filter(|a| !a.is_empty()),
            Kind::Rss => self.field(&["creator", "author"]),
        };
        let values = [Some(entry_id), self.field(&["title"]), link, published_at, summary, author];
        Ok(COLUMNS.iter().zip(values).map(|(c, v)| (c.to_string(), v.map_or(Value::Null, Value::String))).collect())
    }
}

/// An RFC 3339 or RFC 2822 date as an RFC 3339 instant in UTC.
fn instant(raw: &str) -> Option<String> {
    let parsed = OffsetDateTime::parse(raw, &Rfc3339).or_else(|_| OffsetDateTime::parse(raw, &Rfc2822)).ok()?;
    parsed.to_offset(UtcOffset::UTC).format(&Rfc3339).ok()
}

fn local(e: &BytesStart<'_>) -> String {
    String::from_utf8_lossy(e.local_name().as_ref()).into_owned()
}

/// The `rel` and `href` of a link element, when it carries an `href`.
fn href(e: &BytesStart<'_>) -> Option<(Option<String>, String)> {
    let mut rel = None;
    let mut href = None;
    for a in e.attributes().flatten() {
        let value = a.unescape_value().map(|v| v.into_owned()).unwrap_or_else(|_| String::from_utf8_lossy(&a.value).into_owned());
        match a.key.local_name().as_ref() {
            b"rel" => rel = Some(value),
            b"href" => href = Some(value),
            _ => {}
        }
    }
    href.map(|h| (rel, h))
}

/// Decode a feed document into one row per item or entry, in document order.
pub fn rows(body: &[u8], input: &str) -> Result<Vec<Row>, Failure> {
    let xml = std::str::from_utf8(body).map_err(|e| unreadable(input, format!("byte {}", e.valid_up_to()), "the body is not UTF-8"))?;
    let mut reader = Reader::from_str(xml);
    let mut depth = 0usize;
    let mut kind: Option<Kind> = None;
    // The open entry and its depth, the direct child being read, and an Atom author's name.
    let mut entry: Option<(usize, Entry)> = None;
    let mut child: Option<(String, String)> = None;
    let mut name: Option<String> = None;
    let mut out = Vec::new();
    loop {
        let position = reader.buffer_position();
        let event = reader.read_event().map_err(|e| unreadable(input, format!("byte {position}"), e))?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(event, Event::Empty(_));
                let at = depth + 1;
                let tag = local(e);
                if at == 1 {
                    kind = Some(match tag.as_str() {
                        "rss" | "RDF" => Kind::Rss,
                        "feed" => Kind::Atom,
                        other => return Err(unreadable(input, "the root element".into(), format!("`{other}` is neither an RSS nor an Atom document"))),
                    });
                } else if let Some((d, current)) = entry.as_mut() {
                    if at == *d + 1 {
                        // An `href`-bearing link, such as an RSS item's `atom:link`, carries no link text.
                        let linked = tag == "link" && href(e).inspect(|l| current.links.push(l.clone())).is_some();
                        if !empty && !linked {
                            child = Some((tag, String::new()));
                        }
                    } else if at == *d + 2 && !empty && tag == "name" && child.as_ref().is_some_and(|(c, _)| c == "author") {
                        name = Some(String::new());
                    }
                } else if !empty && matches!((kind, tag.as_str()), (Some(Kind::Rss), "item") | (Some(Kind::Atom), "entry")) {
                    entry = Some((at, Entry::default()));
                }
                if !empty {
                    depth = at;
                }
            }
            Event::Text(ref t) => {
                if let Some(n) = name.as_mut().or(child.as_mut().map(|(_, v)| v)) {
                    n.push_str(&text(t));
                }
            }
            Event::CData(ref t) => {
                if let Some(n) = name.as_mut().or(child.as_mut().map(|(_, v)| v)) {
                    n.push_str(&cdata(t));
                }
            }
            Event::End(_) => {
                if let Some(d) = entry.as_ref().map(|(d, _)| *d) {
                    if depth == d + 2 {
                        if let (Some(n), Some((_, current))) = (name.take(), entry.as_mut()) {
                            current.author_name.get_or_insert(n);
                        }
                    } else if depth == d + 1 {
                        // The first non-empty text of each child names the field.
                        if let (Some((tag, value)), Some((_, current))) = (child.take(), entry.as_mut()) {
                            if !value.trim().is_empty() {
                                current.fields.entry(tag).or_insert(value);
                            }
                        }
                    } else if depth == d {
                        if let Some((_, done)) = entry.take() {
                            out.push(done.row(kind.unwrap_or(Kind::Rss), out.len(), input)?);
                        }
                    }
                }
                depth = depth.saturating_sub(1);
            }
            Event::Eof => {
                return match (kind, depth) {
                    (None, _) => Err(unreadable(input, "the document".into(), "no root element: neither an RSS nor an Atom document")),
                    (Some(_), 0) => Ok(out),
                    (Some(_), open) => Err(unreadable(input, format!("byte {}", reader.buffer_position()), format!("{open} elements are unclosed"))),
                };
            }
            _ => {}
        }
    }
}
