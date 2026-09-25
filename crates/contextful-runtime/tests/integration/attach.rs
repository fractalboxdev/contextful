//! `connector.attach`: the mediated client judges each request, pins every hop to the
//! configured origin and records which headers carried a credential.

use crate::support::{Response, Server};
use contextful_core::connector::attach::Allowlist;
use contextful_core::connector::reference::Hydrated;
use contextful_core::run::FailureTag;
use contextful_runtime::client::{Client, HeaderValue};
use url::Url;

fn client(server: &Server) -> Client {
    Client::new(Allowlist::parse(&["127.0.0.1"]).unwrap(), Url::parse(&server.url("/")).unwrap())
}

fn url(s: &str) -> Url {
    Url::parse(s).unwrap()
}

fn plain() -> Vec<(String, HeaderValue)> {
    vec![("Accept".to_string(), HeaderValue::Plain("application/json".into()))]
}

fn bearer() -> Vec<(String, HeaderValue)> {
    vec![("Authorization".to_string(), HeaderValue::Sensitive(Hydrated::new("Bearer vendor-token-value")))]
}

fn redirecting(to: impl Fn(u16) -> String + Send + Sync + 'static) -> Server {
    Server::start(move |r| match r.path() {
        "/start" => Response { status: 302, headers: vec![("Location".into(), to(0))], body: Vec::new() },
        "/landed" => Response::json(200, "{\"ok\":true}"),
        _ => Response::json(404, "{}"),
    })
}

/// A request whose host the declaration does not cover raises `SecretUnpermittedRequest` and fails the guest call
/// even where guest code discards the error.
// spec: connector.attach.unpermitted-request@06ec9181
#[test]
fn a_request_to_an_uncovered_host_fails_before_any_socket() {
    let server = Server::start(|_| Response::json(200, "{}"));
    // The origin is the server; the declaration covers another host.
    let c = Client::new(Allowlist::parse(&["api.vendor.example"]).unwrap(), Url::parse(&server.url("/")).unwrap());
    let f = c.send("GET", &url(&server.url("/v1")), &plain(), None).unwrap_err();
    assert!(f.message.starts_with("SecretUnpermittedRequest"), "{f}");
    assert!(f.deterministic, "a refusal over static input spends no retry");
    assert!(server.requests.lock().unwrap().is_empty(), "no request left the process");
    // The same client reaches a host the declaration covers.
    let ok = client(&server).send("GET", &url(&server.url("/v1")), &plain(), None).unwrap();
    assert_eq!(ok.status, 200);
}

/// Every source, credential-bearing or not, follows a hop only when it keeps the configured host and port on the
/// same or a stronger transport; cleartext to TLS at that host and port is followed.
// spec: connector.attach.redirect-pinning@b54ab05f
#[test]
fn a_hop_is_followed_only_within_the_configured_origin() {
    // Same host and port: followed, with or without a credential.
    let server = redirecting(|_| "/landed".into());
    for headers in [plain(), bearer()] {
        let resp = client(&server).send("GET", &url(&server.url("/start")), &headers, None).unwrap();
        assert_eq!((resp.status, resp.url.path()), (200, "/landed"));
    }
    // Cleartext to TLS at the same host and port is followed: the client attempts the TLS
    // hop, which this plain listener cannot answer, and reports a transport failure
    // rather than an origin refusal.
    let upgrade = redirecting_to_tls();
    let f = client(&upgrade).send("GET", &url(&upgrade.url("/start")), &plain(), None).unwrap_err();
    assert_eq!(f.tag, FailureTag::Transient, "{f}");
    assert!(!f.message.contains("SecretRedirectOffOrigin"), "{f}");
}

/// A server redirecting `/start` to the TLS scheme at its own host and port.
fn redirecting_to_tls() -> Server {
    let port = std::sync::Arc::new(std::sync::atomic::AtomicU16::new(0));
    let p = port.clone();
    let s = Server::start(move |r| match r.path() {
        "/start" => {
            let port = p.load(std::sync::atomic::Ordering::SeqCst);
            Response { status: 302, headers: vec![("Location".into(), format!("https://127.0.0.1:{port}/landed"))], body: Vec::new() }
        }
        _ => Response::json(200, "{}"),
    });
    port.store(s.port, std::sync::atomic::Ordering::SeqCst);
    s
}

/// A redirect or vendor-supplied next link moving from TLS to cleartext, or to another host or port, raises
/// `SecretRedirectOffOrigin`, and the read fails.
// spec: connector.attach.weakened-hop@5d335517
#[test]
fn a_hop_off_the_origin_or_down_to_cleartext_is_refused() {
    let elsewhere = Server::start(|_| Response::json(200, "{\"leaked\":true}"));
    let other_port = elsewhere.url("/landed");
    let off_port = redirecting(move |_| other_port.clone());
    let f = client(&off_port).send("GET", &url(&off_port.url("/start")), &bearer(), None).unwrap_err();
    assert!(f.message.starts_with("SecretRedirectOffOrigin"), "{f}");
    assert!(elsewhere.requests.lock().unwrap().is_empty(), "the hop was not taken");
    let other_host = format!("http://localhost:{}/landed", off_port.port);
    let off_host = redirecting(move |_| other_host.clone());
    let f = client(&off_host).send("GET", &url(&off_host.url("/start")), &plain(), None).unwrap_err();
    assert!(f.message.starts_with("SecretRedirectOffOrigin"), "{f}");
    // TLS down to cleartext at the same host refuses before any request.
    let secure = Client::new(Allowlist::parse(&["api.vendor.example"]).unwrap(), url("https://api.vendor.example/v1"));
    let f = secure.send("GET", &url("http://api.vendor.example/v1"), &plain(), None).unwrap_err();
    assert!(f.message.starts_with("SecretRedirectOffOrigin"), "{f}");
}

