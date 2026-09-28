//! `connector.lease`: the lease provider at the head of the chain, its mint wire and its
//! bootstrap credential.

use crate::support::{configure_proxy, proxy_env, name, Fixed, Response, Server, SetClock};
use contextful_core::connector::lease::Scopes;
use contextful_core::connector::resolve::Provider;
use contextful_core::run::FailureTag;
use contextful_outbound::secrets::{assemble, LeaseProvider};
use contextful_outbound::Resolver;
use std::collections::BTreeMap;
use std::sync::Arc;

const MINT: &str = "mint-bootstrap-value";

fn env(server: &Server, scopes: &str) -> BTreeMap<String, String> {
    [
        ("CONTEXTFUL_SECRETS_BACKEND", "lease,env"),
        ("CONTEXTFUL_LEASE_SCOPES", scopes),
        ("CONTEXTFUL_LEASE_PROVIDER_URL", server.url("").as_str()),
        ("LEASE_MINT", MINT),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// A provider answering every mint with `status` and `body`.
fn provider(status: u16, body: &'static str) -> Server {
    Server::start(move |_| Response::json(status, body))
}

fn leased(expires_in: u64) -> Server {
    Server::start(move |r| {
        if r.header("authorization") == Some(&format!("Bearer {MINT}")) {
            Response::json(200, &format!("{{\"value\":\"leased-value\",\"expires_in\":{expires_in}}}"))
        } else {
            Response::json(401, "{}")
        }
    })
}

fn resolver(server: &Server, scopes: &str, clock: &SetClock) -> Resolver {
    assemble(&env(server, scopes), Arc::new(clock.clone())).unwrap()
}

fn mints(server: &Server) -> usize {
    server.received("/leases").len()
}

/// For a declared name the lease provider answers and no adapter behind it is reached; a shadowing adapter raises
/// under {{connector.resolve.shadowed-name}}.
// spec: connector.lease.head-of-chain@dccd7e48
#[test]
fn the_lease_provider_answers_a_declared_name_ahead_of_every_adapter() {
    let _env = proxy_env();
    let server = leased(600);
    let clock = SetClock::new();
    let r = resolver(&server, "vendor-token", &clock);
    assert_eq!(r.hydrate(&name("vendor-token")).unwrap().reveal(), "leased-value");
    assert_eq!(r.attribution().get("vendor-token").map(String::as_str), Some("lease"));
    // An adapter behind the provider holding the same name refuses as shadowing.
    let behind: Vec<Arc<dyn Provider>> = vec![Fixed::new("env", &[("lease-mint", MINT)]), Fixed::new("manager", &[("vendor-token", "stored")])];
    let lease = LeaseProvider::new(&server.url(""), Scopes::parse("vendor-token").unwrap(), name("lease-mint"), behind.clone(), Arc::new(clock.clone())).unwrap();
    let mut chain: Vec<Arc<dyn Provider>> = vec![Arc::new(lease)];
    chain.extend(behind);
    let shadowed = Resolver::new(chain, false, Arc::new(clock));
    let f = shadowed.hydrate(&name("vendor-token")).unwrap_err();
    assert!(f.message.starts_with("SecretNameShadowed") && f.message.contains("lease") && f.message.contains("manager"), "{f}");
}

/// An undeclared name sends the lease provider no request and leaves precedence among the other adapters
/// untouched.
// spec: connector.lease.undeclared-name-defers@b2abe5be
#[test]
fn an_undeclared_name_passes_the_provider_by() {
    let _env = proxy_env();
    let server = leased(600);
    let clock = SetClock::new();
    let behind: Vec<Arc<dyn Provider>> = vec![Fixed::new("env", &[("lease-mint", MINT)]), Fixed::new("manager", &[("other-token", "from-manager")]), Fixed::new("keychain", &[])];
    let lease = LeaseProvider::new(&server.url(""), Scopes::parse("vendor-token").unwrap(), name("lease-mint"), behind.clone(), Arc::new(clock.clone())).unwrap();
    let mut chain: Vec<Arc<dyn Provider>> = vec![Arc::new(lease)];
    chain.extend(behind);
    let r = Resolver::new(chain, false, Arc::new(clock));
    assert_eq!(r.hydrate(&name("other-token")).unwrap().reveal(), "from-manager");
    assert_eq!(r.attribution().get("other-token").map(String::as_str), Some("manager"));
    assert_eq!(mints(&server), 0);
}

/// Selecting the lease backend with no scope declared raises `SecretLeaseScopesEmpty` at startup.
// spec: connector.lease.empty-scope-set@4ed767e3
#[test]
fn the_lease_backend_with_no_scope_refuses_at_assembly() {
    let _env = proxy_env();
    let server = leased(600);
    for scopes in ["", " , "] {
        let err = assemble(&env(&server, scopes), Arc::new(SetClock::new())).err().unwrap();
        assert!(err.message.starts_with("SecretLeaseScopesEmpty"), "{err}");
    }
    assert_eq!(mints(&server), 0);
}

/// A lease is material plus one expiry, held as a pair in memory for the run, evicted at expiry, and written to
/// no file, journal entry or audit record.
// spec: connector.lease.lease@cde859ef
#[test]
fn a_lease_is_evicted_at_its_expiry_and_reminted() {
    let _env = proxy_env();
    let dir = tempfile::tempdir().unwrap();
    let before: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    let server = leased(60);
    let clock = SetClock::new();
    let r = resolver(&server, "vendor-token", &clock);
    r.hydrate(&name("vendor-token")).unwrap();
    clock.advance(50);
    r.hydrate(&name("vendor-token")).unwrap();
    assert_eq!(mints(&server), 1, "held for its window");
    clock.advance(10);
    r.hydrate(&name("vendor-token")).unwrap();
    assert_eq!(mints(&server), 2, "evicted at expiry");
    // The resolver holds its leases in memory: nothing lands on disk.
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), before.len());
}

