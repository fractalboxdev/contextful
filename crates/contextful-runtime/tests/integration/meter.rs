//! `connector.meter`: a metered client reserves one permit per outbound request against
//! the bound limiter, and reports what the vendor said.

use crate::support::{Fixed, Request, Response, Server, SetClock};
use contextful_core::connector::attach::Allowlist;
use contextful_core::connector::meter::{LimiterBinding, LimiterDeclaration};
use contextful_core::connector::resolve::Provider;
use contextful_core::run::FailureTag;
use contextful_runtime::client::{Client, HeaderValue};
use contextful_runtime::{Limiter, Meter, Resolver};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use url::Url;

const TOKEN: &str = "limiter-token-value";
const RUN: &str = "run-7f3a";

fn url(s: &str) -> Url {
    Url::parse(s).unwrap()
}

fn declaration() -> LimiterDeclaration {
    LimiterDeclaration::new("graph-app", "reads", &["X-App-Usage"]).unwrap()
}

/// A vendor answering every path 200 with quota-state headers, redirecting `/start` to `/landed`.
fn vendor() -> Server {
    Server::start(|r| match r.path() {
        "/start" => Response { status: 302, headers: vec![("Location".into(), "/landed".into())], body: Vec::new() },
        _ => Response {
            status: 200,
            headers: vec![("X-App-Usage".into(), "{\"call_count\":41}".into()), ("X-Other".into(), "kept-home".into()), ("Retry-After".into(), "3".into())],
            body: b"{}".to_vec(),
        },
    })
}

/// A limiter granting `permits` under `ttl` to a bearer of [`TOKEN`] and accepting every report.
fn granting(permits: u32, ttl: u64) -> Server {
    Server::start(move |r| {
        if r.header("authorization") != Some(&format!("Bearer {TOKEN}")) {
            return Response::json(401, "{}");
        }
        match r.path() {
            "/acquire" => Response::json(200, &format!("{{\"decision\":\"granted\",\"permits\":{permits},\"ttl_secs\":{ttl}}}")),
            _ => Response::json(204, ""),
        }
    })
}

fn resolver(clock: &SetClock) -> Arc<Resolver> {
    let fixed: Arc<dyn Provider> = Fixed::new("store", &[("limiter-token", TOKEN)]);
    Arc::new(Resolver::new(vec![fixed], false, Arc::new(clock.clone())))
}

fn limiter_at(base: &str, permits: u32, clock: &SetClock) -> Arc<Limiter> {
    let binding = LimiterBinding::parse("graph-app", base, "secret://limiter-token", Some(permits)).unwrap();
    Arc::new(Limiter::new(binding, resolver(clock), RUN, Arc::new(clock.clone())).unwrap())
}

fn metered(vendor: &Server, limiter: &Arc<Limiter>) -> Client {
    Client::new(Allowlist::parse(&["127.0.0.1"]).unwrap(), url(&vendor.url("/"))).metered(Meter::new(declaration(), Some(limiter.clone())))
}

fn get(c: &Client, server: &Server, path: &str) -> Result<contextful_runtime::Response, contextful_core::run::Failure> {
    c.send("GET", &url(&server.url(path)), &[("Accept".to_string(), HeaderValue::Plain("application/json".into()))], None)
}

fn json(r: &Request) -> Value {
    serde_json::from_slice(&r.body).unwrap()
}

fn vendor_hits(server: &Server) -> usize {
    server.requests.lock().unwrap().len()
}

/// The declaration names the quota, the class every request reserves under, and the headers the report forwards;
/// a header it does not name stays home.
#[test]
fn the_declaration_names_the_quota_the_class_and_the_forwarded_headers() {
    let (v, l, clock) = (vendor(), granting(8, 10), SetClock::new());
    let limiter = limiter_at(&l.url(""), 8, &clock);
    get(&metered(&v, &limiter), &v, "/v1").unwrap();
    limiter.finish();
    let acquire = json(&l.received("/acquire")[0]);
    assert_eq!((acquire["quota"].as_str(), acquire["class"].as_str()), (Some("graph-app"), Some("reads")));
    let usage = &json(&l.received("/report")[0])["usage"][0];
    assert_eq!(usage["headers"]["x-app-usage"], "{\"call_count\":41}");
    assert!(usage["headers"].get("x-other").is_none(), "{usage}");
}

