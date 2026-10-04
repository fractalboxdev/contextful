//! `connector.resolve`: the provider chain, its cache and its gates.

use crate::support::{name, Fixed, SetClock};
use contextful_core::connector::reference::{Hydrated, Template, SENTINEL};
use contextful_core::connector::resolve::Provider;
use contextful_outbound::secrets::{assemble, EnvProvider};
use contextful_outbound::Resolver;
use std::collections::BTreeMap;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

fn resolver(chain: Vec<Arc<dyn Provider>>, clock: &SetClock) -> Resolver {
    Resolver::new(chain, false, Arc::new(clock.clone()))
}

fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// One port answers a reference with material. Every backend is an adapter behind it, and
/// `CONTEXTFUL_SECRETS_BACKEND` selects the adapters the chain assembles.
// spec: connector.resolve.provider-port@3090f72d
#[test]
fn the_backend_variable_selects_the_adapters() {
    let clock = SetClock::new();
    // Unset, the chain is the environment adapter, which answers no template.
    let r = assemble(&vars(&[("VENDOR_TOKEN", "t")]), Arc::new(clock.clone())).unwrap();
    assert!(r.hydrate(&name("vendor-token")).unwrap_err().message.starts_with("SecretUnresolvedReference"));
    // `lease` joins at the head when named.
    let with_lease = vars(&[("CONTEXTFUL_SECRETS_BACKEND", "lease,env"), ("CONTEXTFUL_LEASE_SCOPES", "vendor-token"), ("CONTEXTFUL_LEASE_PROVIDER_URL", "http://127.0.0.1:9")]);
    assert!(assemble(&with_lease, Arc::new(clock.clone())).is_ok());
    // An adapter this build does not link refuses at assembly.
    let err = assemble(&vars(&[("CONTEXTFUL_SECRETS_BACKEND", "vault")]), Arc::new(clock.clone())).err().unwrap();
    assert!(err.message.contains("vault"), "{err}");
    // Every adapter answers through the one port.
    let fixed: Arc<dyn Provider> = Fixed::new("manager", &[("vendor-token", "from-manager")]);
    let env: Arc<dyn Provider> = Arc::new(EnvProvider::new(vars(&[("VENDOR_TOKEN", "from-env")])));
    for (p, want) in [(fixed, "from-manager"), (env, "from-env")] {
        assert_eq!(p.answer(&name("vendor-token")).unwrap().unwrap().value.reveal(), want);
    }
}

/// Hydration stops at the earliest adapter answering the name, and later adapters are not consulted for that
/// reference during the run.
// spec: connector.resolve.first-hit-wins@39c91a7f
#[test]
fn the_earliest_answer_serves_and_later_adapters_are_left_alone() {
    let clock = SetClock::new();
    let (first, later) = (Fixed::new("first", &[("vendor-token", "one")]), Fixed::new("later", &[]));
    let r = resolver(vec![first.clone(), later.clone()], &clock);
    assert_eq!(r.hydrate(&name("vendor-token")).unwrap().reveal(), "one");
    let consulted = later.calls();
    // Across cache expiries, the later adapter is not asked again, even once it holds the name.
    later.set("vendor-token", "two");
    for _ in 0..3 {
        clock.advance(301);
        assert_eq!(r.hydrate(&name("vendor-token")).unwrap().reveal(), "one");
    }
    assert_eq!(later.calls(), consulted);
    assert_eq!(r.attribution().get("vendor-token").map(String::as_str), Some("first"));
}

/// At first hydration, a name answered by the serving adapter and by any adapter behind it raises
/// `SecretNameShadowed`, naming the logical name and both adapters.
// spec: connector.resolve.shadowed-name@6e255560
#[test]
fn a_name_two_adapters_answer_is_refused_at_first_hydration() {
    let clock = SetClock::new();
    let r = resolver(vec![Fixed::new("manager", &[("vendor-token", "a")]), Fixed::new("keychain", &[]), Fixed::new("stray", &[("vendor-token", "b")])], &clock);
    let f = r.hydrate(&name("vendor-token")).unwrap_err();
    assert!(f.message.starts_with("SecretNameShadowed"), "{f}");
    for part in ["vendor-token", "manager", "stray"] {
        assert!(f.message.contains(part), "{part} missing from {f}");
    }
    assert!(!f.message.contains("`a`") && !f.message.contains("`b`"));
}

