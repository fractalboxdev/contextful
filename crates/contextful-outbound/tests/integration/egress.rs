//! `connector.attach` and `connector.meter` at the transport port: the mediated client
//! reaches the network through a port with a resolve half and a send half, and each hop
//! passes the allowlist, then one pre-send hook carrying its intent, then resolution.

use crate::support::{proxy_env, Fixed, Response, Server, SetClock};
use contextful_core::connector::attach::Allowlist;
use contextful_core::connector::meter::{LimiterBinding, LimiterDeclaration};
use contextful_core::connector::resolve::Provider;
use contextful_core::run::retry::{decide, Decision, Schedule};
use contextful_core::run::FailureTag;
use contextful_outbound::client::{Client, HeaderValue};
use contextful_outbound::egress::{Inbound, Intent, Outbound, Outcome, PreSendHook, Transport, TransportFault};
use contextful_outbound::{Limiter, Meter, Resolver};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use url::Url;

fn url(s: &str) -> Url {
    Url::parse(s).unwrap()
}

fn plain() -> Vec<(String, HeaderValue)> {
    vec![("Accept".to_string(), HeaderValue::Plain("application/json".into()))]
}

/// One scripted answer of the in-memory transport.
#[derive(Clone)]
pub(crate) enum Scripted {
    Answer { status: u16, headers: Vec<(String, String)>, body: Vec<u8> },
    Fail(String),
}

/// What the in-memory transport saw, in order: `resolve <host>:<port>`, `send <method> <url> <addrs>`.
#[derive(Default)]
pub(crate) struct Recording {
    /// The addresses every host resolves to, by host.
    answers: Vec<(String, Vec<SocketAddr>)>,
    /// Answers for successive sends; the last repeats.
    script: Mutex<Vec<Scripted>>,
    events: Arc<Mutex<Vec<String>>>,
    sent_addrs: Mutex<Vec<Vec<SocketAddr>>>,
    /// Each send's proxy choice: `true` when it bypassed the system proxy.
    pub(crate) directs: Mutex<Vec<bool>>,
}

impl Recording {
    pub(crate) fn new(answers: &[(&str, &str)], script: Vec<Scripted>, events: Arc<Mutex<Vec<String>>>) -> Arc<Recording> {
        let answers = answers.iter().map(|(h, a)| (h.to_string(), vec![a.parse().unwrap()])).collect();
        Arc::new(Recording { answers, script: Mutex::new(script), events, sent_addrs: Mutex::default(), directs: Mutex::default() })
    }

    pub(crate) fn lookups(&self) -> usize {
        self.events.lock().unwrap().iter().filter(|e| e.starts_with("resolve")).count()
    }

    pub(crate) fn sends(&self) -> usize {
        self.events.lock().unwrap().iter().filter(|e| e.starts_with("send")).count()
    }
}

impl Transport for Recording {
    fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
        self.events.lock().unwrap().push(format!("resolve {host}:{port}"));
        let found = self.answers.iter().find(|(h, _)| h == host).map(|(_, a)| a.iter().map(|a| SocketAddr::new(a.ip(), port)).collect());
        found.ok_or_else(|| format!("no address for `{host}`"))
    }

    fn send(&self, request: &Outbound<'_>) -> Result<Inbound, TransportFault> {
        self.events.lock().unwrap().push(format!("send {} {}", request.method, request.url));
        self.sent_addrs.lock().unwrap().push(request.addrs.to_vec());
        self.directs.lock().unwrap().push(request.direct);
        let mut script = self.script.lock().unwrap();
        let next = if script.len() > 1 { script.remove(0) } else { script[0].clone() };
        match next {
            Scripted::Answer { status, headers, body } => Ok(Inbound { status, headers, body }),
            Scripted::Fail(why) => Err(TransportFault::Failed(why)),
        }
    }
}

pub(crate) fn ok(body: &str) -> Scripted {
    Scripted::Answer { status: 200, headers: Vec::new(), body: body.as_bytes().to_vec() }
}

