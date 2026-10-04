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

/// A read with 201 expansion pointers refuses before its first follow-up request.
#[test]
fn expansion_rejects_201_followups_before_the_first() {
    let vendor = Server::start(|r| match r.path() {
        "/index" => {
            let rows: Vec<Value> = (0..201).map(|id| json!({"id": id, "detail_url": format!("/detail/{id}")})).collect();
            Response::json(200, &serde_json::to_string(&rows).unwrap())
        }
        _ => Response::json(200, "{\"description\":\"detail\"}"),
    });
    let s = source(
        json!({"endpoint": vendor.url("/index"), "expansion": {"pointer_column": "detail_url", "target_column": "detail"}}),
        vec![],
    );
    let failure = s.walk(&request(None), &Never).unwrap_err();
    assert!(failure.message.contains("200"), "{failure}");
    assert_eq!(vendor.received("/index").len(), 1);
    assert_eq!(vendor.requests.lock().unwrap().len(), 1, "no detail request follows an over-budget index");
}

/// One expansion pointer fetches its document into the declared target column.
#[test]
fn expansion_follows_a_row_pointer() {
    let vendor = Server::start(|r| match r.path() {
        "/index" => Response::json(200, "[{\"id\":1,\"detail_url\":\"/detail/1\"}]"),
        _ => Response::json(200, "{\"description\":\"detail\"}"),
    });
    let s = source(
        json!({"endpoint": vendor.url("/index"), "expansion": {"pointer_column": "detail_url", "target_column": "detail"}}),
        vec![],
    );
    let rows = s.walk(&request(None), &Never).unwrap();
    assert_eq!(rows[0]["detail"]["description"], "detail");
    assert_eq!(vendor.received("/detail/1").len(), 1);
}

/// Both expansion pointer forms refuse at build without issuing a request.
#[test]
fn expansion_rejects_two_pointer_forms() {
    let cfg = json!({"endpoint":"https://api.vendor.example/index", "expansion":{"pointer_column":"url", "url_template":"https://api.vendor.example/{id}", "target_column":"detail"}});
    assert!(format!("{}", HttpConfig::parse(&cfg).unwrap_err()).contains("ConnectorPointerAmbiguous"));
}

/// A malformed row pointer and an occupied destination each refuse before detail I/O.
#[test]
fn expansion_preflights_every_row_pointer() {
    for row in [json!({"id": 1}), json!({"id": 1, "url": "/detail/1", "detail": "occupied"})] {
        let vendor = Server::start(move |r| match r.path() {
            "/index" => Response::json(200, &json!([{"id": 0, "url": "/detail/0"}, row]).to_string()),
            _ => Response::json(200, "{\"ok\":true}"),
        });
        let s = source(json!({"endpoint": vendor.url("/index"), "expansion": {"pointer_column":"url", "target_column":"detail"}}), vec![]);
        let failure = s.walk(&request(None), &Never).unwrap_err();
        assert!(failure.message.contains("ConnectorPointerColumnMissing") || failure.message.contains("ConnectorTargetColumnOccupied"), "{failure}");
        assert_eq!(vendor.requests.lock().unwrap().len(), 1);
    }
}

/// A follow-up failure refuses the complete index read.
#[test]
fn expansion_fails_the_read_on_a_failed_followup() {
    let vendor = Server::start(|r| match r.path() {
        "/index" => Response::json(200, "[{\"id\":1,\"url\":\"/detail/1\"}]"),
        _ => Response::json(503, "{}"),
    });
    let s = source(json!({"endpoint": vendor.url("/index"), "expansion": {"pointer_column":"url", "target_column":"detail"}}), vec![]);
    let failure = s.walk(&request(None), &Never).unwrap_err();
    assert!(failure.message.contains("ConnectorExpansionFailed"), "{failure}");
}

