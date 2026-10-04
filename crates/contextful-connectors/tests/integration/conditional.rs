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
    assert_eq!(first["more"], json!(false), "a conditional pull reads one document");

    let second = pull(&mut s, Some(validators.clone()));
    assert_eq!(second["rows"], json!([]));
    assert_eq!(second["cursor"], validators, "a 304 holds the position");
    assert_eq!(second["snapshot_complete"], json!(false), "a 304 has not examined the snapshot");

    let seen = vendor.received("/news.rss");
    assert_eq!(seen.len(), 2);
    assert_eq!((seen[0].header("if-none-match"), seen[0].header("if-modified-since")), (None, None));
    assert_eq!((seen[1].header("if-none-match"), seen[1].header("if-modified-since")), (Some(ETAG), Some(MODIFIED)));
}

#[test]
fn an_empty_conditional_success_completes_the_snapshot() {
    let vendor = Server::start(|_| Response { status: 200, headers: vec![("ETag".into(), "empty-v2".into())], body: b"[]".to_vec() });
    let mut s = source(json!({"endpoint": vendor.url("/items"), "conditional": true}), vec![]);
    let out = pull(&mut s, Some(json!({"etag": "populated-v1"})));
    assert_eq!(out["rows"], json!([]));
    assert_eq!(out["cursor"], json!({"etag": "empty-v2"}));
    assert_eq!(out["snapshot_complete"], json!(true));
}

/// A conditional pull issues one request, and the validators are its whole position: no page token or watermark
/// rides beside them, and the pull reports no further page.
// spec: connector.source.conditional-position@67dec9d6
#[test]
fn a_conditional_pull_commits_the_validators_alone() {
    let vendor = Server::start(|_| Response {
        status: 200,
        headers: vec![("ETag".into(), ETAG.into()), ("Link".into(), "</news.rss?page=2>; rel=\"next\"".into())],
        body: FEED.as_bytes().to_vec(),
    });
    let mut s = source(json!({"endpoint": vendor.url("/news.rss"), "format": "feed", "conditional": true}), vec![]);
    let out = pull(&mut s, Some(json!({"next": "/news.rss?page=2", "field": "published_at", "at": "2026-02-03T08:00:00Z", "etag": "W/\"old\""})));
    assert_eq!(out["rows"].as_array().unwrap().len(), 2);
    assert_eq!(out["cursor"], json!({"etag": ETAG}), "the served validators replace the whole position");
    assert_eq!(out["more"], json!(false));
    let seen = vendor.received("/news.rss");
    assert_eq!(seen.len(), 1, "one request, the served next link unfollowed");
    assert_eq!((seen[0].query("page"), seen[0].header("if-none-match")), (None, Some("W/\"old\"")));
}

#[test]
fn the_scope_probe_precedes_the_conditional_request() {
    let vendor = Server::start(|r| match r.path() {
        "/identity" => Response { status: 200, headers: vec![("X-Granted-Scopes".into(), "feed.read".into())], body: Vec::new() },
        _ if r.header("if-none-match") == Some(ETAG) => Response { status: 304, headers: vec![], body: Vec::new() },
        _ => Response { status: 200, headers: vec![("ETag".into(), ETAG.into())], body: FEED.as_bytes().to_vec() },
    });
    let mut s = source(
        json!({
            "endpoint": vendor.url("/news.rss"), "format": "feed", "conditional": true,
            "headers": {"Authorization": "Bearer ${secret://feed-token}"},
            "scope_probe": {"endpoint": vendor.url("/identity"), "scopes_header": "X-Granted-Scopes", "expect": ["feed.read"]}
        }),
        vec![("feed-token", "tok-1")],
    );
    let first = pull(&mut s, None);
    assert_eq!(first["rows"].as_array().unwrap().len(), 2);
    let second = pull(&mut s, Some(first["cursor"].clone()));
    assert_eq!(second["rows"], json!([]));
    let paths: Vec<String> = vendor.requests.lock().unwrap().iter().map(|r| r.path().to_string()).collect();
    assert_eq!(paths, ["/identity", "/news.rss", "/news.rss"], "one probe, ahead of the first conditional request");
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
    for extra in [json!({"page_param": "p"}), json!({"next_cursor_path": "/next", "cursor_param": "after"}), json!({"next_url_path": "/next"}), json!({"link_header": true})] {
        let mut cfg = json!({"endpoint": "https://news.example.test/feed", "format": "json", "conditional": true});
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
