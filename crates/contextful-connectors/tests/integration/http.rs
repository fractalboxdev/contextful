//! `connector.source` over the generic HTTP source.

use crate::support::{request, source, Never, Response, Server};
use contextful_connectors::http::{ConfigError, HttpConfig, PAGE_CAP};
use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::Source;
use contextful_core::run::{FailureTag, RunError};
use serde_json::{json, Value};

fn ids(rows: &[serde_json::Map<String, Value>]) -> Vec<String> {
    rows.iter().map(|r| r["id"].as_str().unwrap_or_default().to_string()).collect()
}

/// The generic HTTP source binds credentials through a `headers` table whose values are templates in the
/// {{connector.reference.value-template}} grammar, hydrated per read.
// spec: connector.source.http-headers@7077d7f3
#[test]
fn header_templates_hydrate_onto_each_read() {
    let vendor = Server::start(|_| Response::json(200, "[{\"id\":\"a\"}]"));
    let s = source(
        json!({"endpoint": vendor.url("/v1/items"), "headers": {"Authorization": "Bearer ${secret://vendor-token}", "X-Account": "acct-${secret://account}"}}),
        vec![("vendor-token", "tok-7c1f"), ("account", "4471")],
    );
    for _ in 0..2 {
        assert_eq!(ids(&s.walk(&request(None), &Never).unwrap()), ["a"]);
    }
    let seen = vendor.received("/v1/items");
    assert_eq!(seen.len(), 2);
    for r in &seen {
        assert_eq!((r.method.as_str(), r.body.len()), ("GET", 0));
        assert_eq!(r.header("authorization"), Some("Bearer tok-7c1f"));
        assert_eq!(r.header("x-account"), Some("acct-4471"));
    }
    assert_eq!(s.client.sensitive_headers(), ["Authorization", "X-Account"]);
    // A template naming no bound credential refuses before any request.
    let unbound = source(json!({"endpoint": vendor.url("/v1/items"), "headers": {"Authorization": "Bearer ${secret://missing}"}}), vec![]);
    let f = unbound.walk(&request(None), &Never).unwrap_err();
    assert!(f.message.contains("SecretUnresolvedReference"), "{f}");
    assert_eq!(vendor.received("/v1/items").len(), 2);
}

/// A JSON record path, a pagination shape, or a decode key declared against a format that does not read it raises
/// `ConnectorFormatKeyRejected` at build.
// spec: connector.source.format-key-mismatch@27debf11
#[test]
fn a_json_key_on_another_format_is_refused_at_build() {
    for (format, key) in [("csv", "records"), ("jsonl", "next_cursor_path"), ("csv", "next_url_path")] {
        let cfg = json!({"endpoint": "https://api.vendor.example/v1", "format": format, key: "/data"});
        match HttpConfig::parse(&cfg) {
            Err(ConfigError::Connector(ConnectorError::ConnectorFormatKeyRejected(m))) => assert!(m.contains(key) && m.contains(format), "{m}"),
            other => panic!("{format}/{key}: {other:?}"),
        }
    }
    assert!(HttpConfig::parse(&json!({"endpoint": "https://api.vendor.example/v1", "format": "json", "records": "/data"})).is_ok());
    assert!(HttpConfig::parse(&json!({"endpoint": "https://api.vendor.example/v1", "format": "csv", "page_param": "p"})).is_ok());
}

