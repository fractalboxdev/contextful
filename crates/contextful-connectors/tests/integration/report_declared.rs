//! The declared HTTP report vendor over a loopback server: its one allowlisted host, its
//! hydrated credential, a request that draws no answer, and the registry entry.

use crate::support::{request, resolver, Never, Response, Server};
use contextful_connectors::http::Mediation;
use contextful_connectors::report::declared::{DeclaredVendor, ReportDeclaration};
use contextful_connectors::report::{ReportSource, NAME};
use contextful_core::run::ports::Source;
use contextful_core::run::FailureTag;
use serde_json::{json, Value};

const TODAY: &str = "2030-01-10";

/// A declaration over `base` naming the rows URL at `/rows_url` of the done poll.
fn declaration(base: &str, extra: Value) -> ReportDeclaration {
    let mut value = json!({
        "partitions": [{"account": "a1", "grain": "day", "breakdown": "age"}],
        "metrics": ["spend"],
        "entity": "ad_id",
        "date": "date_start",
        "create": {"url": format!("{base}/v1/{{account}}/reports"), "body": {"since": "{start}"}},
        "poll": {"url": format!("{base}/v1/reports/{{id}}"), "interval": "0s"},
        "state": {"pointer": "/status", "done": "done", "failed": "failed"},
        "result": {"pointer": "/rows_url"},
    });
    for (k, v) in extra.as_object().unwrap() {
        value[k] = v.clone();
    }
    ReportDeclaration::parse(&value).unwrap()
}

fn source(declaration: ReportDeclaration, secrets: Vec<(&'static str, &'static str)>) -> ReportSource<DeclaredVendor> {
    let report = declaration.report.clone();
    let vendor = DeclaredVendor::mediated(declaration, resolver(secrets), &Mediation::default()).unwrap();
    ReportSource::new(report, vendor, TODAY).unwrap()
}

/// Every pull of one read, the last answer an error when the read fails.
fn read(source: &mut dyn Source) -> Result<Vec<Value>, contextful_core::run::Failure> {
    let (mut position, mut batches) = (None, Vec::new());
    loop {
        let batch: Value = serde_json::from_slice(&source.pull(&request(position.clone()), &Never)?).unwrap();
        position = batch.get("cursor").cloned();
        let more = batch["more"].as_bool().unwrap_or(false);
        batches.push(batch);
        if !more {
            return Ok(batches);
        }
    }
}

/// Every request passes the mediated client under the create URL's host, its declared credential hydrated onto each;
/// a rows URL the done poll names on another origin refuses as a hop leaving it.
// spec: connector.source.report-origin@ef16b7b6
#[test]
fn a_rows_url_the_vendor_names_off_the_create_origin_refuses() {
    let elsewhere = Server::start(|_| Response::json(200, r#"{"data":[]}"#));
    let rows_url = elsewhere.url("/rows");
    let vendor = Server::start(move |r| match (r.method.as_str(), r.path()) {
        ("POST", "/v1/a1/reports") => Response::json(201, r#"{"id":"r-1"}"#),
        ("GET", "/v1/reports/r-1") => Response::json(200, &json!({"status": "done", "rows_url": rows_url}).to_string()),
        _ => Response::json(404, "{}"),
    });
    let headers = json!({"headers": {"Authorization": "Bearer ${secret://vendor-token}"}});
    let mut s = source(declaration(&vendor.url(""), headers), vec![("vendor-token", "tok-7c1f")]);
    let failure = read(&mut s).unwrap_err();
    assert!(failure.message.contains("SecretRedirectOffOrigin"), "{failure}");
    assert!(elsewhere.requests.lock().unwrap().is_empty(), "no request leaves the create origin");
    let seen = vendor.requests.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(seen.iter().all(|r| r.header("authorization") == Some("Bearer tok-7c1f")), "{seen:?}");
    assert_eq!(serde_json::from_slice::<Value>(&seen[0].body).unwrap(), json!({"since": "2029-12-13"}), "the default 28-day window");
    assert!(contextful_connectors::BUILT_IN.contains(&NAME), "the `report` source is compiled in");
}

/// A request that draws no answer keeps its classified failure: an unreachable vendor is transient, so the
/// partition holds at its committed position and the read ends.
// spec: connector.source.partition-isolation@e11cae37
#[test]
fn an_unreachable_vendor_holds_its_partition_as_a_retryable_failure() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut s = source(declaration(&format!("http://127.0.0.1:{port}"), json!({})), vec![]);
    let batches = read(&mut s).unwrap();
    assert!(!batches.is_empty());
    let last = batches.last().unwrap();
    assert_eq!(last["rows"], json!([]));
    assert!(last["audit"].as_array().is_some_and(|a| a.iter().any(|e| e.as_str().is_some_and(|e| e.contains("held at its committed position")))), "{last}");
    let classified = ReportSource::<DeclaredVendor>::classify(&declaration("http://127.0.0.1:1", json!({})).report, &contextful_connectors::report::VendorFault {
        status: 0,
        body: Value::Null,
        failure: Some(contextful_core::run::Failure::new(FailureTag::Transient, "request failed")),
    });
    assert_eq!(classified.tag, FailureTag::Transient);
}