/// The engine's shared client reserves ahead of each outbound request, a followed hop included, so one call
/// following one redirect spends two permits.
// spec: connector.meter.reservation-point@d6f1791b
#[test]
fn each_outbound_request_spends_one_permit() {
    let (v, l, clock) = (vendor(), granting(8, 10), SetClock::new());
    let limiter = limiter_at(&l.url(""), 8, &clock);
    let c = metered(&v, &limiter);
    get(&c, &v, "/start").unwrap();
    get(&c, &v, "/v1").unwrap();
    limiter.finish();
    assert_eq!(vendor_hits(&v), 3);
    assert_eq!(l.received("/acquire").len(), 1, "one batch covers three requests");
    let report = json(&l.received("/report")[0]);
    assert_eq!((report["granted"].as_u64(), report["spent"].as_u64()), (Some(8), Some(3)));
}

/// A batch serves one request per permit until it is spent or its TTL lapses; the report ahead of the next acquire
/// states granted against spent, surrendering the unspent.
// spec: connector.meter.permit-batch@6dd4f50c
#[test]
fn a_batch_is_spent_one_permit_per_request_and_surrendered_on_report() {
    let (v, l, clock) = (vendor(), granting(2, 10), SetClock::new());
    let limiter = limiter_at(&l.url(""), 2, &clock);
    let c = metered(&v, &limiter);
    for _ in 0..3 {
        get(&c, &v, "/v1").unwrap();
    }
    assert_eq!(l.received("/acquire").len(), 2, "a spent batch refills");
    let first = json(&l.received("/report")[0]);
    assert_eq!((first["granted"].as_u64(), first["spent"].as_u64()), (Some(2), Some(2)));
    // One permit of the second batch is left; past its TTL it is spent no more.
    clock.advance(11);
    get(&c, &v, "/v1").unwrap();
    assert_eq!(l.received("/acquire").len(), 3, "an expired batch refills");
    let second = json(&l.received("/report")[1]);
    assert_eq!((second["granted"].as_u64(), second["spent"].as_u64()), (Some(2), Some(1)), "the unspent permit is surrendered");
    assert_eq!(json(&l.received("/acquire")[0])["permits"], 2);
}

/// Acquire is an authenticated POST; an explicit denial, a bare `429` with `Retry-After` and a zero-permit grant each
/// fail the request as rate-limited with the limiter's wait, and the vendor sees nothing.
// spec: connector.meter.acquire@8dead24a
#[test]
fn a_denial_a_bare_429_and_a_zero_grant_all_hold_the_request_back() {
    let (v, clock) = (vendor(), SetClock::new());
    let l = granting(8, 10);
    let limiter = limiter_at(&l.url(""), 8, &clock);
    get(&metered(&v, &limiter), &v, "/v1").unwrap();
    let acquire = &l.received("/acquire")[0];
    assert_eq!(acquire.method, "POST");
    assert_eq!(acquire.header("authorization"), Some(format!("Bearer {TOKEN}").as_str()));
    assert_eq!(json(acquire), serde_json::json!({ "quota": "graph-app", "class": "reads", "permits": 8 }));

    let denying = |status: u16, headers: Vec<(String, String)>, body: &'static str| {
        Server::start(move |r| match r.path() {
            "/acquire" => Response { status, headers: headers.clone(), body: body.as_bytes().to_vec() },
            _ => Response::json(204, ""),
        })
    };
    let cases = [
        (denying(200, vec![], "{\"decision\":\"denied\",\"retry_after_secs\":9}"), 9),
        (denying(429, vec![("Retry-After".into(), "7".into())], ""), 7),
        (denying(200, vec![], "{\"decision\":\"granted\",\"permits\":0,\"ttl_secs\":5}"), 5),
    ];
    for (l, wait) in cases {
        let v = vendor();
        let f = get(&metered(&v, &limiter_at(&l.url(""), 8, &clock)), &v, "/v1").unwrap_err();
        assert_eq!(f.tag, FailureTag::RateLimited, "{f}");
        assert_eq!(f.retry_after_secs, Some(wait));
        assert_eq!(vendor_hits(&v), 0, "a held-back request never reaches the vendor");
    }
}