/// A walk declares exactly one pagination shape: a page parameter with a start page, a next-cursor path with a
/// cursor parameter, a next-URL path, or link-header following.
// spec: connector.source.pagination@472ace8f
#[test]
fn each_pagination_shape_walks_to_its_end() {
    // Page parameter from a start page, ending on an empty page.
    let pages = Server::start(|r| match r.query("p").as_deref() {
        Some("5") => Response::json(200, "{\"data\":[{\"id\":\"p5\"}]}"),
        Some("6") => Response::json(200, "{\"data\":[{\"id\":\"p6\"}]}"),
        _ => Response::json(200, "{\"data\":[]}"),
    });
    let s = source(json!({"endpoint": pages.url("/v1"), "records": "/data", "page_param": "p", "start_page": 5}), vec![]);
    assert_eq!(ids(&s.walk(&request(None), &Never).unwrap()), ["p5", "p6"]);

    // Next cursor read at a pointer and sent back in a parameter.
    let cursor = Server::start(|r| match r.query("after").as_deref() {
        None => Response::json(200, "{\"data\":[{\"id\":\"c1\"}],\"meta\":{\"next\":\"k2\"}}"),
        Some("k2") => Response::json(200, "{\"data\":[{\"id\":\"c2\"}],\"meta\":{\"next\":null}}"),
        _ => Response::json(500, "{}"),
    });
    let s = source(json!({"endpoint": cursor.url("/v1"), "records": "/data", "next_cursor_path": "/meta/next", "cursor_param": "after"}), vec![]);
    assert_eq!(ids(&s.walk(&request(None), &Never).unwrap()), ["c1", "c2"]);

    // Next URL read at a pointer.
    let next = Server::start(|r| match r.path() {
        "/v1" => Response::json(200, "{\"data\":[{\"id\":\"u1\"}],\"next\":\"/v1/page2\"}"),
        _ => Response::json(200, "{\"data\":[{\"id\":\"u2\"}]}"),
    });
    let s = source(json!({"endpoint": next.url("/v1"), "records": "/data", "next_url_path": "/next"}), vec![]);
    assert_eq!(ids(&s.walk(&request(None), &Never).unwrap()), ["u1", "u2"]);

    // Link-header following.
    let link = Server::start(|r| match r.path() {
        "/v1" => Response { status: 200, headers: vec![("Link".into(), "</v1/2>; rel=\"next\"".into())], body: b"[{\"id\":\"l1\"}]".to_vec() },
        _ => Response::json(200, "[{\"id\":\"l2\"}]"),
    });
    let s = source(json!({"endpoint": link.url("/v1"), "link_header": true}), vec![]);
    assert_eq!(ids(&s.walk(&request(None), &Never).unwrap()), ["l1", "l2"]);
}

/// Declaring two pagination shapes raises `ConnectorPaginationAmbiguous`.
// spec: connector.source.pagination-ambiguity@7124b478
#[test]
fn two_pagination_shapes_are_refused() {
    for extra in [json!({"page_param": "p", "link_header": true}), json!({"next_cursor_path": "/n", "next_url_path": "/u"})] {
        let mut cfg = json!({"endpoint": "https://api.vendor.example/v1"});
        cfg.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        assert!(matches!(HttpConfig::parse(&cfg), Err(ConfigError::Connector(ConnectorError::ConnectorPaginationAmbiguous(_)))), "{cfg}");
    }
}

/// One walk issues at most 1000 requests.
// spec: connector.source.page-cap@ded4550b
#[test]
fn a_walk_stops_at_1000_requests() {
    assert_eq!(PAGE_CAP, 1000);
    // A vendor that always has another page.
    let vendor = Server::start(|r| Response::json(200, &format!("[{{\"id\":\"{}\"}}]", r.query("p").unwrap_or_default())));
    let s = source(json!({"endpoint": vendor.url("/v1"), "page_param": "p"}), vec![]);
    let f = s.walk(&request(None), &Never).unwrap_err();
    assert!(f.message.contains("1000 requests"), "{f}");
    assert_eq!(vendor.received("/v1").len(), 1000);
}

/// A vendor returning a token or link it already served raises `ConnectorPageLoop`.
// spec: connector.source.page-loop@e4d5bd35
#[test]
fn a_repeated_token_is_a_page_loop() {
    let vendor = Server::start(|r| match r.query("cursor").as_deref() {
        None => Response::json(200, "{\"data\":[{\"id\":\"a\"}],\"next\":\"k1\"}"),
        Some("k1") => Response::json(200, "{\"data\":[{\"id\":\"b\"}],\"next\":\"k2\"}"),
        _ => Response::json(200, "{\"data\":[{\"id\":\"c\"}],\"next\":\"k1\"}"),
    });
    let s = source(json!({"endpoint": vendor.url("/v1"), "records": "/data", "next_cursor_path": "/next"}), vec![]);
    let f = s.walk(&request(None), &Never).unwrap_err();
    assert!(f.message.starts_with("ConnectorPageLoop"), "{f}");
    assert_eq!(vendor.received("/v1").len(), 3, "the loop is caught at the first repeat");
}

