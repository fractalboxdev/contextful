//! The `search` source over a loopback provider: its one allowlisted host, its paged
//! position per query, and the registry entry.

use crate::support::{request, resolver, Never, Response, Server};
use contextful_connectors::http::{ConfigError, Mediation};
use contextful_connectors::search::{SearchConfig, SearchSource, NAME};
use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::Source;
use contextful_core::time::Instant;
use serde_json::{json, Value};

fn config(base: &str) -> Value {
    json!({"url": format!("{base}/search?q={{query}}&since={{since}}"), "queries": {"a": "alpha"}, "freshness": "1d", "max_results": 5, "headers": {"Authorization": "Bearer ${secret://provider-key}"}})
}

/// Every pull of one read.
fn read(source: &mut dyn Source, position: Option<Value>) -> Vec<Value> {
    let (mut position, mut batches) = (position, Vec::new());
    loop {
        let batch: Value = serde_json::from_slice(&source.pull(&request(position.clone()), &Never).unwrap()).unwrap();
        position = batch.get("cursor").cloned();
        let more = batch["more"].as_bool().unwrap_or(false);
        batches.push(batch);
        if !more {
            return batches;
        }
    }
}

/// One query pages by its cursor under the hydrated credential, and the last batch's position holds the query's
/// watermark and no open pass.
#[test]
fn a_search_read_pages_one_query_and_commits_its_watermark() {
    let provider = Server::start(|r| match r.query("cursor").as_deref() {
        None => Response::json(200, r#"{"results": [{"url": "https://a.test", "published_at": "2030-01-01T05:00:00Z"}], "next": "c2"}"#),
        Some(_) => Response::json(200, r#"{"results": [{"url": "https://b.test", "published_at": "2030-01-01T04:00:00Z"}]}"#),
    });
    let config = SearchConfig::parse(&config(&provider.url(""))).unwrap();
    let now = Instant::parse("2030-01-02T00:00:00Z").unwrap();
    let mut source = SearchSource::mediated(config, resolver(vec![("provider-key", "key-1")]), &Mediation::default(), now).unwrap();
    let batches = read(&mut source, None);
    let rows: Vec<Value> = batches.iter().flat_map(|b| b["rows"].as_array().cloned().unwrap_or_default()).collect();
    assert_eq!(rows.len(), 2, "{batches:?}");
    assert!(rows.iter().all(|r| r["search_query"] == "a"), "{rows:?}");
    let seen = provider.received("/search");
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(seen.iter().all(|r| r.header("authorization") == Some("Bearer key-1") && r.query("since").as_deref() == Some("2030-01-01T00:00:00Z")), "{seen:?}");
    assert_eq!(batches.last().unwrap()["cursor"]["queries"]["a"], json!({"watermark": "2030-01-01T05:00:00Z"}));
    assert!(contextful_connectors::BUILT_IN.contains(&NAME), "the `search` source is compiled in");
}

/// A source opened afresh at the position a finished query's batch carries reads the next query, as a pull after
/// replayed ones does, and its last query's batch ends the read.
// spec: run.journal.replay-lands@0cc34691
#[test]
fn a_fresh_source_at_a_recorded_position_reads_the_next_query() {
    let provider = Server::start(|r| match (r.query("q").as_deref(), r.query("cursor").as_deref()) {
        (Some("alpha"), None) => Response::json(200, r#"{"results": [{"url": "https://a1.test", "published_at": "2030-01-01T05:00:00Z"}], "next": "c2"}"#),
        (Some("alpha"), Some(_)) => Response::json(200, r#"{"results": [{"url": "https://a2.test", "published_at": "2030-01-01T04:00:00Z"}]}"#),
        _ => Response::json(200, r#"{"results": [{"url": "https://b1.test", "published_at": "2030-01-01T06:00:00Z"}]}"#),
    });
    let mut declared = config(&provider.url(""));
    declared["queries"] = json!({"a": "alpha", "b": "beta"});
    let config = SearchConfig::parse(&declared).unwrap();
    let now = Instant::parse("2030-01-02T00:00:00Z").unwrap();
    let open = || SearchSource::mediated(config.clone(), resolver(vec![("provider-key", "key-1")]), &Mediation::default(), now).unwrap();

    let mut first = open();
    let pull = |source: &mut SearchSource, position: Option<Value>| -> Value { serde_json::from_slice(&source.pull(&request(position), &Never).unwrap()).unwrap() };
    let page_one = pull(&mut first, None);
    let page_two = pull(&mut first, page_one.get("cursor").cloned());
    assert_eq!(page_two["more"], true, "{page_two}");

    let resumed = pull(&mut open(), page_two.get("cursor").cloned());
    let rows = resumed["rows"].as_array().cloned().unwrap_or_default();
    assert_eq!(rows.len(), 1, "{resumed}");
    assert_eq!(rows[0]["search_query"], "b", "{resumed}");
    assert_eq!(resumed["more"], false, "the last query's batch ends the read: {resumed}");
    let alpha: Vec<_> = provider.received("/search").into_iter().filter(|r| r.query("q").as_deref() == Some("alpha")).collect();
    assert_eq!(alpha.len(), 2, "the resumed source reads no recorded page again: {alpha:?}");
}

/// A cleartext provider off loopback refuses as `ConnectorProviderOriginRejected`; a placeholder in the host fails
/// validation.
#[test]
fn a_search_provider_off_tls_or_a_templated_host_refuses() {
    match SearchConfig::parse(&config("http://search.test")) {
        Err(ConfigError::Connector(ConnectorError::ConnectorProviderOriginRejected(m))) => assert!(m.contains("search.test"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(SearchConfig::parse(&config("https://search.test")).is_ok());
    let refused = SearchConfig::parse(&config("https://{query}.test")).unwrap_err();
    assert!(refused.to_string().contains("host"), "{refused}");
}
