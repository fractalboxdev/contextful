//! `connector.declare-capability`: the scope probe judges the bound credential's grant at
//! session open, ahead of the first read.

use crate::support::{Response, Server};
use contextful_core::connector::attach::Allowlist;
use contextful_core::connector::probe::ScopeProbe;
use contextful_core::connector::reference::Hydrated;
use contextful_core::run::{Failure, FailureTag};
use contextful_runtime::client::{Client, HeaderValue};
use contextful_runtime::probe::{open_session, probe};
use url::Url;

const HEADER: &str = "X-Granted-Scopes";
const TOKEN: &str = "Bearer vendor-token-value";

fn allow() -> Allowlist {
    Allowlist::parse(&["127.0.0.1"]).unwrap()
}

fn declared(server: &Server) -> ScopeProbe {
    ScopeProbe::new(&server.url("/identity"), HEADER, &["reports.read", "accounts.read"]).unwrap()
}

fn token() -> Hydrated {
    Hydrated::new(TOKEN)
}

/// A vendor answering `/identity` with `granted` in the scopes header (none when `None`)
/// and `/read` with one record.
fn vendor(granted: Option<&'static str>) -> Server {
    Server::start(move |r| match r.path() {
        "/identity" => Response { status: 200, headers: granted.map(|g| vec![(HEADER.to_string(), g.to_string())]).unwrap_or_default(), body: b"{}".to_vec() },
        "/read" => Response::json(200, "[{\"id\":1}]"),
        _ => Response::json(404, "{}"),
    })
}

/// The session's first read, sent through the source's client.
fn first_read(server: &Server) -> Result<u16, Failure> {
    let c = Client::new(allow(), Url::parse(&server.url("/")).unwrap());
    let headers = [("Authorization".to_string(), HeaderValue::Sensitive(token()))];
    c.send("GET", &Url::parse(&server.url("/read")).unwrap(), &headers, None).map(|r| r.status)
}

fn open(server: &Server, probe: Option<&ScopeProbe>) -> Result<u16, Failure> {
    open_session(&allow(), probe, ("Authorization", &token()), || first_read(server))
}

fn paths(server: &Server) -> Vec<String> {
    server.requests.lock().unwrap().iter().map(|r| r.path().to_string()).collect()
}

/// A manifest may declare an identity endpoint, the response header carrying granted scopes, and the grant it
/// expects. The host calls it with the bound credential ahead of the first read.
// spec: connector.declare-capability.scope-probe@058eafc8
#[test]
fn the_probe_calls_the_identity_endpoint_with_the_bound_credential_before_the_first_read() {
    let server = vendor(Some("reports.read, accounts.read"));
    assert_eq!(open(&server, Some(&declared(&server))).unwrap(), 200);
    assert_eq!(paths(&server), ["/identity", "/read"], "the probe precedes the first read");
    let identity = &server.received("/identity")[0];
    assert_eq!(identity.header("authorization"), Some(TOKEN), "the probe carries the bound credential");
    // With no probe declared, the session opens straight to its first read.
    let bare = vendor(None);
    assert_eq!(open(&bare, None).unwrap(), 200);
    assert_eq!(paths(&bare), ["/read"]);
}

/// The scope probe's host sits on the allowlist and its scheme is TLS or loopback.
// spec: connector.declare-capability.probe-transport@f4786ed7
#[test]
fn the_probe_reaches_only_an_allowlisted_host_over_tls_or_loopback() {
    let server = vendor(Some("reports.read"));
    // An identity endpoint the allowlist does not cover refuses before any socket.
    let elsewhere = Allowlist::parse(&["api.vendor.example"]).unwrap();
    let f = probe(&elsewhere, &declared(&server), ("Authorization", &token())).unwrap_err();
    assert!(f.message.starts_with("SecretUnpermittedRequest"), "{f}");
    assert!(f.deterministic);
    assert!(paths(&server).is_empty(), "no request left the process");
    // A cleartext identity endpoint off loopback refuses, the credential unsent.
    let cleartext = ScopeProbe::new("http://api.vendor.example/identity", HEADER, &["reports.read"]).unwrap();
    let f = probe(&elsewhere, &cleartext, ("Authorization", &token())).unwrap_err();
    assert!(f.message.starts_with("SecretCleartextEndpoint"), "{f}");
    assert!(!f.message.contains(TOKEN), "{f}");
    // Loopback cleartext on the allowlist is admitted.
    assert_eq!(probe(&allow(), &declared(&server), ("Authorization", &token())).unwrap(), ["reports.read"]);
}

/// A granted scope outside the declared expectation raises `ConnectorScopeExceeded`, and the session does not open.
// spec: connector.declare-capability.scope-exceeded@8ba8ed4a
#[test]
fn a_grant_beyond_the_expectation_refuses_and_the_session_does_not_open() {
    let server = vendor(Some("reports.read,admin.write, some.future.scope"));
    let f = open(&server, Some(&declared(&server))).unwrap_err();
    assert!(f.message.starts_with("ConnectorScopeExceeded"), "{f}");
    assert!(f.message.contains("admin.write, some.future.scope"), "the excess is named, unknown scopes included: {f}");
    assert!(f.deterministic, "an over-scoped credential is over-scoped on every retry");
    assert_eq!(paths(&server), ["/identity"], "no read follows a refused probe");
    // A subset of the expectation, in any order, case or spacing, is within it.
    let within = vendor(Some("  ACCOUNTS.READ  "));
    assert_eq!(open(&within, Some(&declared(&within))).unwrap(), 200);
}

/// A probe response carrying no granted-scopes header raises `ConnectorScopeUnverified`.
// spec: connector.declare-capability.scope-unverified@fd50de06
#[test]
fn an_answer_without_the_scopes_header_refuses() {
    let server = vendor(None);
    let f = open(&server, Some(&declared(&server))).unwrap_err();
    assert!(f.message.starts_with("ConnectorScopeUnverified"), "{f}");
    assert!(f.message.contains(HEADER), "{f}");
    assert_eq!(paths(&server), ["/identity"], "no read follows an unverified grant");
}

#[test]
fn a_rejected_credential_at_the_probe_classifies_as_expired_auth() {
    let server = Server::start(|_| Response::json(401, "{}"));
    let f = open(&server, Some(&declared(&server))).unwrap_err();
    assert_eq!(f.tag, FailureTag::AuthExpired, "{f}");
    assert_eq!(paths(&server), ["/identity"]);
}