/// A hook recording every intent and outcome it sees, refusing hosts listed in `refuse`.
pub(crate) struct Ledger {
    refuse: Vec<String>,
    events: Arc<Mutex<Vec<String>>>,
    pub(crate) intents: Mutex<Vec<Intent>>,
    pub(crate) outcomes: Mutex<Vec<Outcome>>,
}

impl Ledger {
    pub(crate) fn new(refuse: &[&str], events: Arc<Mutex<Vec<String>>>) -> Arc<Ledger> {
        Arc::new(Ledger { refuse: refuse.iter().map(|s| s.to_string()).collect(), events, intents: Mutex::default(), outcomes: Mutex::default() })
    }
}

impl PreSendHook for Ledger {
    fn admit(&self, intent: &Intent) -> Result<(), String> {
        self.events.lock().unwrap().push(format!("admit {}", intent.host));
        self.intents.lock().unwrap().push(intent.clone());
        if self.refuse.contains(&intent.host) {
            return Err("local only".into());
        }
        Ok(())
    }

    fn settle(&self, _intent: &Intent, outcome: &Outcome) {
        self.events.lock().unwrap().push("settle".into());
        self.outcomes.lock().unwrap().push(outcome.clone());
    }
}

fn vendor_client(transport: Arc<Recording>) -> Client {
    Client::new(Allowlist::parse(&["api.vendor.example"]).unwrap(), url("https://api.vendor.example/v1")).with_transport(transport)
}

/// The mediated client reaches the network only through a transport port. Its send half connects to an address the
/// client vetted, follows no redirect, and applies the client's proxy choice, timeout and body ceiling.
// spec: connector.attach.transport-port@1e0d1dbd
#[test]
fn the_send_half_receives_only_vetted_addresses_and_follows_no_redirect() {
    let events = Arc::default();
    let t = Recording::new(
        &[("api.vendor.example", "93.184.216.34:0")],
        vec![Scripted::Answer { status: 302, headers: vec![("Location".into(), "/v1/landed".into())], body: Vec::new() }, ok("{}")],
        events,
    );
    let c = vendor_client(t.clone()).with_body_limit(1024);
    let resp = c.send("GET", &url("https://api.vendor.example/v1/start"), &plain(), None).unwrap();
    assert_eq!((resp.status, resp.url.path()), (200, "/v1/landed"), "the client, not the transport, followed the hop");
    assert_eq!(t.sends(), 2, "each hop is one send");
    for addrs in t.sent_addrs.lock().unwrap().iter() {
        assert_eq!(addrs, &vec!["93.184.216.34:443".parse::<SocketAddr>().unwrap()]);
    }

    // A public host answering a private address never reaches the send half.
    let t = Recording::new(&[("api.vendor.example", "10.0.0.5:0")], vec![ok("{}")], Arc::default());
    let f = vendor_client(t.clone()).send("GET", &url("https://api.vendor.example/v1"), &plain(), None).unwrap_err();
    assert!(f.message.starts_with("ConnectorPrivateAddress"), "{f}");
    assert_eq!(t.sends(), 0);
}

/// A vendor name answering an internal address through the resolve half is refused before the send half.
#[test]
fn a_name_resolving_inward_never_reaches_the_send_half() {
    let inward = ["10.1.2.3:0", "172.16.0.9:0", "192.168.1.1:0", "169.254.169.254:0", "127.0.0.1:0", "[fd00::1]:0", "[fe80::1]:0", "[::1]:0"];
    let mut admitted = 0u64;
    for addr in inward {
        let t = Recording::new(&[("api.vendor.example", addr)], vec![ok("{}")], Arc::default());
        let f = vendor_client(t.clone()).send("GET", &url("https://api.vendor.example/v1"), &plain(), None).unwrap_err();
        assert!(f.message.starts_with("ConnectorPrivateAddress"), "{addr}: {f}");
        assert!(f.deterministic);
        admitted += t.sends() as u64;
    }
    contextful_eval::record::emit("egress-internal-address-resolved", admitted as f64, inward.len() as u64, 0);
    assert_eq!(admitted, 0, "an inward answer reached the send half");
    // A configured loopback name may resolve to loopback.
    let t = Recording::new(&[("localhost", "127.0.0.1:0")], vec![ok("{}")], Arc::default());
    let c = Client::new(Allowlist::parse(&["localhost"]).unwrap(), url("http://localhost:8080/")).with_transport(t.clone());
    assert_eq!(c.send("GET", &url("http://localhost:8080/v1"), &plain(), None).unwrap().status, 200);
}

