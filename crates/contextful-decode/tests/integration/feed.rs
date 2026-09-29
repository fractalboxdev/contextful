//! The feed decoder: RSS 2.0 and Atom 1.0 documents, one row per item or entry.

use contextful_decode::{decode, Format};
use serde_json::{json, Value};

const ATOM: &[u8] = include_bytes!("../fixtures/feed/commits.atom");
const RSS: &[u8] = include_bytes!("../fixtures/feed/releases.rss");

fn feed(body: &[u8]) -> Vec<Value> {
    let (rows, parsed) = decode(Format::Feed, body, None, "feed.xml").unwrap();
    assert!(parsed.is_none(), "a feed exposes no JSON body to a pagination pointer");
    rows.into_iter().map(Value::Object).collect()
}

/// `format = "feed"` decodes an RSS or Atom document into one row per item or entry carrying `entry_id`, `title`,
/// `link`, `published_at`, `summary` and `author`, each null where the entry holds none.
// spec: connector.source.feed-format@56103434
#[test]
fn an_atom_and_an_rss_document_land_one_row_per_entry() {
    assert_eq!(Format::parse("feed").unwrap(), Format::Feed);
    assert_eq!(Format::Feed.name(), "feed");

    let atom = feed(ATOM);
    assert_eq!(atom.len(), 3);
    assert_eq!(
        atom[2],
        json!({
            "entry_id": "tag:github.com,2008:Grit::Commit/553c2077f0edc3d5dc5d17262f6aa498e69d6f8e",
            "title": "first commit",
            "link": "https://github.com/octocat/Hello-World/commit/553c2077f0edc3d5dc5d17262f6aa498e69d6f8e",
            "published_at": "2011-01-26T19:06:08Z",
            "summary": "<pre style='white-space:pre-wrap;width:81ex'>first commit</pre>",
            "author": "Cameron423698",
        })
    );

    let rss = feed(RSS);
    assert_eq!(rss.len(), 3);
    assert_eq!(
        rss[0],
        json!({
            "entry_id": "release-2.1.0",
            "title": "v2.1.0",
            "link": "https://releases.example.test/v2.1.0",
            "published_at": "2026-03-03T08:30:00Z",
            "summary": "<p>Adds <b>feeds</b> &amp; fixes.</p>",
            "author": "Release Bot",
        })
    );
    assert_eq!(rss[1]["title"], json!("v2.0.1 & notes"));
    assert_eq!(rss[1]["author"], Value::Null);
}

/// A feed row's `entry_id` is the Atom `id` or the RSS `guid`, else the entry's link; an entry carrying neither is
/// unreadable input under {{run.land.unreadable-input}}.
// spec: connector.source.feed-entry-id@a85e9b75
#[test]
fn an_entry_id_falls_back_to_the_link_and_an_entry_with_neither_refuses() {
    let rss = feed(RSS);
    assert_eq!(rss[1]["entry_id"], json!("https://releases.example.test/v2.0.1"), "no guid: the link identifies the item");
    assert_eq!(rss[2]["entry_id"], json!("https://releases.example.test/undated"));

    let anonymous = br#"<rss version="2.0"><channel><item><title>a</title></item></channel></rss>"#;
    let f = decode(Format::Feed, anonymous, None, "news.rss").unwrap_err();
    assert!(f.message.starts_with("PipelineUnreadableInput") && f.message.contains("news.rss") && f.message.contains("entry 0"), "{f}");
}

/// `published_at` is the Atom `published`, else `updated`, or the RSS `pubDate`, folded to an RFC 3339 instant in
/// UTC; a date that is neither RFC 3339 nor RFC 2822 is unreadable input.
// spec: connector.source.feed-published-at@144ef7a7
#[test]
fn published_at_lands_as_a_utc_instant_or_null_and_an_unreadable_date_refuses() {
    let rss = feed(RSS);
    assert_eq!(rss[1]["published_at"], json!("2026-02-02T23:00:00Z"), "an RFC 822 date in GMT");
    assert_eq!(rss[2]["published_at"], Value::Null, "an undated item lands null");

    // Atom: `published` wins over `updated`; an offset folds to UTC.
    let atom = br#"<feed xmlns="http://www.w3.org/2005/Atom"><entry><id>urn:e:1</id><updated>2030-01-02T00:00:00Z</updated><published>2030-01-01T09:00:00+08:00</published><link rel="self" href="https://x.test/self"/><link href="https://x.test/1"/></entry></feed>"#;
    let rows = feed(atom);
    assert_eq!(rows[0]["published_at"], json!("2030-01-01T01:00:00Z"));
    assert_eq!(rows[0]["link"], json!("https://x.test/1"), "the alternate link, not `self`");

    let bad = br#"<rss version="2.0"><channel><item><guid>g</guid><pubDate>yesterday</pubDate></item></channel></rss>"#;
    let f = decode(Format::Feed, bad, None, "news.rss").unwrap_err();
    assert!(f.message.starts_with("PipelineUnreadableInput") && f.message.contains("yesterday"), "{f}");
}

#[test]
fn a_document_that_is_neither_rss_nor_atom_refuses_whole() {
    for body in [&b"<html><body/></html>"[..], b"<rss><channel><item>", b"not xml at all"] {
        let f = decode(Format::Feed, body, None, "page.html").unwrap_err();
        assert!(f.message.starts_with("PipelineUnreadableInput") && f.message.contains("page.html"), "{f}");
    }
}
