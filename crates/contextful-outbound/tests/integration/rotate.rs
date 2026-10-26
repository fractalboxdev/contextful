//! `connector.rotate`: refresh, the conditional put to the answering adapter, and turnover.

use crate::support::{at, name, proxy_env, Response, Server, SetClock, T0};
use contextful_core::connector::reference::{Hydrated, SecretName, Template};
use contextful_core::connector::resolve::{Answer, Provider};
use contextful_core::connector::rotate::{refresh_at, PutError, Version, VersionedStore};
use contextful_core::run::{Failure, FailureTag};
use contextful_core::time::Instant;
use contextful_outbound::secrets::{assemble_all, Rotator};
use contextful_outbound::Resolver;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// A customer-operated manager: versioned storage answering reads and conditional puts.
struct Manager {
    stored: Mutex<(String, u32)>,
    puts: Mutex<Vec<(String, Option<String>, Option<Instant>)>>,
    deny: AtomicBool,
    answers: AtomicUsize,
}

impl Manager {
    fn new(value: &str) -> Arc<Manager> {
        Arc::new(Manager { stored: Mutex::new((value.to_string(), 1)), puts: Mutex::default(), deny: AtomicBool::new(false), answers: AtomicUsize::new(0) })
    }

    /// Another writer stores `value` as the next version.
    fn overwrite(&self, value: &str) {
        let mut s = self.stored.lock().unwrap();
        *s = (value.to_string(), s.1 + 1);
    }
}

impl Provider for Manager {
    fn name(&self) -> &str {
        "manager"
    }
    fn answer(&self, n: &SecretName) -> Result<Option<Answer>, Failure> {
        if n.as_str() != "vendor-token" {
            return Ok(None);
        }
        self.answers.fetch_add(1, Ordering::SeqCst);
        let s = self.stored.lock().unwrap();
        Ok(Some(Answer { value: Hydrated::new(s.0.clone()), expires_at: None, version: Some(s.1.to_string()) }))
    }
}

impl VersionedStore for Manager {
    fn adapter(&self) -> &str {
        "manager"
    }
    fn put(&self, n: &SecretName, value: &Hydrated, if_version: Option<&str>, expires_hint: Option<Instant>) -> Result<Version, PutError> {
        self.puts.lock().unwrap().push((n.to_string(), if_version.map(str::to_string), expires_hint));
        if self.deny.load(Ordering::SeqCst) {
            return Err(PutError::Denied("write access refused".into()));
        }
        let mut s = self.stored.lock().unwrap();
        if if_version != Some(s.1.to_string().as_str()) {
            return Err(PutError::Conflict(s.1.to_string()));
        }
        *s = (value.reveal().to_string(), s.1 + 1);
        Ok(s.1.to_string())
    }
}

fn rig(manager: &Arc<Manager>, clock: &SetClock) -> (Arc<Resolver>, Rotator) {
    let chain: Vec<Arc<dyn Provider>> = vec![manager.clone()];
    let resolver = Arc::new(Resolver::new(chain, false, Arc::new(clock.clone())));
    let writers: BTreeMap<String, Arc<dyn VersionedStore>> = [("manager".to_string(), manager.clone() as Arc<dyn VersionedStore>)].into();
    (resolver.clone(), Rotator::new(resolver, writers))
}

fn refreshed(value: &'static str, hint: Option<Instant>) -> impl FnOnce(&Hydrated) -> Result<(Hydrated, Option<Instant>), Failure> {
    move |_| Ok((Hydrated::new(value), hint))
}

/// An OAuth credential refreshes at the retirement-margin point, puts the new version to the
/// adapter that answered its name, conditioned on the hydrated version, then clears the entry.
// spec: connector.record.provider-custody@1dd8cfd2
// spec: connector.rotate.oauth-credential@391c7c68
#[test]
fn a_refresh_puts_forward_to_the_answering_adapter_conditioned_on_the_hydrated_version() {
    let t0 = at(T0);
    assert_eq!(refresh_at(t0, t0.plus_secs(100)), t0.plus_secs(90));
    assert_eq!(refresh_at(t0, t0.plus_secs(3600)), t0.plus_secs(3570));
    let manager = Manager::new("old-token");
    let clock = SetClock::new();
    let (resolver, rotator) = rig(&manager, &clock);
    assert_eq!(resolver.hydrate(&name("vendor-token")).unwrap().reveal(), "old-token");
    rotator.rotate(&name("vendor-token"), refreshed("new-token", None)).unwrap();
    assert_eq!(manager.puts.lock().unwrap()[0].1.as_deref(), Some("1"));
    // The manager owns the new version; the resolver reads it back at its next hydration.
    assert_eq!(resolver.hydrate(&name("vendor-token")).unwrap().reveal(), "new-token");
    assert_eq!(resolver.hydrated_version(&name("vendor-token")).as_deref(), Some("2"));
}