/// The host resolves a permitted host name once per request and connects to the address it vetted, with no second
/// lookup between check and connect.
// spec: connector.attach.resolve-once@9bf12798
#[test]
fn one_lookup_per_hop_and_the_connect_takes_its_answer() {
    let events: Arc<Mutex<Vec<String>>> = Arc::default();
    let t = Recording::new(&[("api.vendor.example", "93.184.216.34:0")], vec![ok("{}")], events.clone());
    vendor_client(t.clone()).send("GET", &url("https://api.vendor.example/v1"), &plain(), None).unwrap();
    assert_eq!(*events.lock().unwrap(), ["resolve api.vendor.example:443", "send GET https://api.vendor.example/v1"]);
    assert_eq!(t.sent_addrs.lock().unwrap()[0], vec!["93.184.216.34:443".parse::<SocketAddr>().unwrap()]);
}

/// Name resolution runs through the port's resolve half, after the pre-send hook admits the hop; the client vets
/// each address it answers.
// spec: connector.attach.resolve-half@df0b14b9
#[test]
fn resolution_follows_the_hook_and_runs_through_the_port() {
    let events: Arc<Mutex<Vec<String>>> = Arc::default();
    let t = Recording::new(&[("api.vendor.example", "93.184.216.34:0")], vec![ok("{}")], events.clone());
    let hook = Ledger::new(&[], events.clone());
    vendor_client(t).with_hook(hook).send("GET", &url("https://api.vendor.example/v1"), &plain(), None).unwrap();
    let seen: Vec<String> = events.lock().unwrap().iter().map(|e| e.split_whitespace().next().unwrap().to_string()).collect();
    assert_eq!(seen, ["admit", "resolve", "send", "settle"]);
}

/// Each outbound hop passes one pre-send hook after the allowlist and before name resolution, carrying the hop's
/// intent: method, scrubbed URL, host, port, traffic class, run id and request body bytes.
// spec: connector.meter.pre-send-hook@b3f5f3fb
#[test]
fn the_hook_sees_each_hops_intent_after_the_allowlist() {
    let events: Arc<Mutex<Vec<String>>> = Arc::default();
    let t = Recording::new(&[("api.vendor.example", "93.184.216.34:0")], vec![ok("{}")], events.clone());
    let hook = Ledger::new(&[], events.clone());
    let c = vendor_client(t).with_hook(hook.clone()).for_run("run-7f3a");
    c.send("POST", &url("https://api.vendor.example/v1/orders?api_key=q-secret#frag"), &plain(), Some(b"{\"a\":1}")).unwrap();
    let intent = hook.intents.lock().unwrap()[0].clone();
    assert_eq!(intent.method, "POST");
    assert_eq!(intent.url, "https://api.vendor.example/v1/orders", "the intent carries the scrubbed URL");
    assert_eq!((intent.host.as_str(), intent.port), ("api.vendor.example", 443));
    assert_eq!((intent.run_id.as_deref(), intent.class.as_deref()), (Some("run-7f3a"), None));
    assert_eq!(intent.body_bytes, 7);

    // A request the allowlist refuses never reaches the hook.
    let before = hook.intents.lock().unwrap().len();
    let f = c.send("GET", &url("https://elsewhere.example/v1"), &plain(), None).unwrap_err();
    assert!(f.message.starts_with("SecretRedirectOffOrigin") || f.message.starts_with("SecretUnpermittedRequest"), "{f}");
    let off_list = Client::new(Allowlist::parse(&["api.vendor.example"]).unwrap(), url("https://other.example/"))
        .with_transport(Recording::new(&[], vec![ok("{}")], events.clone()))
        .with_hook(hook.clone());
    let f = off_list.send("GET", &url("https://other.example/v1"), &plain(), None).unwrap_err();
    assert!(f.message.starts_with("SecretUnpermittedRequest"), "{f}");
    assert_eq!(hook.intents.lock().unwrap().len(), before, "no intent reached the hook");
}

