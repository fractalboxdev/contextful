//! `connector.attach` at the guest's mediation point.

use crate::support::{loopback, open_with, text, Response, Server};
use contextful_core::connector::attach::Allowlist;
use contextful_core::connector::reference::{Hydrated, SecretName, Template};
use contextful_core::connector::resolve::{Answer, Provider};
use contextful_core::ports::Clock;
use contextful_core::run::{Failure, FailureTag};
use contextful_core::time::Instant;
use contextful_wasm::{Grant, Hydrate, Limits};
use contextful_outbound::client::HeaderValue;
use contextful_outbound::Resolver;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

struct Bearer;

impl Hydrate for Bearer {
    fn hydrate(&self) -> Result<HeaderValue, Failure> {
        Ok(HeaderValue::Sensitive(Hydrated::new("Bearer vendor-token-value").into()))
    }
}

fn bearer() -> Vec<(String, Arc<dyn Hydrate>)> {
    vec![("Authorization".into(), Arc::new(Bearer))]
}

/// No host import hands credential bytes to guest code; a guest names a request and receives a response.
// spec: connector.attach.no-material-to-a-guest@643f937c
// spec: connector.attach.mediation-covers-every-egress@b0a89377
#[test]
fn the_host_attaches_the_credential_and_the_guest_sees_only_the_response() {
    let server = Server::start(|r| Response::text(200, if r.header("authorization").is_some() { "attached" } else { "bare" }));
    let grant = Grant { hydrate: bearer(), ..loopback() };
    let mut s = open_with(grant, &Limits::default(), None).unwrap();
    s.open(&format!("fetch {}", server.url("/v1")), None).unwrap();
    let seen = text(&s.next().unwrap().unwrap());
    assert_eq!(seen, "200 attached");
    assert!(!seen.contains("vendor-token-value"));
    let got = server.received();
    assert_eq!(got[0].header("authorization"), Some("Bearer vendor-token-value"), "the vendor received the host's header");
    assert!(format!("{:?}", s.traffic()).find("vendor-token-value").is_none(), "the record names no value");
}

/// At session open, a credential beside a wildcard or a second host raises `SecretWildcardHost`. The guest host has
/// no validation step, so `connector.attach.bound-host` stays unpinned here.
#[test]
fn a_credential_beside_a_wildcard_or_second_host_refuses_the_session() {
    for hosts in [&["*.vendor.example"][..], &["api.vendor.example", "127.0.0.1"][..]] {
        let grant = Grant { allow: Allowlist::parse(hosts).unwrap(), hydrate: bearer(), ..loopback() };
        let f = open_with(grant, &Limits::default(), None).err().expect("refused");
        assert!(f.message.starts_with("SecretWildcardHost"), "{hosts:?}: {f}");
        let grant = Grant { allow: Allowlist::parse(hosts).unwrap(), hydrate: minted(), ..loopback() };
        let f = open_with(grant, &Limits::default(), None).err().expect("refused");
        assert!(f.message.starts_with("SecretWildcardHost"), "a per-request header binds a credential too: {hosts:?}: {f}");
    }
    let unbound = Grant { allow: Allowlist::parse(&["*.vendor.example"]).unwrap(), attach: Vec::new(), hydrate: Vec::new(), gate: None, hook: None, class: None, run_id: None, transport: None };
    assert!(open_with(unbound, &Limits::default(), None).is_ok(), "a wildcard binds no credential");
}

/// A lease provider stand-in: each hydration mints the next token, or fails as the mint endpoint does.
#[derive(Default)]
struct Mint {
    asked: AtomicUsize,
    down: bool,
    last: Mutex<Option<Weak<Hydrated>>>,
}

impl Hydrate for Mint {
    fn hydrate(&self) -> Result<HeaderValue, Failure> {
        let n = self.asked.fetch_add(1, Ordering::SeqCst) + 1;
        if self.down {
            return Err(Failure::new(FailureTag::Transient, "the mint endpoint is unreachable"));
        }
        let value = Arc::new(Hydrated::new(format!("Bearer lease-{n}")));
        *self.last.lock().unwrap() = Some(Arc::downgrade(&value));
        Ok(HeaderValue::Sensitive(value))
    }
}

fn minted() -> Vec<(String, Arc<dyn Hydrate>)> {
    vec![("Authorization".into(), Arc::new(Mint::default()) as Arc<dyn Hydrate>)]
}