/// The store contributes versioned storage; the host drives the refresh and writes back
/// as a blind versioned put.
// spec: connector.rotate.division-of-labor@73f1cbf2
#[test]
fn the_host_drives_the_refresh_and_the_store_only_stores() {
    let manager = Manager::new("old-token");
    let (_, rotator) = rig(&manager, &SetClock::new());
    let seen = Mutex::new(String::new());
    rotator
        .rotate(&name("vendor-token"), |current| {
            *seen.lock().unwrap() = current.reveal().to_string();
            Ok((Hydrated::new("new-token"), None))
        })
        .unwrap();
    assert_eq!(seen.lock().unwrap().as_str(), "old-token");
    assert_eq!(manager.stored.lock().unwrap().0, "new-token");
}

/// The put carries an opaque expiry hint beside the value.
// spec: connector.rotate.division-of-labor@73f1cbf2
#[test]
fn the_put_carries_the_refreshed_expiry_as_a_hint() {
    let manager = Manager::new("old-token");
    let (_, rotator) = rig(&manager, &SetClock::new());
    let hint = at(T0).plus_secs(3600);
    rotator.rotate(&name("vendor-token"), refreshed("new-token", Some(hint))).unwrap();
    assert_eq!(manager.puts.lock().unwrap()[0].2, Some(hint));
}

/// A refused put, or an answering adapter with no write half, fails closed and the refreshed
/// token is dropped unserved.
// spec: connector.rotate.write-back-denied@13fed6fb
#[test]
fn a_refused_put_fails_closed_and_the_refreshed_token_is_never_served() {
    let manager = Manager::new("old-token");
    manager.deny.store(true, Ordering::SeqCst);
    let (resolver, rotator) = rig(&manager, &SetClock::new());
    let f = rotator.rotate(&name("vendor-token"), refreshed("new-token", None)).unwrap_err();
    assert!(f.message.starts_with("SecretRotationWriteDenied"), "{f}");
    assert_eq!(f.tag, FailureTag::Permanent);
    assert!(!f.message.contains("new-token"));
    assert_ne!(resolver.hydrate(&name("vendor-token")).unwrap().reveal(), "new-token");

    // The environment adapter has no write half: the refresh never runs.
    let env = assemble_all(&[("VENDOR_TOKEN".to_string(), "env-token".to_string())].into(), Arc::new(SetClock::new())).unwrap();
    assert!(env.writers.is_empty());
    let resolver = Arc::new(env.resolver);
    resolver.resolve(&name("vendor-token")).unwrap();
    let rotator = Rotator::new(resolver, env.writers);
    let f = rotator.rotate(&name("vendor-token"), |_| panic!("refreshed with no write half")).unwrap_err();
    assert!(f.message.starts_with("SecretRotationWriteDenied") && f.message.contains("`env`"), "{f}");
}

/// A put losing to another writer re-hydrates that writer's version and raises nothing.
// spec: connector.rotate.conditional-put@3eec24df
#[test]
fn a_put_losing_to_another_writer_adopts_its_version() {
    let manager = Manager::new("old-token");
    let (resolver, rotator) = rig(&manager, &SetClock::new());
    resolver.hydrate(&name("vendor-token")).unwrap();
    manager.overwrite("replica-token");
    rotator.rotate(&name("vendor-token"), refreshed("late-token", None)).unwrap();
    assert_eq!(resolver.hydrated_version(&name("vendor-token")).as_deref(), Some("2"));
    assert_eq!(resolver.hydrate(&name("vendor-token")).unwrap().reveal(), "replica-token");
    assert_eq!(manager.stored.lock().unwrap().0, "replica-token");
}