/// The origin answering the request whose body lands equals the configured host and port.
// spec: connector.attach.landed-origin@342100c9
#[test]
fn the_body_that_lands_comes_from_the_configured_origin() {
    let server = redirecting(|_| "/landed".into());
    let configured = url(&server.url("/"));
    let resp = client(&server).send("GET", &url(&server.url("/start")), &plain(), None).unwrap();
    assert_eq!(resp.body, b"{\"ok\":true}");
    assert_eq!((resp.url.host_str(), resp.url.port()), (configured.host_str(), configured.port()));
}

/// A header hydrated from a reference is marked sensitive, and sending it to a cleartext endpoint raises
/// `SecretCleartextEndpoint`, IPv4 and IPv6 loopback exempt.
// spec: connector.attach.cleartext-endpoint@88b2d8b0
#[test]
fn a_credential_never_travels_in_cleartext_outside_loopback() {
    let c = Client::new(Allowlist::parse(&["api.vendor.example"]).unwrap(), url("http://api.vendor.example/v1"));
    let f = c.send("GET", &url("http://api.vendor.example/v1"), &bearer(), None).unwrap_err();
    assert!(f.message.starts_with("SecretCleartextEndpoint") && f.message.contains("Authorization"), "{f}");
    // A plain header to the same endpoint is not held to the rule: the request proceeds to name resolution.
    let f = c.send("GET", &url("http://api.vendor.example/v1"), &plain(), None).unwrap_err();
    assert!(!f.message.contains("SecretCleartextEndpoint"), "{f}");
    // IPv4 loopback is exempt.
    let server = Server::start(|_| Response::json(200, "{}"));
    assert_eq!(client(&server).send("GET", &url(&server.url("/v1")), &bearer(), None).unwrap().status, 200);
    // IPv6 loopback is exempt from the transport rule the client applies.
    assert!(contextful_core::connector::attach::check_transport(&url("http://[::1]:9/v1"), "Authorization").is_ok());
    // One loopback predicate serves every exemption: the name `localhost` and 127.0.0.0/8 too.
    assert!(contextful_core::connector::attach::check_transport(&url("http://localhost:9/v1"), "Authorization").is_ok());
    assert!(contextful_core::connector::attach::check_transport(&url("http://127.0.0.2:9/v1"), "Authorization").is_ok());
    assert!(contextful_core::connector::attach::check_transport(&url("http://localhost.example:9/v1"), "Authorization").is_err());
}

/// A run records which header names carried a credential, by name only.
// spec: connector.attach.sensitive-header-record@65cfc3a7
#[test]
fn the_client_records_credential_header_names_and_no_value() {
    let server = Server::start(|_| Response::json(200, "{}"));
    let c = client(&server);
    let mut headers = bearer();
    headers.extend(plain());
    c.send("GET", &url(&server.url("/v1")), &headers, None).unwrap();
    c.send("GET", &url(&server.url("/v1")), &headers, None).unwrap();
    assert_eq!(c.sensitive_headers(), ["Authorization"]);
    assert!(!format!("{:?}", c.sensitive_headers()).contains("vendor-token-value"));
    assert_eq!(server.requests.lock().unwrap()[0].header("authorization"), Some("Bearer vendor-token-value"));
}

/// `Referer` is off on every outbound client.
// spec: connector.attach.referer-off@42f41c3a
#[test]
fn no_request_carries_a_referer() {
    let server = redirecting(|_| "/landed".into());
    for headers in [plain(), bearer()] {
        client(&server).send("GET", &url(&server.url("/start")), &headers, None).unwrap();
    }
    let seen = server.requests.lock().unwrap();
    assert!(seen.iter().any(|r| r.path() == "/landed"), "the hop was followed");
    assert!(seen.iter().all(|r| r.header("referer").is_none()), "{seen:?}");
}

/// A URL reaching an error message, a log line or a landed column carries scheme, host, port and path; query,
/// fragment and userinfo are dropped.
// spec: connector.attach.url-scrubbing@543ce135
#[test]
fn a_failed_requests_message_carries_no_query_or_fragment() {
    // A port nothing listens on: the request fails at connect.
    let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let origin = url(&format!("http://127.0.0.1:{dead}/v1"));
    let c = Client::new(Allowlist::parse(&["127.0.0.1"]).unwrap(), origin);
    let target = url(&format!("http://127.0.0.1:{dead}/v1/orders?api_key=q-secret-123#frag-secret"));
    let f = c.send("GET", &target, &plain(), None).unwrap_err();
    assert!(f.message.contains(&format!("http://127.0.0.1:{dead}/v1/orders")), "{f}");
    for leaked in ["q-secret-123", "api_key", "frag-secret"] {
        assert!(!f.message.contains(leaked), "{leaked} in {f}");
    }
    // An error naming a redirect target scrubs it as well.
    let server = redirecting(|_| "http://evil.example/steal?token=t-secret#x".into());
    let f = client(&server).send("GET", &url(&server.url("/start")), &plain(), None).unwrap_err();
    assert!(f.message.contains("http://evil.example/steal") && !f.message.contains("t-secret"), "{f}");
}

#[test]
fn a_body_past_the_ceiling_is_permanent() {
    let server = Server::start(|_| Response::json(200, "{\"rows\":[1,2,3,4,5,6,7,8,9]}"));
    let c = client(&server).with_body_limit(8);
    let f = c.send("GET", &url(&server.url("/v1")), &plain(), None).unwrap_err();
    assert_eq!(f.tag, contextful_core::run::FailureTag::Permanent, "{f}");
    assert!(f.deterministic, "retrying reads the same body");
}