/// Every hydrated value, a pure-literal template included, rides a wrapper whose debug and display forms print a
/// fixed sentinel. The bytes are revealed only where the host writes the request.
// spec: connector.resolve.redacting-wrapper@49916b92
#[test]
fn a_hydrated_value_prints_as_the_sentinel() {
    let clock = SetClock::new();
    let r = resolver(vec![Fixed::new("manager", &[("vendor-token", "s3cr3t-value")])], &clock);
    for t in ["Bearer ${secret://vendor-token}", "no reference at all"] {
        let v: Hydrated = r.render(&Template::parse(t).unwrap()).unwrap();
        assert_eq!(format!("{v}"), SENTINEL);
        assert_eq!(format!("{v:?}"), SENTINEL);
        assert_eq!(format!("{:?}", Some(v)), format!("Some({SENTINEL})"));
    }
    assert_eq!(r.render(&Template::parse("Bearer ${secret://vendor-token}").unwrap()).unwrap().reveal(), "Bearer s3cr3t-value");
}

/// One resolver with its own cache is built per source, and concurrent first hydrations of one name collapse
/// under a single-flight gate.
// spec: connector.resolve.resolver-per-source@14d0bf74
#[test]
fn concurrent_first_hydrations_make_one_call_and_resolvers_share_no_cache() {
    let clock = SetClock::new();
    let slow = Arc::new(Fixed { label: "manager", values: Mutex::new(vec![("vendor-token".into(), "v".into())]), calls: AtomicUsize::new(0), delay_ms: 100, });
    let r = Arc::new(resolver(vec![slow.clone()], &clock));
    std::thread::scope(|s| {
        for _ in 0..8 {
            let r = r.clone();
            s.spawn(move || assert_eq!(r.hydrate(&name("vendor-token")).unwrap().reveal(), "v"));
        }
    });
    assert_eq!(slow.calls(), 1);
    // A second source's resolver keeps its own cache.
    let other = resolver(vec![slow.clone()], &clock);
    other.hydrate(&name("vendor-token")).unwrap();
    assert_eq!(slow.calls(), 2);
}

/// A cache entry lives at most 300 s.
// spec: connector.resolve.cache-ttl@95b0f721
#[test]
fn a_cached_value_retires_at_300_s() {
    assert_eq!(contextful_core::connector::lease::CACHE_TTL_SECS, 300);
    let clock = SetClock::new();
    let p = Fixed::new("manager", &[("vendor-token", "v")]);
    let r = resolver(vec![p.clone()], &clock);
    r.hydrate(&name("vendor-token")).unwrap();
    clock.advance(299);
    r.hydrate(&name("vendor-token")).unwrap();
    assert_eq!(p.calls(), 1);
    clock.advance(1);
    r.hydrate(&name("vendor-token")).unwrap();
    assert_eq!(p.calls(), 2);
}

#[test]
fn cached_hydrations_share_one_buffer_until_retirement() {
    let clock = SetClock::new();
    let p = Fixed::new("manager", &[("vendor-token", "first")]);
    let r = resolver(vec![p.clone()], &clock);
    let first = r.hydrate(&name("vendor-token")).unwrap();
    let second = r.hydrate(&name("vendor-token")).unwrap();
    assert!(Arc::ptr_eq(&first, &second), "a cache hit copied the credential bytes");
    clock.advance(300);
    let renewed = r.hydrate(&name("vendor-token")).unwrap();
    assert!(!Arc::ptr_eq(&first, &renewed), "the expired buffer stayed cached");
    let retired = Arc::downgrade(&first);
    drop(first);
    drop(second);
    assert!(retired.upgrade().is_none(), "the retired buffer stayed alive");
    assert_eq!(p.calls(), 2);
}

/// An expired credential leaves the resolver even when its provider cannot renew it.
#[test]
fn an_expired_buffer_is_released_when_refresh_fails() {
    let clock = SetClock::new();
    let provider = Fixed::new("manager", &[("vendor-token", "first")]);
    let resolver = resolver(vec![provider.clone()], &clock);
    let value = resolver.hydrate(&name("vendor-token")).unwrap();
    let retired = Arc::downgrade(&value);
    drop(value);
    provider.values.lock().unwrap().clear();
    clock.advance(300);
    let failure = resolver.hydrate(&name("vendor-token")).unwrap_err();
    assert!(failure.message.starts_with("SecretUnresolvedReference"), "{failure}");
    assert!(retired.upgrade().is_none(), "the expired credential stayed in the cache after refresh failed");
}

