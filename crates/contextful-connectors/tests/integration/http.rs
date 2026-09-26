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
    // Every placeholder is checked, not the first alone.
    match HttpConfig::parse(&json!({"endpoint": "https://api.vendor.example/v1/{table}/{account}"})) {
        Err(ConfigError::Connector(ConnectorError::ConnectorPlaceholderUnbound(m))) => assert!(m.contains("{account}"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_table_pattern_binds_each_segment_percent_encoded() {
    let c = HttpConfig::parse(&json!({
        "endpoint": "https://api.vendor.example/repos/{owner}/{repo}/{stream}",
        "table_pattern": "{stream}/{owner}/{repo}"
    }))
    .unwrap();
    assert_eq!(c.table_url("issues/acme/wid gets").unwrap().path(), "/repos/acme/wid%20gets/issues");
    assert_eq!(c.table_url("pulls/acme/tools").unwrap().path(), "/repos/acme/tools/pulls");
    // A literal segment matches exactly and binds nothing.
    let lit = HttpConfig::parse(&json!({"endpoint": "https://api.vendor.example/v2/{name}", "table_pattern": "crm/{name}"})).unwrap();
    assert_eq!(lit.table_url("crm/deals?all").unwrap().path(), "/v2/deals%3Fall");
    // A placeholder the pattern does not name is refused at build.
    match HttpConfig::parse(&json!({"endpoint": "https://api.vendor.example/{owner}/{account}", "table_pattern": "{owner}/{repo}"})) {
        Err(ConfigError::Connector(ConnectorError::ConnectorPlaceholderUnbound(m))) => assert!(m.contains("{account}"), "{m}"),
        other => panic!("{other:?}"),
    }
    // A pattern naming one field twice, mixing text into a placeholder segment, empty, or
    // claiming the whole-name `{table}` as a field is refused at build.
    for pattern in ["{a}/{a}", "v{a}/{b}", "", "{}/x", "{table}/{b}"] {
        assert!(matches!(HttpConfig::parse(&json!({"endpoint": "https://api.vendor.example/x", "table_pattern": pattern})), Err(ConfigError::Run(RunError::Invalid(_)))), "{pattern}");
    }
    // A field the endpoint never places would land one stream under many table names.
    match HttpConfig::parse(&json!({"endpoint": "https://api.vendor.example/{stream}", "table_pattern": "{stream}/{owner}"})) {
        Err(ConfigError::Run(RunError::Invalid(m))) => assert!(m.contains("{owner}"), "{m}"),
        other => panic!("{other:?}"),
    }
}

/// A source binding any credential carries one non-wildcard host, checked at validation and at session open; a
/// wildcard entry, or an endpoint host a table name binds, beside a bound credential raises `SecretWildcardHost`.
// spec: connector.attach.bound-host@a014ba6b
#[test]
fn a_credentialed_endpoint_host_is_never_bound_from_a_table_name() {
    let auth = json!({"Authorization": "Bearer ${secret://vendor-token}"});
    for (endpoint, pattern) in [
        ("https://{host}/api/{stream}", Some("{host}/{stream}")),
        ("https://{table}.vendor.example/v1", None),
        ("https://api.{region}.vendor.example/{stream}", Some("{region}/{stream}")),
        ("{scheme}://api.vendor.example/{stream}", Some("{scheme}/{stream}")),
    ] {
        let mut cfg = json!({"endpoint": endpoint, "headers": auth});
        if let Some(p) = pattern {
            cfg["table_pattern"] = json!(p);
        }
        match HttpConfig::parse(&cfg) {
            Err(ConfigError::Connector(ConnectorError::SecretWildcardHost(m))) => assert!(m.contains("table"), "{m}"),
            other => panic!("{endpoint}: {other:?}"),
        }
    }
    // A table name binds the path and the query of a credentialed source.
    let c = HttpConfig::parse(&json!({"endpoint": "https://api.vendor.example/{stream}?owner={owner}", "table_pattern": "{stream}/{owner}", "headers": auth})).unwrap();
    assert_eq!(c.table_url("issues/acme").unwrap().as_str(), "https://api.vendor.example/issues?owner=acme");
    assert!(c.allowlist("issues/acme").unwrap().permits("api.vendor.example"));
    // At session open, a wildcard entry or a second host beside the credential refuses too.
    use contextful_core::connector::attach::Allowlist;
    assert!(matches!(Allowlist::parse(&["*.vendor.example"]).unwrap().check_bound(), Err(ConnectorError::SecretWildcardHost(_))));
    assert!(matches!(Allowlist::parse(&["a.vendor.example", "b.vendor.example"]).unwrap().check_bound(), Err(ConnectorError::SecretWildcardHost(_))));
    // Without a bound credential, a table may still name its host.
    assert!(HttpConfig::parse(&json!({"endpoint": "https://{table}.vendor.example/v1"})).is_ok());
}

/// A table not matching the declared pattern, or binding `.` or `..` into a placeholder, raises `ConnectorTableUnmatched`
/// ahead of any request.
// spec: connector.source.table-unmatched@8c1761ac
#[test]
fn a_table_off_the_pattern_is_refused_before_any_request() {
    let vendor = Server::start(|_| Response::json(200, "[{\"id\":\"a\"}]"));
    let config = HttpConfig::parse(&json!({"endpoint": vendor.url("/repos/{owner}/{repo}/{stream}"), "table_pattern": "{stream}/{owner}/{repo}"})).unwrap();
    // A `.` or `..` field would be a dot segment the URL parser folds into another endpoint.
    for table in ["issues/acme", "issues/acme/widgets/extra", "issues//widgets", "issues/acme/", "issues/acme/..", "issues/./x", "issues/../admin"] {
        match contextful_connectors::http::HttpSource::new(config.clone(), table, crate::support::resolver(vec![])) {
            Err(ConnectorError::ConnectorTableUnmatched(m)) => assert!(m.contains(table) && m.contains("{stream}/{owner}/{repo}"), "{m}"),
            Err(other) => panic!("{table}: {other:?}"),
            Ok(_) => panic!("{table}: bound"),
        }
        assert!(matches!(config.allowlist(table), Err(ConnectorError::ConnectorTableUnmatched(_))), "{table}");
    }
    let lit = HttpConfig::parse(&json!({"endpoint": vendor.url("/v2/{name}"), "table_pattern": "crm/{name}"})).unwrap();
    assert!(matches!(lit.table_url("erp/deals"), Err(ConnectorError::ConnectorTableUnmatched(_))));
    // The whole-name `{table}` refuses a dot segment the same way.
    let whole = HttpConfig::parse(&json!({"endpoint": vendor.url("/v1/{table}")})).unwrap();
    for table in ["..", "."] {
        match contextful_connectors::http::HttpSource::new(whole.clone(), table, crate::support::resolver(vec![])) {
            Err(ConnectorError::ConnectorTableUnmatched(m)) => assert!(m.contains(&format!("`{table}`")), "{m}"),
            Err(other) => panic!("{table}: {other:?}"),
            Ok(_) => panic!("{table}: bound"),
        }
    }
    assert_eq!(whole.table_url("..x").unwrap().path(), "/v1/..x");
    assert!(vendor.requests.lock().unwrap().is_empty(), "no request reached the vendor");
    // A matching table walks.
    let s = contextful_connectors::http::HttpSource::new(config, "issues/acme/widgets", crate::support::resolver(vec![])).unwrap();
    assert_eq!(ids(&s.walk(&request(None), &Never).unwrap()), ["a"]);
    assert_eq!(vendor.received("/repos/acme/widgets/issues").len(), 1);
}

#[test]
fn the_allowlist_is_the_host_each_table_reaches() {
    let c = HttpConfig::parse(&json!({"endpoint": "https://{table}.vendor.example/v1"})).unwrap();
    assert!(c.allowlist("eu").unwrap().permits("eu.vendor.example"));
    assert!(!c.allowlist("eu").unwrap().permits("us.vendor.example"));
    assert!(!c.allowlist("eu").unwrap().permits("table.vendor.example"));
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

/// A lease provider minting `lease-<n>` for 20 s at a time on a clock the vendor moves.
struct Minting {
    clock: std::sync::Arc<std::sync::atomic::AtomicI64>,
    minted: std::sync::atomic::AtomicUsize,
}

impl contextful_core::connector::resolve::Provider for Minting {
    fn name(&self) -> &str {
        "lease"
    }

    fn answer(&self, _: &contextful_core::connector::reference::SecretName) -> Result<Option<contextful_core::connector::resolve::Answer>, contextful_core::run::Failure> {
        let n = self.minted.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let now = contextful_core::time::Instant::from_unix_secs(self.clock.load(std::sync::atomic::Ordering::SeqCst)).unwrap();
        Ok(Some(contextful_core::connector::resolve::Answer {
            value: contextful_core::connector::reference::Hydrated::new(format!("lease-{n}")),
            expires_at: Some(now.plus_secs(20)),
        }))
    }
}

struct Ticking(std::sync::Arc<std::sync::atomic::AtomicI64>);

impl contextful_core::ports::Clock for Ticking {
    fn now(&self) -> contextful_core::time::Instant {
        contextful_core::time::Instant::from_unix_secs(self.0.load(std::sync::atomic::Ordering::SeqCst)).unwrap()
    }
}

#[test]
fn a_long_walk_rehydrates_a_lease_before_it_expires() {
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
    use std::sync::Arc;
    let clock = Arc::new(AtomicI64::new(1_900_000_000));
    let moved = clock.clone();
    let server = Server::start(move |r| {
        // Every page takes 30 s of the clock, longer than one lease lives.
        moved.fetch_add(30, Ordering::SeqCst);
        let page: usize = r.query("page").and_then(|p| p.parse().ok()).unwrap_or(1);
        let body = if page <= 3 { format!("[{{\"id\":{page}}}]") } else { "[]".to_string() };
        Response::json(200, &body)
    });
    let provider = Arc::new(Minting { clock: clock.clone(), minted: AtomicUsize::new(0) });
    let resolver = Arc::new(contextful_runtime::Resolver::new(vec![provider], false, Arc::new(Ticking(clock))));
    let config = HttpConfig::parse(&json!({
        "endpoint": server.url("/v1/items"), "page_param": "page",
        "headers": {"Authorization": "Bearer ${secret://vendor-token}"}
    }))
    .unwrap();
    let mut src = contextful_connectors::http::HttpSource::new(config, "t", resolver).unwrap();
    src.pull(&request(None), &Never).unwrap();
    let sent: Vec<String> = server.requests.lock().unwrap().iter().map(|r| r.header("authorization").unwrap_or_default().to_string()).collect();
    assert_eq!(sent, ["Bearer lease-1", "Bearer lease-2", "Bearer lease-3", "Bearer lease-4"], "each page carries a live lease");
}
