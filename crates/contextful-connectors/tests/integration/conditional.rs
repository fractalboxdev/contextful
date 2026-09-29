//! `connector.source` conditional GET over a feed: validators ride the position.

use crate::support::{request, source, Never, Response, Server};
use contextful_connectors::http::{ConfigError, HttpConfig};
use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::Source;
use serde_json::{json, Value};

const FEED: &str = r#"<rss version="2.0"><channel><title>n</title>
<item><guid>a</guid><title>First</title><pubDate>Mon, 02 Feb 2026 23:00:00 GMT</pubDate></item>
<item><guid>b</guid><title>Second</title><pubDate>Tue, 03 Feb 2026 08:00:00 GMT</pubDate></item>
</channel></rss>"#;

const ETAG: &str = "W/\"feed-v1\"";
const MODIFIED: &str = "Tue, 03 Feb 2026 08:00:00 GMT";

fn pull(s: &mut contextful_connectors::http::HttpSource, position: Option<Value>) -> Value {
    serde_json::from_slice(&s.pull(&request(position), &Never).unwrap()).unwrap()
}

/// `conditional = true` sends the position's `etag` as `If-None-Match` and `last_modified` as `If-Modified-Since`.
/// A `304` lands no rows and holds the position; a `2xx` commits the validators it serves.
// spec: connector.source.conditional-get@847ddbc8
#[test]
fn a_not_modified_feed_lands_nothing_and_holds_the_validators() {
    let vendor = Server::start(|r| {
        if r.header("if-none-match") == Some(ETAG) {
            return Response { status: 304, headers: vec![], body: Vec::new() };
        }
        Response {
            status: 200,
            headers: vec![("Content-Type".into(), "application/rss+xml".into()), ("ETag".into(), ETAG.into()), ("Last-Modified".into(), MODIFIED.into())],
            body: FEED.as_bytes().to_vec(),
        }
    });
    let mut s = source(json!({"endpoint": vendor.url("/news.rss"), "format": "feed", "conditional": true}), vec![]);

    let first = pull(&mut s, None);
    assert_eq!(first["rows"].as_array().unwrap().len(), 2);
    assert_eq!(first["rows"][1]["published_at"], json!("2026-02-03T08:00:00Z"));
    let validators = json!({"etag": ETAG, "last_modified": MODIFIED});
    assert_eq!(first["cursor"], validators, "a 2xx commits the served validators as the position");

    let second = pull(&mut s, Some(validators.clone()));
    assert_eq!(second["rows"], json!([]));
    assert_eq!(second["cursor"], validators, "a 304 holds the position");

    let seen = vendor.received("/news.rss");
    assert_eq!(seen.len(), 2);
    assert_eq!((seen[0].header("if-none-match"), seen[0].header("if-modified-since")), (None, None));
    assert_eq!((seen[1].header("if-none-match"), seen[1].header("if-modified-since")), (Some(ETAG), Some(MODIFIED)));
}

#[test]
fn a_response_serving_no_validator_commits_an_empty_position() {
    let vendor = Server::start(|_| Response { status: 200, headers: vec![], body: FEED.as_bytes().to_vec() });
    let mut s = source(json!({"endpoint": vendor.url("/news.rss"), "format": "feed", "conditional": true}), vec![]);
    let out = pull(&mut s, Some(json!({"etag": "W/\"stale\""})));
    assert_eq!(out["rows"].as_array().unwrap().len(), 2);
    assert_eq!(out["cursor"], json!({}), "a stale validator is not sent again");
}

/// `conditional` beside a pagination shape or a declared `incremental` raises `ConnectorConditionalRejected` at
/// build.
// spec: connector.source.conditional-rejected@f20042c5
#[test]
fn conditional_beside_a_page_walk_or_an_incremental_field_is_refused() {
    for extra in [json!({"page_param": "p"}), json!({"link_header": true})] {
        let mut cfg = json!({"endpoint": "https://news.example.test/feed", "format": "feed", "conditional": true});
        cfg.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        match HttpConfig::parse(&cfg) {
            Err(ConfigError::Connector(ConnectorError::ConnectorConditionalRejected(m))) => assert!(m.contains("conditional"), "{m}"),
            other => panic!("{cfg}: {other:?}"),
        }
    }
    let config = HttpConfig::parse(&json!({"endpoint": "https://news.example.test/feed", "format": "feed", "conditional": true})).unwrap();
    assert!(matches!(config.accepts_incremental(), Err(ConnectorError::ConnectorConditionalRejected(_))));
    let plain = HttpConfig::parse(&json!({"endpoint": "https://news.example.test/feed", "format": "feed"})).unwrap();
    assert!(plain.accepts_incremental().is_ok());
}