/// An answer the engine cannot read fails the request with `ConnectorLimiterUnreadable` and spends nothing.
// spec: connector.meter.unreadable-answer@398c4b5f
#[test]
fn an_unreadable_answer_fails_the_request_before_the_vendor() {
    let (v, clock) = (vendor(), SetClock::new());
    let l = Server::start(|_| Response::json(200, "<html>limiter</html>"));
    let f = get(&metered(&v, &limiter_at(&l.url(""), 8, &clock)), &v, "/v1").unwrap_err();
    assert!(f.message.starts_with("ConnectorLimiterUnreadable"), "{f}");
    assert_eq!(vendor_hits(&v), 0);
}

/// The report is an authenticated POST of quota, class, granted, spent, one entry per vendor response, the
/// observation instant and the run id.
// spec: connector.meter.report@e4610fca
#[test]
fn the_report_carries_the_accounting_the_responses_and_the_run() {
    let (v, l, clock) = (vendor(), granting(8, 10), SetClock::new());
    let limiter = limiter_at(&l.url(""), 8, &clock);
    let c = metered(&v, &limiter);
    get(&c, &v, "/v1").unwrap();
    get(&c, &v, "/v2").unwrap();
    limiter.finish();
    let sent = &l.received("/report")[0];
    assert_eq!(sent.method, "POST");
    assert_eq!(sent.header("authorization"), Some(format!("Bearer {TOKEN}").as_str()));
    let r = json(sent);
    assert_eq!((r["quota"].as_str(), r["class"].as_str(), r["run_id"].as_str()), (Some("graph-app"), Some("reads"), Some(RUN)));
    assert_eq!((r["granted"].as_u64(), r["spent"].as_u64()), (Some(8), Some(2)));
    assert_eq!(r["observed_at"], "2030-01-01T00:00:00Z");
    let usage = r["usage"].as_array().unwrap();
    assert_eq!(usage.len(), 2, "one entry per vendor response");
    assert_eq!((usage[0]["status"].as_u64(), usage[0]["retry_after_secs"].as_u64()), (Some(200), Some(3)));
}

/// A failing report endpoint is retried under backoff up to three attempts when the run finishes; one that never
/// takes the report is recorded in the audit and fails no request.
#[test]
fn a_report_retries_then_lands_in_the_audit_and_fails_nothing() {
    let clock = SetClock::new();
    // Two failures, then success: delivered on the third attempt.
    let flaky = Arc::new(AtomicUsize::new(0));
    let seen = flaky.clone();
    let l = Server::start(move |r| match r.path() {
        "/acquire" => Response::json(200, "{\"permits\":1,\"ttl_secs\":10}"),
        _ if seen.fetch_add(1, Ordering::SeqCst) < 2 => Response::json(503, "{}"),
        _ => Response::json(204, ""),
    });
    let v = vendor();
    let limiter = limiter_at(&l.url(""), 1, &clock);
    get(&metered(&v, &limiter), &v, "/v1").unwrap();
    limiter.finish();
    assert_eq!(l.received("/report").len(), 3);
    assert!(limiter.audit().is_empty(), "{:?}", limiter.audit());

    // Never accepted: one attempt ahead of the refill, three at finish, one audit entry naming both batches, and
    // every request still succeeds.
    let l = Server::start(|r| match r.path() {
        "/acquire" => Response::json(200, "{\"permits\":1,\"ttl_secs\":10}"),
        _ => Response::json(500, "{}"),
    });
    let limiter = limiter_at(&l.url(""), 1, &clock);
    let c = metered(&v, &limiter);
    get(&c, &v, "/v1").unwrap();
    get(&c, &v, "/v2").unwrap();
    assert_eq!(l.received("/report").len(), 1, "the report ahead of the refill is attempted once");
    limiter.finish();
    assert_eq!(l.received("/report").len(), 4);
    let audit = limiter.audit();
    assert_eq!(audit.len(), 1, "{audit:?}");
    assert!(audit[0].contains("undelivered") && audit[0].contains("graph-app") && audit[0].contains("2 spent of 2 granted"), "{audit:?}");
    assert!(!audit[0].contains(TOKEN));
}