/// Material enters the process per request, while the request is built: a session holds no hydrated value between
/// requests, so each request carries what the provider answers at that moment.
// spec: connector.attach.per-request-hydration@93cdb614
// spec: connector.resolve.hydration-is-just-in-time@d63a022b
#[test]
fn a_hydrated_header_renders_afresh_for_each_request() {
    let server = Server::start(|_| Response::text(200, "ok"));
    let mint = Arc::new(Mint::default());
    let grant = Grant { hydrate: vec![("Authorization".into(), mint.clone() as Arc<dyn Hydrate>)], ..loopback() };
    let mut s = open_with(grant, &Limits::default(), None).unwrap();
    assert_eq!(mint.asked.load(Ordering::SeqCst), 0, "opening a session hydrates nothing");
    for _ in 0..3 {
        s.open(&format!("fetch {}", server.url("/v1")), None).unwrap();
        assert_eq!(text(&s.next().unwrap().unwrap()), "200 ok");
        assert!(mint.last.lock().unwrap().as_ref().unwrap().upgrade().is_none(), "the mediator retained a hydrated request header");
    }
    let got = server.received();
    assert_eq!(got[0].header("authorization"), Some("Bearer lease-1"));
    assert_eq!(got[1].header("authorization"), Some("Bearer lease-2"));
    assert_eq!(got[2].header("authorization"), Some("Bearer lease-3"));
    assert_eq!(mint.asked.load(Ordering::SeqCst), 3);
    assert!(format!("{:?}", s.traffic()).find("lease-").is_none(), "the record names no value");
}

#[derive(Default)]
struct Rotating {
    version: AtomicUsize,
    calls: AtomicUsize,
}

impl Provider for Rotating {
    fn name(&self) -> &str { "rotating" }

    fn answer(&self, name: &SecretName) -> Result<Option<Answer>, Failure> {
        if name.as_str() != "vendor-token" { return Ok(None); }
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Some(Answer { value: Hydrated::new(format!("token-{}", self.version.load(Ordering::SeqCst))), expires_at: None, version: None }))
    }
}

#[derive(Default)]
struct MovingClock(AtomicI64);

impl Clock for MovingClock {
    fn now(&self) -> Instant { Instant::from_unix_secs(self.0.load(Ordering::SeqCst)).unwrap() }
}

struct Rendered {
    resolver: Resolver,
    template: Template,
}

impl Hydrate for Rendered {
    fn hydrate(&self) -> Result<HeaderValue, Failure> {
        self.resolver.render(&self.template).map(|value| HeaderValue::Sensitive(Arc::new(value)))
    }
}

#[test]
fn a_rotated_secret_reaches_the_next_request_after_cache_expiry() {
    let server = Server::start(|_| Response::text(200, "ok"));
    let provider = Arc::new(Rotating::default());
    let clock = Arc::new(MovingClock::default());
    let resolver = Resolver::new(vec![provider.clone()], false, clock.clone());
    let rendered = Arc::new(Rendered { resolver, template: Template::parse("Bearer ${secret://vendor-token}").unwrap() });
    let grant = Grant { hydrate: vec![("Authorization".into(), rendered)], ..loopback() };
    let mut session = open_with(grant, &Limits::default(), None).unwrap();
    for version in [0, 0, 1] {
        provider.version.store(version, Ordering::SeqCst);
        if version == 1 { clock.0.store(300, Ordering::SeqCst); }
        session.open(&format!("fetch {}", server.url("/rotation")), None).unwrap();
        assert_eq!(text(&session.next().unwrap().unwrap()), "200 ok");
    }
    let got = server.received();
    assert_eq!(got.iter().map(|request| request.header("authorization")).collect::<Vec<_>>(), vec![Some("Bearer token-0"), Some("Bearer token-0"), Some("Bearer token-1")]);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
}

/// A hydration failure sends the vendor nothing and fails the call with the provider's failure, even when the guest
/// swallows the refused request.
// spec: connector.lease.vendor-requests-on-failure@639c5f2f
#[test]
fn a_failed_hydration_sends_nothing_and_fails_the_call() {
    let server = Server::start(|_| Response::text(200, "ok"));
    let mint = Arc::new(Mint { down: true, ..Mint::default() });
    let grant = Grant { hydrate: vec![("Authorization".into(), mint.clone() as Arc<dyn Hydrate>)], ..loopback() };
    let mut s = open_with(grant, &Limits::default(), None).unwrap();
    s.open(&format!("swallow {}", server.url("/v1")), None).unwrap();
    let f = s.next().unwrap_err();
    assert_eq!(f.tag, FailureTag::Transient, "{f}");
    assert!(f.message.contains("mint endpoint is unreachable"), "{f}");
    assert_eq!(mint.asked.load(Ordering::SeqCst), 1);
    assert!(server.received().is_empty(), "the vendor received a request without its credential");
}