/// A vendor-supplied next link is judged as a hop under {{connector.attach.weakened-hop}}.
// spec: connector.source.next-link-origin@e4dc93c0
#[test]
fn a_next_link_off_the_configured_origin_fails_the_read() {
    let elsewhere = Server::start(|_| Response::json(200, "{\"data\":[{\"id\":\"stolen\"}]}"));
    let target = elsewhere.url("/collect");
    let vendor = Server::start(move |_| Response::json(200, &format!("{{\"data\":[{{\"id\":\"a\"}}],\"next\":\"{target}\"}}")));
    let s = source(json!({"endpoint": vendor.url("/v1"), "records": "/data", "next_url_path": "/next"}), vec![]);
    let f = s.walk(&request(None), &Never).unwrap_err();
    assert!(f.message.contains("SecretRedirectOffOrigin"), "{f}");
    assert!(elsewhere.requests.lock().unwrap().is_empty(), "no request reached the other port");
}

/// A URL placeholder with no pattern to bind it raises `ConnectorPlaceholderUnbound` at build.
// spec: connector.source.placeholder-unbound@4b5e57a5
#[test]
fn a_placeholder_other_than_the_table_is_refused_at_build() {
    match HttpConfig::parse(&json!({"endpoint": "https://api.vendor.example/v1/{account}/items"})) {
        Err(ConfigError::Connector(ConnectorError::ConnectorPlaceholderUnbound(m))) => assert!(m.contains("{account}"), "{m}"),
        other => panic!("{other:?}"),
    }
    let c = HttpConfig::parse(&json!({"endpoint": "https://api.vendor.example/v1/{table}/items"})).unwrap();
    assert_eq!(c.table_url("sales orders").unwrap().path(), "/v1/sales%20orders/items");
}

/// A source config key outside the set that source enumerates raises `PipelineUnknownConfigKey` before any I/O,
/// naming the key and the accepted keys; request headers and the forwarded guest table are exempt.
// spec: run.declare.config-key@427de929
#[test]
fn an_unknown_config_key_is_refused_naming_the_accepted_keys() {
    match HttpConfig::parse(&json!({"endpoint": "https://api.vendor.example/v1", "paginate": "yes"})) {
        Err(ConfigError::Run(RunError::PipelineUnknownConfigKey(m))) => {
            assert!(m.contains("`paginate`"), "{m}");
            assert!(m.contains("endpoint") && m.contains("page_param") && m.contains("headers"), "{m}");
        }
        other => panic!("{other:?}"),
    }
    // Any header name is the vendor's vocabulary.
    assert!(HttpConfig::parse(&json!({"endpoint": "https://api.vendor.example/v1", "headers": {"X-Paginate": "yes", "Accept": "application/json"}})).is_ok());
}

/// Input a parser cannot read raises `PipelineUnreadableInput`, naming the path and the position inside it,
/// permanent against the retry schedule and failing one table's pull.
// spec: run.land.unreadable-input@e97b3caa
#[test]
fn an_unreadable_body_refuses_naming_path_and_position() {
    let vendor = Server::start(|_| Response::json(200, "{\"data\": [\n  {\"id\": \"a\"},\n  {\"id\": oops}\n]}"));
    let mut s = source(json!({"endpoint": vendor.url("/v1/items"), "records": "/data"}), vec![]);
    let f = s.pull(&request(None), &Never).unwrap_err();
    assert!(f.message.starts_with("PipelineUnreadableInput"), "{f}");
    assert!(f.message.contains("/v1/items") && f.message.contains("line 3"), "{f}");
    assert_eq!(f.tag, FailureTag::Permanent);
    assert!(f.deterministic, "a parse refusal spends no retry");
}