/// Row templates percent-encode scalars, and a template cannot choose its own host.
#[test]
fn expansion_template_binds_row_values_under_the_source_host() {
    for template in ["https://{id}.vendor.example/detail", "https://api.vendor.example/detail"] {
        let cfg = json!({"endpoint":"https://api.vendor.example/index", "expansion":{"url_template":template, "target_column":"detail"}});
        assert!(format!("{}", HttpConfig::parse(&cfg).unwrap_err()).contains("ConnectorTemplateRejected"));
    }
    let vendor = Server::start(|r| match r.path() {
        "/index" => Response::json(200, "[{\"id\":\"a/b\"}]"),
        _ => Response::json(200, "{\"ok\":true}"),
    });
    let s = source(json!({"endpoint": vendor.url("/index"), "expansion": {"url_template": vendor.url("/detail/{id}"), "target_column":"detail"}}), vec![]);
    assert_eq!(s.walk(&request(None), &Never).unwrap()[0]["detail"]["ok"], true);
    assert_eq!(vendor.received("/detail/a%2Fb").len(), 1);
}

/// A row below the watermark costs no follow-up request.
#[test]
fn expansion_runs_after_the_watermark_filter() {
    let vendor = Server::start(|r| match r.path() {
        "/index" => Response::json(200, "[{\"id\":1,\"at\":\"2024-01-01T00:00:00Z\",\"url\":\"/detail/old\"},{\"id\":2,\"at\":\"2026-01-01T00:00:00Z\",\"url\":\"/detail/new\"}]"),
        _ => Response::json(200, "{\"ok\":true}"),
    });
    let s = source(json!({"endpoint": vendor.url("/index"), "expansion": {"pointer_column":"url", "target_column":"detail"}}), vec![]).watermarked();
    let rows = s.walk(&request(Some(json!({"field":"at", "at":"2025-01-01T00:00:00Z"}))), &Never).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], 2);
    assert!(vendor.received("/detail/old").is_empty());
    assert_eq!(vendor.received("/detail/new").len(), 1);
}

/// A pointer outside the configured host cannot receive source headers.
#[test]
fn expansion_keeps_the_existing_outbound_host_policy() {
    let outside = Server::start(|_| Response::json(200, "{\"ok\":true}"));
    let pointer = outside.url("/detail");
    let vendor = Server::start(move |_| Response::json(200, &json!([{"id":1,"url":pointer}]).to_string()));
    let s = source(json!({"endpoint": vendor.url("/index"), "expansion": {"pointer_column":"url", "target_column":"detail"}}), vec![]);
    assert!(s.walk(&request(None), &Never).unwrap_err().message.contains("ConnectorExpansionFailed"));
    assert!(outside.received("/detail").is_empty());
}

/// A declared non-UTF-8 label decodes CSV and rejects invalid bytes for that label.
#[test]
fn csv_declared_encoding_decodes_and_refuses_invalid_bytes() {
    let vendor = Server::start(|_| Response { status: 200, headers: vec![], body: b"name\ncaf\xe9\n".to_vec() });
    let s = source(json!({"endpoint": vendor.url("/rows"), "format":"csv", "encoding":"windows-1252"}), vec![]);
    assert_eq!(s.walk(&request(None), &Never).unwrap()[0]["name"], "café");

    let invalid = Server::start(|_| Response { status: 200, headers: vec![], body: b"name\n\x81\n".to_vec() });
    let s = source(json!({"endpoint": invalid.url("/rows"), "format":"csv", "encoding":"shift_jis"}), vec![]);
    assert!(s.walk(&request(None), &Never).unwrap_err().message.contains("ConnectorEncodingInvalid"));
}