/// A hook refusing an intent raises `ConnectorEgressRefused` naming the host and the hook's reason. No name is
/// resolved, and the retry schedule never retries it.
// spec: connector.meter.hook-refusal@82c6892f
#[test]
fn a_refused_intent_resolves_no_name_and_is_never_retried() {
    let events: Arc<Mutex<Vec<String>>> = Arc::default();
    let t = Recording::new(&[("api.vendor.example", "93.184.216.34:0")], vec![ok("{}")], events.clone());
    let hook = Ledger::new(&["api.vendor.example"], events.clone());
    let f = vendor_client(t.clone()).with_hook(hook.clone()).send("GET", &url("https://api.vendor.example/v1"), &plain(), None).unwrap_err();
    assert!(f.message.starts_with("ConnectorEgressRefused"), "{f}");
    assert!(f.message.contains("api.vendor.example") && f.message.contains("local only"), "{f}");
    contextful_eval::record::emit("egress-refusal-before-dns", t.lookups() as f64, 1, 0);
    assert_eq!((t.lookups(), t.sends()), (0, 0), "no lookup and no send follow a refusal");
    assert!(hook.outcomes.lock().unwrap().is_empty(), "a refused hop has no outcome to settle");
    let schedule = Schedule::default();
    assert!(matches!(decide(&schedule, "fetch", 1, &f, 7), Decision::Fail { consumed_attempt: false, .. }));
}

/// The hook receives each admitted hop's outcome after the body read or the failure: the status or transport
/// failure, the request body bytes sent, and the response body bytes the host read. A followed redirect is two
/// intents.
// spec: connector.meter.hook-settle@50450905
#[test]
fn each_admitted_hop_settles_its_status_and_byte_counts() {
    let events: Arc<Mutex<Vec<String>>> = Arc::default();
    let t = Recording::new(
        &[("api.vendor.example", "93.184.216.34:0")],
        vec![Scripted::Answer { status: 307, headers: vec![("Location".into(), "/v1/landed".into())], body: b"moved".to_vec() }, ok("{\"rows\":[]}")],
        events.clone(),
    );
    let hook = Ledger::new(&[], events.clone());
    vendor_client(t).with_hook(hook.clone()).send("POST", &url("https://api.vendor.example/v1/start"), &plain(), Some(b"abc")).unwrap();
    let intents = hook.intents.lock().unwrap();
    assert_eq!(intents.len(), 2, "a followed redirect is two intents");
    assert_eq!((intents[0].url.as_str(), intents[1].url.as_str()), ("https://api.vendor.example/v1/start", "https://api.vendor.example/v1/landed"));
    let outcomes = hook.outcomes.lock().unwrap();
    assert_eq!(outcomes.len(), 2);
    assert_eq!((outcomes[0].status, outcomes[0].sent, outcomes[0].received), (Some(307), 3, 5), "the redirect's body counts as read");
    assert_eq!((outcomes[1].status, outcomes[1].sent, outcomes[1].received), (Some(200), 3, 11));
    assert!(outcomes.iter().all(|o| o.failure.is_none()));

    // A transport failure settles as a failure with nothing received.
    let t = Recording::new(&[("api.vendor.example", "93.184.216.34:0")], vec![Scripted::Fail("connection reset".into())], Arc::default());
    let hook = Ledger::new(&[], Arc::default());
    let f = vendor_client(t).with_hook(hook.clone()).send("GET", &url("https://api.vendor.example/v1"), &plain(), None).unwrap_err();
    assert_eq!(f.tag, FailureTag::Transient);
    let outcome = hook.outcomes.lock().unwrap()[0].clone();
    assert_eq!((outcome.status, outcome.received), (None, 0));
    assert!(outcome.failure.as_deref().is_some_and(|w| w.contains("connection reset")), "{outcome:?}");

    // An admitted hop whose name answers an internal address settles the refusal.
    let t = Recording::new(&[("api.vendor.example", "10.0.0.5:0")], vec![ok("{}")], Arc::default());
    let hook = Ledger::new(&[], Arc::default());
    vendor_client(t).with_hook(hook.clone()).send("GET", &url("https://api.vendor.example/v1"), &plain(), None).unwrap_err();
    assert!(hook.outcomes.lock().unwrap()[0].failure.as_deref().is_some_and(|w| w.starts_with("ConnectorPrivateAddress")));
}