/// A leased name's cache entry is bounded by the tighter of {{connector.resolve.cache-ttl}} and the lease's own
/// expiry.
// spec: connector.lease.cache-window@695b6b2e
#[test]
fn a_leased_entry_retires_by_the_tighter_bound() {
    let _env = proxy_env();
    for (expires_in, held, reminted) in [(60, 50, 60), (3_600, 260, 300)] {
        let server = leased(expires_in);
        let clock = SetClock::new();
        let r = resolver(&server, "vendor-token", &clock);
        r.hydrate(&name("vendor-token")).unwrap();
        clock.advance(held);
        r.hydrate(&name("vendor-token")).unwrap();
        assert_eq!(mints(&server), 1, "lease {expires_in} s at +{held} s");
        clock.advance(reminted - held);
        r.hydrate(&name("vendor-token")).unwrap();
        assert_eq!(mints(&server), 2, "lease {expires_in} s at +{reminted} s");
    }
}

/// The provider answers `POST <provider-url>/leases` carrying a bearer mint credential and body `{ "scope":
/// "<lease scope>" }`.
// spec: connector.lease.endpoint@421208bc
#[test]
fn a_mint_is_a_bearer_post_naming_the_scope() {
    let _env = proxy_env();
    let server = leased(600);
    let r = resolver(&server, "vendor-token=orders.read", &SetClock::new());
    r.hydrate(&name("vendor-token")).unwrap();
    let req = &server.received("/leases")[0];
    assert_eq!(req.method, "POST");
    assert_eq!(req.header("authorization"), Some(format!("Bearer {MINT}").as_str()));
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&req.body).unwrap(), serde_json::json!({ "scope": "orders.read" }));
}

/// A `404` raises `SecretLeaseScopeUnknown` as a configuration fault; a declared name never falls through to a
/// stored credential.
// spec: connector.lease.unknown-scope@ac4f5fbf
#[test]
fn an_unknown_scope_is_a_configuration_fault_with_no_fallthrough() {
    let _env = proxy_env();
    let server = provider(404, "{}");
    let mut vars = env(&server, "vendor-token");
    vars.insert("CONTEXTFUL_SECRETS_ALLOW_ENV_TEMPLATES".into(), "1".into());
    vars.insert("VENDOR_TOKEN".into(), "stored-value".into());
    let r = assemble(&vars, Arc::new(SetClock::new())).unwrap();
    let f = r.hydrate(&name("vendor-token")).unwrap_err();
    assert!(f.message.starts_with("SecretLeaseScopeUnknown"), "{f}");
    assert_eq!(f.tag, FailureTag::Config);
}