/// A CSV watermark compares either an RFC 3339 instant or fixed-width decimal digits.
#[test]
fn csv_watermark_rejects_variable_width_clocks() {
    let vendor = Server::start(|_| Response::json(200, "id,at\n1,2025-01-01T00:00:00Z\n2,12\n"));
    let s = source(json!({"endpoint": vendor.url("/rows"), "format":"csv"}), vec![]).watermarked();
    let failure = s.walk(&request(Some(json!({"field":"at", "at":"2024-01-01T00:00:00Z"}))), &Never).unwrap_err();
    assert!(failure.message.contains("ConnectorClockColumnRejected"), "{failure}");
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
    // A workbook is one document: no pagination shape reads it.
    for (key, value) in [("page_param", json!("page")), ("link_header", json!(true))] {
        match HttpConfig::parse(&json!({"endpoint": "https://api.vendor.example/b.xlsx", "format": "xlsx", key: value})) {
            Err(ConfigError::Connector(ConnectorError::ConnectorFormatKeyRejected(m))) => assert!(m.contains(key) && m.contains("xlsx"), "{m}"),
            other => panic!("xlsx/{key}: {other:?}"),
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

/// A URL or bound-column placeholder naming neither `{table}` nor a table-pattern field raises
/// `ConnectorPlaceholderUnbound` at build.
// spec: connector.source.placeholder-unbound@f354f202
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
    let resolver = Arc::new(contextful_outbound::Resolver::new(vec![provider], false, Arc::new(Ticking(clock))));
    let config = HttpConfig::parse(&json!({
        "endpoint": server.url("/v1/items"), "page_param": "page",
        "headers": {"Authorization": "Bearer ${secret://vendor-token}"}
    }))
    .unwrap();
    let src = contextful_connectors::http::HttpSource::new(config, "t", resolver).unwrap();
    src.walk(&request(None), &Never).unwrap();
    let sent: Vec<String> = server.requests.lock().unwrap().iter().map(|r| r.header("authorization").unwrap_or_default().to_string()).collect();
    assert_eq!(sent, ["Bearer lease-1", "Bearer lease-2", "Bearer lease-3", "Bearer lease-4"], "each page carries a live lease");
}

/// The pulled bytes of one pull: rows, cursor and whether another follows.
fn pulled(s: &mut contextful_connectors::http::HttpSource, position: Option<Value>) -> Result<(Vec<String>, Option<Value>, bool), contextful_core::run::Failure> {
    let v: Value = serde_json::from_slice(&s.pull(&request(position), &Never)?).unwrap();
    let rows: Vec<serde_json::Map<String, Value>> = serde_json::from_value(v["rows"].clone()).unwrap();
    Ok((ids(&rows), v.get("cursor").cloned(), v["more"].as_bool().unwrap_or(false)))
}

/// One pull reads one page and pairs it with the position of the page after it.
#[test]
fn one_pull_reads_one_page_and_carries_the_next_as_its_position() {
    let vendor = Server::start(|r| match r.query("after").as_deref() {
        None => Response::json(200, "{\"data\":[{\"id\":\"c1\"}],\"next\":\"k2\"}"),
        Some("k2") => Response::json(200, "{\"data\":[{\"id\":\"c2\"}],\"next\":\"k3\"}"),
        Some("k3") => Response::json(200, "{\"data\":[{\"id\":\"c3\"}],\"next\":null}"),
        _ => Response::json(500, "{}"),
    });
    let config = json!({"endpoint": vendor.url("/v1"), "records": "/data", "next_cursor_path": "/next", "cursor_param": "after"});
    let mut s = source(config.clone(), vec![]);
    assert_eq!(pulled(&mut s, None).unwrap(), (vec!["c1".to_string()], Some(json!({"next": "k2"})), true));
    assert_eq!(pulled(&mut s, Some(json!({"next": "k2"}))).unwrap(), (vec!["c2".to_string()], Some(json!({"next": "k3"})), true));
    // The walk's last page names the start: the next run walks from the first page again.
    assert_eq!(pulled(&mut s, Some(json!({"next": "k3"}))).unwrap(), (vec!["c3".to_string()], Some(json!({"next": null})), false));
    assert_eq!(vendor.received("/v1").len(), 3, "one request per pull");

    // A fresh source resumes from a stored page position without re-reading the pages before it.
    let mut resumed = source(config.clone(), vec![]);
    assert_eq!(pulled(&mut resumed, Some(json!({"next": "k3"}))).unwrap().0, ["c3"]);
    assert_eq!(pulled(&mut resumed, Some(json!({"next": null}))).unwrap().0, ["c1"], "a finished walk reads from the start");
    let targets: Vec<Option<String>> = vendor.received("/v1").iter().map(|r| r.query("after")).collect();
    assert_eq!(targets, [None, Some("k2".into()), Some("k3".into()), Some("k3".into()), None]);

    // A position that is no page position refuses before any request.
    let f = pulled(&mut source(config, vec![]), Some(json!({"next": 7}))).unwrap_err();
    assert_eq!(f.tag, FailureTag::Config, "{f}");
    assert_eq!(vendor.received("/v1").len(), 5);
}

/// Under page-number, next-cursor or no pagination and without an incremental field, one generic HTTP source pull
/// reads one page and carries the next page's token as its position. The walk's last page carries a position naming the first page.
// spec: connector.source.http-page-pull@8b42240d
#[test]
fn a_page_number_walk_pulls_page_by_page_and_ends_naming_the_first_page() {
    let pages = Server::start(|r| match r.query("p").as_deref() {
        Some("1") => Response::json(200, "[{\"id\":\"p1\"}]"),
        _ => Response::json(200, "[]"),
    });
    let mut p = source(json!({"endpoint": pages.url("/v1"), "page_param": "p"}), vec![]);
    assert_eq!(pulled(&mut p, None).unwrap(), (vec!["p1".to_string()], Some(json!({"next": "2"})), true));
    assert_eq!(pulled(&mut p, Some(json!({"next": "2"}))).unwrap(), (vec![], Some(json!({"next": null})), false));
    assert_eq!(pulled(&mut p, Some(json!({"next": null}))).unwrap().0, ["p1"], "the next run walks from the first page");
    let asked: Vec<Option<String>> = pages.received("/v1").iter().map(|r| r.query("p")).collect();
    assert_eq!(asked, [Some("1".into()), Some("2".into()), Some("1".into())]);
}

#[test]
fn an_http_page_marks_only_the_terminal_snapshot_complete() {
    let vendor = Server::start(|r| match r.query("p").as_deref() {
        Some("1") => Response::json(200, "[{\"id\":\"old\"}]"),
        _ => Response::json(200, "[]"),
    });
    let mut s = source(json!({"endpoint": vendor.url("/items"), "page_param": "p"}), vec![]);
    let first: Value = serde_json::from_slice(&s.pull(&request(None), &Never).unwrap()).unwrap();
    assert_eq!(first["more"], json!(true));
    assert_eq!(first["snapshot_complete"], json!(false));
    let last: Value = serde_json::from_slice(&s.pull(&request(first.get("cursor").cloned()), &Never).unwrap()).unwrap();
    assert_eq!(last["rows"], json!([]));
    assert_eq!(last["more"], json!(false));
    assert_eq!(last["snapshot_complete"], json!(true));

    let mut unpaged = source(json!({"endpoint": vendor.url("/empty")}), vec![]);
    let empty: Value = serde_json::from_slice(&unpaged.pull(&request(None), &Never).unwrap()).unwrap();
    assert_eq!(empty["snapshot_complete"], json!(true));
}

/// Under next-URL or Link-header pagination, one generic HTTP source pull walks every page, and no position carries
/// a page URL.
// spec: connector.source.http-url-walk@ef55a50e
#[test]
fn a_next_url_walk_keeps_the_next_urls_query_out_of_every_position() {
    for (key, value) in [("next_url_path", json!("/next")), ("link_header", json!(true))] {
        let secret = "SEKRET123";
        let fail = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let failing = fail.clone();
        let vendor = Server::start(move |r| match r.path() {
            "/v1" => Response {
                status: 200,
                headers: vec![("Link".into(), format!("</v1/2?access_token={secret}>; rel=\"next\""))],
                body: format!("{{\"data\":[{{\"id\":\"a\"}}],\"next\":\"/v1/2?access_token={secret}\"}}").into_bytes(),
            },
            _ if failing.load(std::sync::atomic::Ordering::SeqCst) => Response::json(400, "{}"),
            _ => Response::json(200, "{\"data\":[{\"id\":\"b\"}]}"),
        });
        let mut config = json!({"endpoint": vendor.url("/v1"), "records": "/data"});
        config[key] = value.clone();
        let mut s = source(config.clone(), vec![]);
        // A page failing mid-walk leaves no pulled bytes behind, and its message carries no query.
        let f = s.pull(&request(None), &Never).unwrap_err();
        assert!(!f.message.contains(secret), "{key}: {f}");

        fail.store(false, std::sync::atomic::Ordering::SeqCst);
        let mut s = source(config, vec![]);
        let bytes = s.pull(&request(None), &Never).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(!text.contains(secret), "{key}: {text}");
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!((v["more"].as_bool(), v.get("cursor")), (Some(false), None), "{key}: {text}");
        assert_eq!(v["rows"].as_array().map(Vec::len), Some(2), "{key}: one pull walks every page");
    }
}

#[test]
fn an_empty_url_walk_marks_the_whole_snapshot_complete() {
    let vendor = Server::start(|_| Response::json(200, "{\"data\":[],\"next\":null}"));
    let mut s = source(json!({"endpoint": vendor.url("/items"), "records": "/data", "next_url_path": "/next"}), vec![]);
    let out: Value = serde_json::from_slice(&s.pull(&request(None), &Never).unwrap()).unwrap();
    assert_eq!(out["rows"], json!([]));
    assert_eq!(out["more"], json!(false));
    assert_eq!(out["snapshot_complete"], json!(true));
}

/// Under an incremental field, one generic HTTP source pull walks every page from the stored watermark.
// spec: connector.source.http-watermark-walk@eb93fe37
#[test]
fn a_watermarked_read_walks_every_page_in_one_pull() {
    let vendor = Server::start(|r| match r.query("p").as_deref() {
        Some("1") => Response::json(200, "[{\"id\":\"w1\",\"at\":\"2030-01-02\"}]"),
        Some("2") => Response::json(200, "[{\"id\":\"w2\",\"at\":\"2030-01-03\"}]"),
        _ => Response::json(200, "[]"),
    });
    let mut s = source(json!({"endpoint": vendor.url("/v1"), "page_param": "p", "since_param": "since"}), vec![]).watermarked();
    let (rows, cursor, more) = pulled(&mut s, Some(json!({"field": "at", "at": "2030-01-01"}))).unwrap();
    assert_eq!((rows, cursor, more), (vec!["w1".to_string(), "w2".to_string()], None, false));
    assert!(vendor.received("/v1").iter().all(|r| r.query("since").as_deref() == Some("2030-01-01")));
    let pulled: Value = serde_json::from_slice(&s.pull(&request(Some(json!({"field": "at", "at": "2030-01-01"}))), &Never).unwrap()).unwrap();
    assert_eq!(pulled["snapshot_complete"], json!(false), "a watermark is a window, not a complete snapshot");
}

const LIMITER_TOKEN: &str = "lim-7f2a";

/// A limiter granting `permits` per acquire, recording every call.
fn limiter_server(permits: u32) -> Server {
    Server::start(move |r| {
        if r.header("authorization") != Some(&format!("Bearer {LIMITER_TOKEN}")) {
            return Response::json(401, "{}");
        }
        match r.path() {
            "/acquire" => Response::json(200, &format!("{{\"decision\":\"granted\",\"permits\":{permits},\"ttl_secs\":60}}")),
            _ => Response::json(204, ""),
        }
    })
}

fn bound_limiter(limiter: &Server, permits: u32) -> std::sync::Arc<contextful_outbound::Limiter> {
    let clock = std::sync::Arc::new(contextful_core::ports::FixedClock(contextful_core::time::Instant::parse("2030-01-01T00:00:00Z").unwrap()));
    let binding = contextful_core::connector::meter::LimiterBinding::parse("vendor-app", &limiter.url(""), "secret://limiter-token", Some(permits)).unwrap();
    let resolver = crate::support::resolver(vec![("limiter-token", LIMITER_TOKEN)]);
    std::sync::Arc::new(contextful_outbound::Limiter::new(binding, resolver, "run-7", clock).unwrap())
}

fn metered_config(vendor: &Server) -> Value {
    json!({
        "endpoint": vendor.url("/v1"), "page_param": "p",
        "limiter": {"quota": "vendor-app", "class": "batch-read", "usage_headers": ["X-RateLimit-Remaining"]}
    })
}

fn paged_vendor() -> Server {
    Server::start(|r| match r.query("p").as_deref() {
        Some("1") | Some("2") => Response { status: 200, headers: vec![("X-RateLimit-Remaining".into(), "41".into())], body: b"[{\"id\":\"x\"}]".to_vec() },
        _ => Response::json(200, "[]"),
    })
}

/// The HTTP source's mediated client reserves one permit ahead of each page request, under the declared class.
#[test]
fn a_declared_limiter_reserves_one_permit_per_page() {
    use contextful_connectors::http::{HttpSource, Mediation};
    let (vendor, limiter) = (paged_vendor(), limiter_server(1));
    let bound = bound_limiter(&limiter, 1);
    let config = HttpConfig::parse(&metered_config(&vendor)).unwrap();
    let mut s = HttpSource::mediated(config, "t", crate::support::resolver(vec![]), Mediation { limiter: Some(bound.clone()), ..Mediation::default() }).unwrap();
    let mut position = None;
    loop {
        let (_, cursor, more) = pulled(&mut s, position).unwrap();
        position = cursor;
        if !more {
            break;
        }
    }
    bound.finish();
    assert_eq!(vendor.received("/v1").len(), 3);
    let acquires = limiter.received("/acquire");
    assert_eq!(acquires.len(), 3, "one permit per page");
    let body: Value = serde_json::from_slice(&acquires[0].body).unwrap();
    assert_eq!((body["quota"].as_str(), body["class"].as_str()), (Some("vendor-app"), Some("batch-read")));
    let reported: Value = serde_json::from_slice(&limiter.received("/report").last().unwrap().body).unwrap();
    assert_eq!(reported["run_id"], "run-7");
}

/// Under a declared limiter, a vendor request with no granted reservation raises `ConnectorUnmetered`: a source
/// declaring a quota with no limiter bound sends nothing.
#[test]
fn a_declared_limiter_left_unbound_sends_no_request() {
    let vendor = paged_vendor();
    let mut s = source(metered_config(&vendor), vec![]);
    let f = pulled(&mut s, None).unwrap_err();
    assert!(f.message.starts_with("ConnectorUnmetered"), "{f}");
    assert!(vendor.received("/v1").is_empty());
    // An unreachable limiter is no grant either.
    let gone = Server::start(|_| Response::json(503, "{}"));
    let bound = bound_limiter(&gone, 1);
    let mut s = contextful_connectors::http::HttpSource::mediated(
        HttpConfig::parse(&metered_config(&vendor)).unwrap(),
        "t",
        crate::support::resolver(vec![]),
        contextful_connectors::http::Mediation { limiter: Some(bound), ..Default::default() },
    )
    .unwrap();
    let f = pulled(&mut s, None).unwrap_err();
    assert!(f.message.starts_with("ConnectorUnmetered"), "{f}");
    assert!(vendor.received("/v1").is_empty());
}

/// An operator hook refusing the page request's intent spends no permit.
struct Refusing(std::sync::Mutex<Vec<contextful_outbound::Intent>>);

impl contextful_outbound::PreSendHook for Refusing {
    fn admit(&self, intent: &contextful_outbound::Intent) -> Result<(), String> {
        self.0.lock().unwrap().push(intent.clone());
        Err("outside the change window".into())
    }

    fn settle(&self, _: &contextful_outbound::Intent, _: &contextful_outbound::Outcome) {}
}

#[test]
fn an_operator_hook_composes_in_front_of_the_reservation() {
    use contextful_connectors::http::{HttpSource, Mediation};
    let (vendor, limiter) = (paged_vendor(), limiter_server(1));
    let hook = std::sync::Arc::new(Refusing(Default::default()));
    let mediation = Mediation { limiter: Some(bound_limiter(&limiter, 1)), hook: Some(hook.clone()), run_id: Some("run-7".into()), ..Mediation::default() };
    let mut s = HttpSource::mediated(HttpConfig::parse(&metered_config(&vendor)).unwrap(), "t", crate::support::resolver(vec![]), mediation).unwrap();
    let f = pulled(&mut s, None).unwrap_err();
    assert!(f.message.starts_with("ConnectorEgressRefused") && f.message.contains("outside the change window"), "{f}");
    assert!(limiter.received("/acquire").is_empty() && vendor.received("/v1").is_empty());
    let seen = hook.0.lock().unwrap();
    assert_eq!((seen[0].class.as_deref(), seen[0].run_id.as_deref()), (Some("batch-read"), Some("run-7")));
}

/// The generic HTTP source reads its scope probe from a `scope_probe` config table and carries its first
/// reference-bound header as the bound credential.
// spec: connector.source.http-scope-probe@09566d74
#[test]
fn the_scope_probe_runs_before_the_first_page() {
    let grant = std::sync::Arc::new(std::sync::Mutex::new("items.read"));
    let granted = grant.clone();
    let vendor = Server::start(move |r| match r.path() {
        "/identity" => Response { status: 200, headers: vec![("X-Granted-Scopes".into(), granted.lock().unwrap().to_string())], body: Vec::new() },
        _ => match r.query("p").as_deref() {
            Some("1") => Response::json(200, "[{\"id\":\"a\"}]"),
            _ => Response::json(200, "[]"),
        },
    });
    let config = json!({
        "endpoint": vendor.url("/v1"), "page_param": "p",
        "headers": {"Authorization": "Bearer ${secret://vendor-token}"},
        "scope_probe": {"endpoint": vendor.url("/identity"), "scopes_header": "X-Granted-Scopes", "expect": ["items.read"]}
    });
    let mut s = source(config.clone(), vec![("vendor-token", "tok-1")]);
    let (_, cursor, _) = pulled(&mut s, None).unwrap();
    pulled(&mut s, cursor).unwrap();
    let paths: Vec<String> = vendor.requests.lock().unwrap().iter().map(|r| r.path().to_string()).collect();
    assert_eq!(paths, ["/identity", "/v1", "/v1"]);
    assert_eq!(vendor.received("/identity")[0].header("authorization"), Some("Bearer tok-1"));

    *grant.lock().unwrap() = "items.read, items.write";
    let mut wide = source(config, vec![("vendor-token", "tok-1")]);
    let f = pulled(&mut wide, None).unwrap_err();
    assert!(f.message.starts_with("ConnectorScopeExceeded"), "{f}");
    assert_eq!(vendor.received("/v1").len(), 2, "no page follows a refused grant");

    // A probe with no bound credential to carry refuses at build.
    let bare = json!({"endpoint": vendor.url("/v1"), "scope_probe": {"endpoint": vendor.url("/identity"), "scopes_header": "X-Granted-Scopes", "expect": []}});
    assert!(HttpConfig::parse(&bare).is_err());
}