/// A static key turns over with no engine step: the next cache miss hydrates the current version.
// spec: connector.rotate.static-key@7392e919
#[test]
fn a_static_key_turns_over_at_the_next_cache_miss() {
    let manager = Manager::new("key-v1");
    let clock = SetClock::new();
    let (resolver, _) = rig(&manager, &clock);
    assert_eq!(resolver.hydrate(&name("vendor-token")).unwrap().reveal(), "key-v1");
    manager.overwrite("key-v2");
    clock.advance(299);
    assert_eq!(resolver.hydrate(&name("vendor-token")).unwrap().reveal(), "key-v1");
    clock.advance(1);
    assert_eq!(resolver.hydrate(&name("vendor-token")).unwrap().reveal(), "key-v2");
}

/// Every rotation keeps the logical name: the same template renders the new version.
// spec: connector.rotate.turnover-preserves-the-name@c6642ec4
#[test]
fn a_rotation_keeps_the_name_and_the_template() {
    let manager = Manager::new("old-token");
    let (resolver, rotator) = rig(&manager, &SetClock::new());
    let template = Template::parse("Bearer ${secret://vendor-token}").unwrap();
    assert_eq!(resolver.render(&template).unwrap().reveal(), "Bearer old-token");
    rotator.rotate(&name("vendor-token"), refreshed("new-token", None)).unwrap();
    assert_eq!(resolver.render(&template).unwrap().reveal(), "Bearer new-token");
}

/// The logical name joins the attribution, the cache entry and the put target.
// spec: connector.reference.name-is-the-join@5780e1ab
#[test]
fn the_logical_name_joins_attribution_cache_and_put() {
    let manager = Manager::new("old-token");
    let (resolver, rotator) = rig(&manager, &SetClock::new());
    resolver.hydrate(&name("vendor-token")).unwrap();
    assert_eq!(resolver.attribution().get("vendor-token").map(String::as_str), Some("manager"));
    rotator.rotate(&name("vendor-token"), refreshed("new-token", None)).unwrap();
    assert_eq!(manager.puts.lock().unwrap()[0].0, "vendor-token");
}

/// A development secret has no write half; an operator re-enters it by re-pointing the reference.
// spec: connector.rotate.operator-entered@e366a2eb
#[test]
fn an_environment_secret_turns_over_only_by_re_entry() {
    let a = assemble_all(&[("VENDOR_TOKEN".to_string(), "first".to_string())].into(), Arc::new(SetClock::new())).unwrap();
    assert!(a.writers.is_empty());
    assert_eq!(a.resolver.resolve(&name("vendor-token")).unwrap().reveal(), "first");
    let again = assemble_all(&[("VENDOR_TOKEN".to_string(), "second".to_string())].into(), Arc::new(SetClock::new())).unwrap();
    assert_eq!(again.resolver.resolve(&name("vendor-token")).unwrap().reveal(), "second");
}

/// A leased credential re-mints with no write-back; a failed re-mint leaves no credential.
// spec: connector.rotate.leased-credential@90197a3f
#[test]
fn a_lease_re_mints_without_write_back_and_a_failed_mint_leaves_nothing() {
    let _env = proxy_env();
    let up = Arc::new(AtomicBool::new(true));
    let gate = up.clone();
    let server = Server::start(move |_| {
        if gate.load(Ordering::SeqCst) {
            Response::json(200, r#"{"value":"leased","expires_in":100}"#)
        } else {
            Response::json(503, "{}")
        }
    });
    let clock = SetClock::new();
    let vars: BTreeMap<String, String> = [
        ("CONTEXTFUL_SECRETS_BACKEND", "lease,env"),
        ("CONTEXTFUL_LEASE_SCOPES", "vendor-token"),
        ("CONTEXTFUL_LEASE_PROVIDER_URL", server.url("").as_str()),
        ("LEASE_MINT", "mint"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let a = assemble_all(&vars, Arc::new(clock.clone())).unwrap();
    assert!(!a.writers.contains_key("lease"));
    assert_eq!(a.resolver.hydrate(&name("vendor-token")).unwrap().reveal(), "leased");
    clock.advance(90);
    assert_eq!(a.resolver.hydrate(&name("vendor-token")).unwrap().reveal(), "leased");
    assert_eq!(server.received("/leases").len(), 2);
    up.store(false, Ordering::SeqCst);
    clock.advance(90);
    let f = a.resolver.hydrate(&name("vendor-token")).unwrap_err();
    assert_eq!(f.tag, FailureTag::Transient);
}