/// A report the limiter does not take ahead of a refill is carried into the next one, so no granted or spent permit
/// goes unreported.
#[test]
fn an_undelivered_report_folds_into_the_next() {
    let clock = SetClock::new();
    let reports = Arc::new(AtomicUsize::new(0));
    let seen = reports.clone();
    let l = Server::start(move |r| match r.path() {
        "/acquire" => Response::json(200, "{\"permits\":1,\"ttl_secs\":10}"),
        _ if seen.fetch_add(1, Ordering::SeqCst) == 0 => Response::json(503, "{}"),
        _ => Response::json(204, ""),
    });
    let v = vendor();
    let limiter = limiter_at(&l.url(""), 1, &clock);
    let c = metered(&v, &limiter);
    for path in ["/v1", "/v2", "/v3"] {
        get(&c, &v, path).unwrap();
    }
    let sent = l.received("/report");
    assert_eq!(sent.len(), 2, "one attempt ahead of each refill");
    let landed = json(&sent[1]);
    assert_eq!((landed["granted"].as_u64(), landed["spent"].as_u64()), (Some(2), Some(2)), "{landed}");
    assert_eq!(landed["usage"].as_array().unwrap().len(), 2);
    limiter.finish();
    let last = json(&l.received("/report")[2]);
    assert_eq!((last["granted"].as_u64(), last["spent"].as_u64()), (Some(1), Some(1)));
    assert!(limiter.audit().is_empty(), "{:?}", limiter.audit());
}

/// A report endpoint that accepts the connection and never answers delays a refill by at most the limiter's call
/// timeout, not by a vendor request's.
#[test]
fn a_hanging_report_endpoint_stalls_no_request() {
    let clock = SetClock::new();
    let l = Server::start(|r| match r.path() {
        "/acquire" => Response::json(200, "{\"permits\":1,\"ttl_secs\":10}"),
        _ => {
            std::thread::sleep(Duration::from_secs(5));
            Response::json(204, "")
        }
    });
    let v = vendor();
    let binding = LimiterBinding::parse("graph-app", &l.url(""), "secret://limiter-token", Some(1)).unwrap();
    let limiter = Arc::new(Limiter::new(binding, resolver(&clock), RUN, Arc::new(clock.clone())).unwrap().with_timeout(Duration::from_millis(200)));
    let c = metered(&v, &limiter);
    get(&c, &v, "/v1").unwrap();
    let started = Instant::now();
    get(&c, &v, "/v2").unwrap();
    assert!(started.elapsed() < Duration::from_secs(2), "the refill took {:?}", started.elapsed());
}

/// A limiter answering `401`, `403` or another failing status fails the request as one transient `ConnectorUnmetered`,
/// never as an expired vendor credential.
#[test]
fn a_failing_limiter_answer_is_never_a_vendor_credential_expiry() {
    let clock = SetClock::new();
    for status in [401u16, 403, 400, 404, 503] {
        let v = vendor();
        let l = Server::start(move |_| Response::json(status, "{}"));
        let f = get(&metered(&v, &limiter_at(&l.url(""), 8, &clock)), &v, "/v1").unwrap_err();
        assert!(f.message.starts_with("ConnectorUnmetered"), "{status}: {f}");
        assert_eq!(f.tag, FailureTag::Transient, "{status}: {f}");
        assert_eq!(vendor_hits(&v), 0);
    }
}

/// The operator's limiter endpoint may resolve to a private address; the vendor-host address check does not refuse
/// it.
// spec: connector.meter.limiter-address@26640b2d
#[test]
fn a_limiter_on_a_private_address_is_not_refused_as_one() {
    let (v, clock) = (vendor(), SetClock::new());
    let binding = LimiterBinding::parse("graph-app", "https://10.255.255.1", "secret://limiter-token", Some(1)).unwrap();
    let limiter = Arc::new(Limiter::new(binding, resolver(&clock), RUN, Arc::new(clock.clone())).unwrap().with_timeout(Duration::from_millis(200)));
    let f = get(&metered(&v, &limiter), &v, "/v1").unwrap_err();
    assert!(f.message.starts_with("ConnectorUnmetered"), "{f}");
    assert!(!f.message.contains("ConnectorPrivateAddress"), "{f}");
    assert!(!f.deterministic, "{f}");
    assert_eq!(vendor_hits(&v), 0);
}

