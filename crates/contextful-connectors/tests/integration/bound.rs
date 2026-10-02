//! `connector.source` bound columns: table-pattern fields stamped onto every fetched row.

use crate::support::{request, resolver, Never, Response, Server};
use contextful_connectors::http::{ConfigError, HttpConfig, HttpSource};
use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::Source;
use contextful_core::run::RunError;
use serde_json::{json, Value};

fn source(config: Value, table: &str) -> HttpSource {
    HttpSource::new(HttpConfig::parse(&config).unwrap(), table, resolver(vec![])).unwrap()
}

/// `bound_columns` maps a column name to a template over `{table}` and the table pattern's fields; every row a read
/// fetches carries each column at its filled value, unencoded.
// spec: connector.source.bound-columns@47bfe884
#[test]
fn every_fetched_row_carries_its_bound_columns() {
    let vendor = Server::start(|r| match r.query("page").as_deref() {
        None => Response {
            status: 200,
            headers: vec![("Link".into(), "</repos/octocat/Hello-World/issues?page=2>; rel=\"next\"".into())],
            body: br#"[{"number":7},{"number":12}]"#.to_vec(),
        },
        _ => Response::json(200, r#"[{"number":13}]"#),
    });
    let s = source(
        json!({
            "endpoint": vendor.url("/repos/{owner}/{repo}/issues"),
            "table_pattern": "{owner}/{repo}",
            "link_header": true,
            "bound_columns": {"repo_full_name": "{owner}/{repo}", "source_table": "{table}", "owner": "{owner}"}
        }),
        "octocat/Hello-World",
    );
    let rows = s.walk(&request(None), &Never).unwrap();
    assert_eq!(rows.len(), 3, "every page's rows");
    for r in &rows {
        assert_eq!(r["repo_full_name"], json!("octocat/Hello-World"), "a bound value lands as written, not percent-encoded");
        assert_eq!(r["source_table"], json!("octocat/Hello-World"));
        assert_eq!(r["owner"], json!("octocat"));
    }
    assert_eq!(vendor.received("/repos/octocat/Hello-World/issues").len(), 2);
}

#[test]
fn a_page_by_page_pull_stamps_its_bound_columns_on_every_page() {
    let vendor = Server::start(|r| match r.query("page").as_deref() {
        Some("1") => Response::json(200, r#"[{"number":7},{"number":12}]"#),
        Some("2") => Response::json(200, r#"[{"number":13}]"#),
        _ => Response::json(200, "[]"),
    });
    let mut s = source(
        json!({
            "endpoint": vendor.url("/repos/{owner}/{repo}/issues"),
            "table_pattern": "{owner}/{repo}",
            "page_param": "page",
            "bound_columns": {"repo_full_name": "{owner}/{repo}"}
        }),
        "octocat/Hello-World",
    );
    let mut position = None;
    let mut numbers = Vec::new();
    loop {
        let out: Value = serde_json::from_slice(&s.pull(&request(position), &Never).unwrap()).unwrap();
        for r in out["rows"].as_array().unwrap() {
            assert_eq!(r["repo_full_name"], json!("octocat/Hello-World"), "{r}");
            numbers.push(r["number"].as_u64().unwrap());
        }
        if out["more"] != json!(true) {
            break;
        }
        position = Some(out["cursor"].clone());
    }
    assert_eq!(numbers, [7, 12, 13]);
    let asked: Vec<Option<String>> = vendor.received("/repos/octocat/Hello-World/issues").iter().map(|r| r.query("page")).collect();
    assert_eq!(asked, [Some("1".into()), Some("2".into()), Some("3".into())], "one page per pull");
}

#[test]
fn a_bound_column_placeholder_off_the_pattern_is_refused_at_build() {
    let cfg = json!({
        "endpoint": "https://api.vendor.example/repos/{owner}/{repo}",
        "table_pattern": "{owner}/{repo}",
        "bound_columns": {"org": "{organization}"}
    });
    match HttpConfig::parse(&cfg) {
        Err(ConfigError::Connector(ConnectorError::ConnectorPlaceholderUnbound(m))) => assert!(m.contains("{organization}") && m.contains("org"), "{m}"),
        other => panic!("{other:?}"),
    }
    // Without a pattern, `{table}` alone binds.
    let bare = json!({"endpoint": "https://api.vendor.example/{table}", "bound_columns": {"t": "{table}"}});
    assert!(HttpConfig::parse(&bare).is_ok());
    let off = json!({"endpoint": "https://api.vendor.example/{table}", "bound_columns": {"o": "{owner}"}});
    assert!(matches!(HttpConfig::parse(&off), Err(ConfigError::Connector(ConnectorError::ConnectorPlaceholderUnbound(_)))));
    // Each value is a template string.
    let typed = json!({"endpoint": "https://api.vendor.example/{table}", "bound_columns": {"n": 7}});
    assert!(matches!(HttpConfig::parse(&typed), Err(ConfigError::Run(RunError::Invalid(_)))));
}

/// A fetched row already carrying a bound column raises `ConnectorBoundColumnOccupied`, failing the read.
// spec: connector.source.bound-column-occupied@775dbb4a
#[test]
fn a_fetched_row_already_carrying_a_bound_column_refuses_the_read() {
    let vendor = Server::start(|_| Response::json(200, r#"[{"number":7,"repo_full_name":"someone/else"}]"#));
    let s = source(
        json!({"endpoint": vendor.url("/repos/{owner}/{repo}/issues"), "table_pattern": "{owner}/{repo}", "bound_columns": {"repo_full_name": "{owner}/{repo}"}}),
        "octocat/Hello-World",
    );
    let f = s.walk(&request(None), &Never).unwrap_err();
    assert!(f.message.starts_with("ConnectorBoundColumnOccupied") && f.message.contains("repo_full_name"), "{f}");
}