/// An OAuth access token stand-in: `rejected` replaces the token a `401` named, once per token,
/// or fails as a refused write-back does.
#[derive(Default)]
struct Refreshing {
    generation: AtomicUsize,
    rejections: AtomicUsize,
    refuse: bool,
}

impl Hydrate for Refreshing {
    fn hydrate(&self) -> Result<HeaderValue, Failure> {
        Ok(HeaderValue::Sensitive(Arc::new(Hydrated::new(format!("Bearer token-{}", self.generation.load(Ordering::SeqCst) + 1)))))
    }

    fn rejected(&self, sent: &HeaderValue) -> Result<bool, Failure> {
        self.rejections.fetch_add(1, Ordering::SeqCst);
        if self.refuse {
            return Err(Failure::deterministic(FailureTag::Permanent, "SecretRotationWriteDenied: the store refused the put"));
        }
        if sent.text() == format!("Bearer token-{}", self.generation.load(Ordering::SeqCst) + 1) {
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
        Ok(true)
    }
}

/// A vendor `401` to a request carrying a refreshable header refreshes it once and resends the
/// request once; a second `401` returns to the guest, and a header with no refresh resends nothing.
// spec: connector.rotate.rejected-once@161fc964
#[test]
fn a_rejected_credential_refreshes_once_and_the_request_goes_out_once_more() {
    let server = Server::start(|r| match r.header("authorization") {
        Some("Bearer token-2") => Response::text(200, "ok"),
        _ => Response::text(401, "expired"),
    });
    let refreshing = Arc::new(Refreshing::default());
    let grant = Grant { hydrate: vec![("Authorization".into(), refreshing.clone() as Arc<dyn Hydrate>)], ..loopback() };
    let mut s = open_with(grant, &Limits::default(), None).unwrap();
    s.open(&format!("fetch {}", server.url("/v1")), None).unwrap();
    assert_eq!(text(&s.next().unwrap().unwrap()), "200 ok");
    let got = server.received();
    assert_eq!(got.iter().map(|r| r.header("authorization")).collect::<Vec<_>>(), [Some("Bearer token-1"), Some("Bearer token-2")]);
    assert!(format!("{:?}", s.traffic()).find("token-").is_none(), "the record names no value");

    let refused = Server::start(|_| Response::text(401, "expired"));
    let always = Arc::new(Refreshing::default());
    let grant = Grant { hydrate: vec![("Authorization".into(), always.clone() as Arc<dyn Hydrate>)], ..loopback() };
    let mut s = open_with(grant, &Limits::default(), None).unwrap();
    s.open(&format!("fetch {}", refused.url("/v1")), None).unwrap();
    assert_eq!(text(&s.next().unwrap().unwrap()), "401 expired");
    assert_eq!((refused.received().len(), always.rejections.load(Ordering::SeqCst)), (2, 1));

    let plain = Server::start(|_| Response::text(401, "expired"));
    let grant = Grant { hydrate: bearer(), ..loopback() };
    let mut s = open_with(grant, &Limits::default(), None).unwrap();
    s.open(&format!("fetch {}", plain.url("/v1")), None).unwrap();
    assert_eq!(text(&s.next().unwrap().unwrap()), "401 expired");
    assert_eq!(plain.received().len(), 1);
}

/// A refresh failing after a `401` fails the call with its failure and resends nothing, even
/// when the guest swallows the request.
#[test]
fn a_failed_refresh_fails_the_call_and_resends_nothing() {
    let server = Server::start(|_| Response::text(401, "expired"));
    let refreshing = Arc::new(Refreshing { refuse: true, ..Refreshing::default() });
    let grant = Grant { hydrate: vec![("Authorization".into(), refreshing as Arc<dyn Hydrate>)], ..loopback() };
    let mut s = open_with(grant, &Limits::default(), None).unwrap();
    s.open(&format!("swallow {}", server.url("/v1")), None).unwrap();
    let f = s.next().unwrap_err();
    assert!(f.message.starts_with("SecretRotationWriteDenied"), "{f}");
    assert_eq!(server.received().len(), 1);
}