#[test]
fn an_active_resolver_releases_other_expired_credentials() {
    let clock = SetClock::new();
    let provider = Fixed::new("manager", &[("first-token", "first"), ("second-token", "second")]);
    let resolver = resolver(vec![provider], &clock);
    let first = resolver.hydrate(&name("first-token")).unwrap();
    let retired = Arc::downgrade(&first);
    drop(first);
    clock.advance(300);
    assert_eq!(resolver.hydrate(&name("second-token")).unwrap().reveal(), "second");
    assert!(retired.upgrade().is_none(), "an unrelated expired credential remained in the active resolver");
}

/// A reference no assembled adapter answers raises `SecretUnresolvedReference` naming it, at preflight where the
/// binding is static and at the call otherwise.
// spec: connector.resolve.unresolved-name@01714613
#[test]
fn an_unanswered_reference_refuses_at_preflight_and_at_the_call() {
    let clock = SetClock::new();
    let r = resolver(vec![Fixed::new("manager", &[("other", "x")])], &clock);
    let t = Template::parse("Bearer ${secret://vendor-token}").unwrap();
    let f = r.preflight([&t]).unwrap_err();
    assert!(f.message.starts_with("SecretUnresolvedReference") && f.message.contains("vendor-token"), "{f}");
    let f = r.render(&t).unwrap_err();
    assert!(f.message.starts_with("SecretUnresolvedReference"), "{f}");
    assert!(r.preflight([&Template::parse("Bearer ${secret://other}").unwrap()]).is_ok());
}

/// Template hydration treats the process-environment adapter as a miss and continues down the chain; a name
/// nothing answers falls to {{connector.resolve.unresolved-name}}.
// spec: connector.reference.environment-is-a-miss@b7558178
#[test]
fn the_environment_answers_no_template() {
    let clock = SetClock::new();
    let env: Arc<dyn Provider> = Arc::new(EnvProvider::new(vars(&[("VENDOR_TOKEN", "from-env"), ("OTHER_TOKEN", "env-only")])));
    let manager = Fixed::new("manager", &[("vendor-token", "from-manager")]);
    let r = resolver(vec![env.clone(), manager], &clock);
    // The environment holds the name and sits first; hydration continues past it.
    assert_eq!(r.hydrate(&name("vendor-token")).unwrap().reveal(), "from-manager");
    assert!(r.hydrate(&name("other-token")).unwrap_err().message.starts_with("SecretUnresolvedReference"));
    // With the opt-in, the environment serves templates again.
    let opted = Resolver::new(vec![env], true, Arc::new(clock.clone()));
    assert_eq!(opted.hydrate(&name("other-token")).unwrap().reveal(), "env-only");
}

/// A `secret://<name>` bound whole, outside any template, hydrates through every assembled adapter, the process
/// environment included, under `connector.resolve.first-hit-wins` and `connector.resolve.shadowed-name`.
// spec: connector.reference.whole-value-reference@6d0cffce
#[test]
fn a_whole_value_reference_consults_the_environment() {
    let clock = SetClock::new();
    // The default chain is the environment alone: a template misses, a whole value answers.
    let r = assemble(&vars(&[("SYNC_KEY_ID", "from-env")]), Arc::new(clock.clone())).unwrap();
    assert!(r.hydrate(&name("sync-key-id")).unwrap_err().message.starts_with("SecretUnresolvedReference"));
    assert_eq!(r.resolve(&name("sync-key-id")).unwrap().reveal(), "from-env");
    assert!(r.resolve(&name("sync-absent")).unwrap_err().message.starts_with("SecretUnresolvedReference"));
    assert_eq!(r.attribution().get("sync-key-id").map(String::as_str), Some("env"));
    // A whole-value answer never reaches a template through the cache.
    assert!(r.hydrate(&name("sync-key-id")).unwrap_err().message.starts_with("SecretUnresolvedReference"));
    // An adapter ahead of the environment answers first; the environment answering too shadows it.
    let env: Arc<dyn Provider> = Arc::new(EnvProvider::new(vars(&[("VENDOR_TOKEN", "from-env")])));
    let both: Vec<Arc<dyn Provider>> = vec![Fixed::new("manager", &[("vendor-token", "from-manager")]), env.clone()];
    assert!(resolver(both, &clock).resolve(&name("vendor-token")).unwrap_err().message.starts_with("SecretNameShadowed"));
    let other: Vec<Arc<dyn Provider>> = vec![Fixed::new("manager", &[("other-token", "from-manager")]), env];
    assert_eq!(resolver(other, &clock).resolve(&name("vendor-token")).unwrap().reveal(), "from-env");
}