/// A `401` or `403` raises `SecretMintRejected`, classed `auth_expired`.
// spec: connector.lease.mint-rejected@df32bcc5
#[test]
fn a_rejected_mint_credential_is_auth_expired() {
    let _env = proxy_env();
    for status in [401, 403] {
        let server = provider(status, "{}");
        let f = resolver(&server, "vendor-token", &SetClock::new()).hydrate(&name("vendor-token")).unwrap_err();
        assert!(f.message.starts_with("SecretMintRejected"), "{f}");
        assert_eq!(f.tag, FailureTag::AuthExpired);
    }
}

/// A `429` passes its `Retry-After` to the run's retry, classed `rate_limited`.
// spec: connector.lease.rate-limited@360b9787
#[test]
fn a_throttled_mint_carries_its_retry_after() {
    let _env = proxy_env();
    let server = Server::start(|_| Response { status: 429, headers: vec![("Retry-After".into(), "7".into())], body: Vec::new() });
    let f = resolver(&server, "vendor-token", &SetClock::new()).hydrate(&name("vendor-token")).unwrap_err();
    assert_eq!((f.tag, f.retry_after_secs), (FailureTag::RateLimited, Some(7)));
}

/// Any other `4xx` raises `SecretLeaseRequestRejected` as a permanent configuration fault.
// spec: connector.lease.request-rejected@eb775f32
#[test]
fn another_client_error_is_a_permanent_configuration_fault() {
    let _env = proxy_env();
    for status in [400, 409, 422] {
        let server = provider(status, "{}");
        let f = resolver(&server, "vendor-token", &SetClock::new()).hydrate(&name("vendor-token")).unwrap_err();
        assert!(f.message.starts_with("SecretLeaseRequestRejected"), "{f}");
        assert_eq!(f.tag, FailureTag::Config);
        assert!(f.deterministic, "no retry spends an attempt on it");
    }
}

/// A `5xx` and a transport error are transient. The mint client retries nothing itself; the run's retry schedule
/// is the one retry layer.
// spec: connector.lease.transient-class@4cf439db
#[test]
fn server_and_transport_failures_are_transient_and_tried_once() {
    let _env = proxy_env();
    let server = provider(503, "{}");
    let f = resolver(&server, "vendor-token", &SetClock::new()).hydrate(&name("vendor-token")).unwrap_err();
    assert_eq!(f.tag, FailureTag::Transient);
    assert_eq!(mints(&server), 1, "the mint client made one attempt");
    let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut vars = env(&server, "vendor-token");
    vars.insert("CONTEXTFUL_LEASE_PROVIDER_URL".into(), format!("http://127.0.0.1:{dead}"));
    let f = assemble(&vars, Arc::new(SetClock::new())).unwrap().hydrate(&name("vendor-token")).unwrap_err();
    assert_eq!(f.tag, FailureTag::Transient, "{f}");
}

/// A resolver issues at most 1 calls to the mint endpoint per declared name per lease window.
// spec: connector.lease.calls-per-resolver@510ace2f
#[test]
fn one_mint_per_name_per_window() {
    let _env = proxy_env();
    assert_eq!(contextful_core::connector::lease::MINT_CALLS_PER_WINDOW, 1);
    let server = leased(600);
    let clock = SetClock::new();
    let r = Arc::new(resolver(&server, "vendor-token,other-token", &clock));
    std::thread::scope(|s| {
        for _ in 0..6 {
            let r = r.clone();
            s.spawn(move || {
                for n in ["vendor-token", "other-token"] {
                    r.hydrate(&name(n)).unwrap();
                }
            });
        }
    });
    clock.advance(200);
    r.hydrate(&name("vendor-token")).unwrap();
    assert_eq!(mints(&server), 2, "one per declared name");
}

/// The lease provider holds the chain as assembled before it joined, so the mint reference hydrates only through
/// adapters behind it.
// spec: connector.lease.bootstrap-non-recursion@072ce297
#[test]
fn the_mint_credential_comes_from_the_adapters_behind_the_provider() {
    let _env = proxy_env();
    let server = leased(600);
    let clock = SetClock::new();
    // The environment answers no template, yet it holds the mint credential for the provider in front of it.
    let r = resolver(&server, "vendor-token", &clock);
    assert!(r.hydrate(&name("lease-mint")).unwrap_err().message.starts_with("SecretUnresolvedReference"));
    assert_eq!(r.hydrate(&name("vendor-token")).unwrap().reveal(), "leased-value");
    assert_eq!(server.received("/leases")[0].header("authorization"), Some(format!("Bearer {MINT}").as_str()));
}