/// A limiter granting a batch to any bearer and accepting every report.
fn granting() -> Server {
    Server::start(|r| match r.path() {
        "/acquire" => Response::json(200, "{\"decision\":\"granted\",\"permits\":5,\"ttl_secs\":60}"),
        _ => Response::json(204, ""),
    })
}

fn limiter(server: &Server, clock: &SetClock) -> Arc<Limiter> {
    let fixed: Arc<dyn Provider> = Fixed::new("store", &[("limiter-token", "limiter-token-value")]);
    let resolver = Arc::new(Resolver::new(vec![fixed], false, Arc::new(clock.clone())));
    let binding = LimiterBinding::parse("graph-app", &server.url(""), "secret://limiter-token", Some(5)).unwrap();
    Arc::new(Limiter::new(binding, resolver, "run-7f3a", Arc::new(clock.clone())).unwrap())
}

/// The limiter reservation is the hook's innermost implementation. An operator hook composes in front of it, so a
/// request the operator hook refuses makes no limiter call and spends no permit.
// spec: connector.meter.hook-composition@0465cf7b
#[test]
fn an_operator_refusal_makes_no_limiter_call() {
    let _env = proxy_env();
    let clock = SetClock::new();
    let (vendor, gate) = (Server::start(|_| Response::json(200, "{}")), granting());
    let declaration = LimiterDeclaration::new("graph-app", "reads", &[] as &[&str]).unwrap();
    let metered = |refuse: &[&str]| {
        let hook = Ledger::new(refuse, Arc::default());
        let c = Client::new(Allowlist::parse(&["127.0.0.1"]).unwrap(), url(&vendor.url("/")))
            .metered(Meter::new(declaration.clone(), Some(limiter(&gate, &clock))))
            .with_hook(hook.clone());
        (c, hook)
    };
    let (refusing, _) = metered(&["127.0.0.1"]);
    let f = refusing.send("GET", &url(&vendor.url("/v1")), &plain(), None).unwrap_err();
    assert!(f.message.starts_with("ConnectorEgressRefused"), "{f}");
    assert!(gate.requests.lock().unwrap().is_empty(), "no acquire left for a refused intent");
    assert!(vendor.requests.lock().unwrap().is_empty());

    // An admitting operator hook hands the hop to the reservation, and the intent carries the declared class and
    // the limiter's run id.
    let (admitting, hook) = metered(&[]);
    assert_eq!(admitting.send("GET", &url(&vendor.url("/v1")), &plain(), None).unwrap().status, 200);
    assert_eq!(gate.received("/acquire").len(), 1);
    let intent = hook.intents.lock().unwrap()[0].clone();
    assert_eq!((intent.class.as_deref(), intent.run_id.as_deref()), (Some("reads"), Some("run-7f3a")));
    assert_eq!(vendor.requests.lock().unwrap().len(), 1);
}