/// With the limiter unreachable, failing, or unbound, a request raises `ConnectorUnmetered` and never reaches the
/// vendor.
// spec: connector.meter.unmetered-request@df535b45
#[test]
fn no_granted_reservation_means_no_request() {
    let clock = SetClock::new();
    let v = vendor();
    // Unreachable: a port nothing listens on.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead = format!("http://127.0.0.1:{}", closed.local_addr().unwrap().port());
    drop(closed);
    let f = get(&metered(&v, &limiter_at(&dead, 8, &clock)), &v, "/v1").unwrap_err();
    assert!(f.message.starts_with("ConnectorUnmetered"), "{f}");
    // Answering a server error.
    let failing = Server::start(|_| Response::json(503, "{}"));
    let f = get(&metered(&v, &limiter_at(&failing.url(""), 8, &clock)), &v, "/v1").unwrap_err();
    assert!(f.message.starts_with("ConnectorUnmetered"), "{f}");
    // Refusing the limiter token: no statement about the vendor credential.
    let refusing = Server::start(|_| Response::json(401, "{}"));
    let f = get(&metered(&v, &limiter_at(&refusing.url(""), 8, &clock)), &v, "/v1").unwrap_err();
    assert!(f.message.starts_with("ConnectorUnmetered"), "{f}");
    assert_ne!(f.tag, FailureTag::AuthExpired, "{f}");
    // Unbound: the declaration carries no limiter.
    let c = Client::new(Allowlist::parse(&["127.0.0.1"]).unwrap(), url(&v.url("/"))).metered(Meter::new(declaration(), None));
    let f = get(&c, &v, "/v1").unwrap_err();
    assert!(f.message.starts_with("ConnectorUnmetered"), "{f}");
    assert!(f.deterministic);
    assert_eq!(vendor_hits(&v), 0);
    // A bound quota loads; an unbound one refuses at load.
    let bindings: BTreeMap<String, LimiterBinding> = BTreeMap::new();
    let err = Limiter::load(&declaration(), &bindings, resolver(&clock), RUN, Arc::new(clock.clone())).unwrap_err();
    assert!(err.message.starts_with("ConnectorQuotaUnbound"), "{err}");
}

/// A limiter token that does not resolve raises `ConnectorUnmetered` naming the token, not an unreachable limiter,
/// and sends no acquire.
#[test]
fn an_unresolved_limiter_token_is_named_as_such() {
    let (v, l, clock) = (vendor(), granting(8, 10), SetClock::new());
    let empty: Arc<dyn Provider> = Fixed::new("store", &[]);
    let resolver = Arc::new(Resolver::new(vec![empty], false, Arc::new(clock.clone())));
    let binding = LimiterBinding::parse("graph-app", &l.url(""), "secret://limiter-token", Some(8)).unwrap();
    let limiter = Arc::new(Limiter::new(binding, resolver, RUN, Arc::new(clock.clone())).unwrap());
    let f = get(&metered(&v, &limiter), &v, "/v1").unwrap_err();
    assert!(f.message.starts_with("ConnectorUnmetered"), "{f}");
    assert!(f.message.contains("does not resolve"), "{f}");
    assert!(!f.message.contains("unreachable"), "{f}");
    assert_eq!(f.message.matches("ConnectorUnmetered").count(), 1, "{f}");
    assert!(l.requests.lock().unwrap().is_empty());
    assert_eq!(vendor_hits(&v), 0);
}

/// A request the allowlist refuses fails with `SecretUnpermittedRequest` before any acquire.
// spec: connector.meter.allowlist-precedence@a04fd503
#[test]
fn a_refused_request_never_reaches_the_limiter() {
    let (v, l, clock) = (vendor(), granting(8, 10), SetClock::new());
    let limiter = limiter_at(&l.url(""), 8, &clock);
    let c = Client::new(Allowlist::parse(&["api.vendor.example"]).unwrap(), url(&v.url("/"))).metered(Meter::new(declaration(), Some(limiter.clone())));
    let f = get(&c, &v, "/v1").unwrap_err();
    assert!(f.message.starts_with("SecretUnpermittedRequest"), "{f}");
    limiter.finish();
    assert!(l.requests.lock().unwrap().is_empty(), "no acquire and no report");
}