/// A bootstrap name no adapter behind the lease provider answers raises `SecretBootstrapUnresolved`, naming the
/// reference.
// spec: connector.lease.bootstrap-unserved@928fba5b
#[test]
fn an_unanswered_bootstrap_refuses_before_any_mint() {
    let _env = proxy_env();
    let server = leased(600);
    let mut vars = env(&server, "vendor-token");
    vars.remove("LEASE_MINT");
    let f = assemble(&vars, Arc::new(SetClock::new())).unwrap().hydrate(&name("vendor-token")).unwrap_err();
    assert!(f.message.starts_with("SecretBootstrapUnresolved") && f.message.contains("secret://lease-mint"), "{f}");
    assert_eq!(mints(&server), 0);
}

/// Listing the bootstrap name among the leased scopes raises `SecretBootstrapLeased` at startup.
// spec: connector.lease.bootstrap-declared-leased@dd32c146
#[test]
fn the_bootstrap_name_cannot_be_leased() {
    let _env = proxy_env();
    let server = leased(600);
    let err = assemble(&env(&server, "vendor-token,lease-mint"), Arc::new(SetClock::new())).err().unwrap();
    assert!(err.message.starts_with("SecretBootstrapLeased"), "{err}");
}

/// The mint client reaches its endpoint over TLS or loopback and ignores system proxy configuration.
// spec: connector.lease.mint-transport@1cf66095
#[test]
fn the_mint_endpoint_is_tls_or_loopback_and_bypasses_the_proxy() {
    let server = leased(600);
    let mut vars = env(&server, "vendor-token");
    vars.insert("CONTEXTFUL_LEASE_PROVIDER_URL".into(), "http://leases.internal.example".into());
    let err = assemble(&vars, Arc::new(SetClock::new())).err().unwrap();
    assert!(err.message.starts_with("SecretCleartextEndpoint"), "{err}");
    vars.insert("CONTEXTFUL_LEASE_PROVIDER_URL".into(), "https://leases.internal.example".into());
    assert!(assemble(&vars, Arc::new(SetClock::new())).is_ok());
    // A proxy configured for the process is not used: the mint reaches the provider directly.
    let proxy = Server::start(|_| Response::json(502, "{}"));
    let hydrated = {
        let _proxied = configure_proxy(&proxy.url(""));
        resolver(&server, "vendor-token", &SetClock::new()).hydrate(&name("vendor-token"))
    };
    assert_eq!(hydrated.unwrap().reveal(), "leased-value");
    assert!(proxy.requests.lock().unwrap().is_empty());
}

/// A `3xx` from the mint endpoint raises `SecretLeaseRedirect`; the mint credential is not replayed at the
/// target.
// spec: connector.lease.redirect@f0c889b1
#[test]
fn a_redirected_mint_refuses_and_the_target_sees_nothing() {
    let _env = proxy_env();
    let server = Server::start(|r| match r.path() {
        "/leases" => Response { status: 307, headers: vec![("Location".into(), "/elsewhere".into())], body: Vec::new() },
        _ => Response::json(200, "{\"value\":\"x\",\"expires_in\":60}"),
    });
    let f = resolver(&server, "vendor-token", &SetClock::new()).hydrate(&name("vendor-token")).unwrap_err();
    assert!(f.message.starts_with("SecretLeaseRedirect"), "{f}");
    assert!(server.received("/elsewhere").is_empty());
}

#[test]
fn a_provider_named_localhost_mints_over_loopback() {
    let _env = proxy_env();
    let server = leased(600);
    let clock = SetClock::new();
    let mut vars = env(&server, "vendor-token");
    vars.insert("CONTEXTFUL_LEASE_PROVIDER_URL".into(), server.url("").replace("127.0.0.1", "localhost"));
    let r = assemble(&vars, Arc::new(clock)).unwrap();
    assert_eq!(r.hydrate(&name("vendor-token")).unwrap().reveal(), "leased-value");
    assert_eq!(mints(&server), 1);
}
