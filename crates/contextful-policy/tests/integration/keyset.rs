//! `authority.verify` and `authority.revoke`: the checkpoint's issuer key set — static
//! pins or a published key route — and the public-key-only checkpoint that holds it.

use contextful_core::issue::SignatureAlgorithm;
use contextful_core::ports::Clock;
use contextful_core::time::Instant;
use contextful_core::AuthorityError;
use contextful_policy::keyset::{
    KeyCheckpoint, KeySet, KeySetFetcher, KeySource, PublishedKeySet, StaticPins, KEY_SET_FORCED_REFRESH_MIN_SECS,
    KEY_SET_REFRESH_INTERVAL_SECS, KEY_SET_STALE_CEILING_SECS,
};
use contextful_policy::possession::{jwk_thumbprint, sign_proof, ProofRequest};
use ed25519_dalek::SigningKey;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

const T0: i64 = 1_893_456_000; // 2030-01-01T00:00:00Z

/// A clock the test advances.
#[derive(Clone)]
struct StepClock(Arc<AtomicI64>);

impl StepClock {
    fn new() -> Self {
        StepClock(Arc::new(AtomicI64::new(T0)))
    }
    fn set(&self, secs_after_t0: i64) {
        self.0.store(T0 + secs_after_t0, Ordering::SeqCst);
    }
}

impl Clock for StepClock {
    fn now(&self) -> Instant {
        Instant::from_unix_secs(self.0.load(Ordering::SeqCst)).unwrap()
    }
}

/// A key route the test controls: its answer, a per-fetch delay, and a fetch counter.
struct Route {
    answer: Mutex<Result<String, String>>,
    delay: Duration,
    fetches: AtomicUsize,
}

impl Route {
    fn serving(doc: String) -> Arc<Route> {
        Arc::new(Route { answer: Mutex::new(Ok(doc)), delay: Duration::ZERO, fetches: AtomicUsize::new(0) })
    }
    fn slow(doc: String, delay: Duration) -> Arc<Route> {
        Arc::new(Route { answer: Mutex::new(Ok(doc)), delay, fetches: AtomicUsize::new(0) })
    }
    fn down() -> Arc<Route> {
        let r = Route::serving(String::new());
        r.go_down();
        r
    }
    fn serve(&self, doc: String) {
        *self.answer.lock().unwrap() = Ok(doc);
    }
    fn go_down(&self) {
        *self.answer.lock().unwrap() = Err("connection refused".into());
    }
    fn fetches(&self) -> usize {
        self.fetches.load(Ordering::SeqCst)
    }
}

impl KeySetFetcher for Route {
    fn fetch(&self) -> Result<Vec<u8>, String> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(self.delay);
        self.answer.lock().unwrap().clone().map(String::into_bytes)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// An issuer public key in the pin grammar, from a fixed seed.
fn pin(seed: u8) -> String {
    format!("ed25519/{}", hex(SigningKey::from_bytes(&[seed; 32]).verifying_key().as_bytes()))
}

/// A published key-set document carrying `(version, seed)` keys.
fn doc(keys: &[(&str, u8)]) -> String {
    let entries: Vec<String> =
        keys.iter().map(|(v, s)| format!("{{\"version\":\"{v}\",\"key\":\"{}\"}}", pin(*s))).collect();
    format!("{{\"keys\":[{}]}}", entries.join(","))
}

fn versions(set: &KeySet) -> Vec<String> {
    set.versions().map(str::to_owned).collect()
}

fn published(route: &Arc<Route>, clock: &StepClock) -> KeyCheckpoint<StepClock> {
    let source = PublishedKeySet::new(route.clone(), clock.clone());
    KeyCheckpoint::start(Box::new(source), clock.clone()).expect("the route serves a set")
}

/// A checkpoint accepts a set of issuer keys: comma-separated static pins, or a project-published unauthenticated key route returning the current and non-retired versions, opted into per key.
// spec: authority.verify.key-set@37cf71d7
#[test]
fn a_checkpoint_accepts_comma_separated_pins_or_a_published_key_route() {
    // Static pins: comma-separated, each optionally named `<version>=`.
    let pins = StaticPins::parse(&format!("k1={}, {}", pin(1), pin(2))).unwrap();
    let set = pins.keys().unwrap();
    assert_eq!(set.len(), 2);
    assert_eq!(set.get("k1").unwrap().public_key.to_bytes(), SigningKey::from_bytes(&[1; 32]).verifying_key().as_bytes());
    assert_eq!(set.get("k1").unwrap().algorithm(), SignatureAlgorithm::Ed25519);
    // An unnamed pin's version is its own key text.
    assert!(set.get(&pin(2)).is_some());

    // A published route returns the current and non-retired versions; the set holds exactly those.
    let clock = StepClock::new();
    let route = Route::serving(doc(&[("2030-01", 3), ("2029-10", 4)]));
    let cp = published(&route, &clock);
    assert_eq!(versions(&cp.keys().unwrap()), ["2030-01", "2029-10"]);

    // Opted into per key: each configured key chooses its source, and the checkpoint takes either.
    let sources: Vec<Box<dyn KeySource>> =
        vec![Box::new(pins), Box::new(PublishedKeySet::new(route.clone(), clock.clone()))];
    for source in sources {
        let cp = KeyCheckpoint::start(source, clock.clone()).unwrap();
        assert_eq!(cp.keys().unwrap().len(), 2);
    }
}

/// A published key set refreshes every 300 s, single-flight, plus one refresh on a signature failure, with failure-forced refreshes throttled.
// spec: authority.verify.key-set-refresh@4b1e7186
#[test]
fn a_published_key_set_refreshes_every_300_s_single_flight_and_once_on_signature_failure() {
    assert_eq!(KEY_SET_REFRESH_INTERVAL_SECS, 300);
    let clock = StepClock::new();
    let route = Route::serving(doc(&[("v1", 1)]));
    let cp = published(&route, &clock);
    assert_eq!(route.fetches(), 1);

    // Within 300 s the fetched set serves; at 300 s it refreshes.
    clock.set(299);
    cp.keys().unwrap();
    assert_eq!(route.fetches(), 1);
    route.serve(doc(&[("v1", 1), ("v2", 2)]));
    clock.set(300);
    assert_eq!(versions(&cp.keys().unwrap()), ["v1", "v2"]);
    assert_eq!(route.fetches(), 2);

    // Single-flight: eight admissions racing an expired set cause one fetch.
    let route = Route::slow(doc(&[("v1", 1)]), Duration::from_millis(100));
    let clock = StepClock::new();
    let cp = Arc::new(published(&route, &clock));
    clock.set(300);
    let barrier = Arc::new(Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let (cp, barrier) = (cp.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                cp.keys().unwrap().len()
            })
        })
        .collect();
    for t in threads {
        assert_eq!(t.join().unwrap(), 1);
    }
    assert_eq!(route.fetches(), 2, "one start fetch, one refresh for eight racers");

    // One refresh on a signature failure: a credential signed by a key published since
    // the last fetch admits after exactly one forced refresh.
    let clock = StepClock::new();
    let route = Route::serving(doc(&[("v1", 1)]));
    let cp = published(&route, &clock);
    route.serve(doc(&[("v1", 1), ("v2", 2)]));
    clock.set(10);
    let signed_by_v2 = |set: &KeySet| {
        set.get("v2").map(|_| ()).ok_or_else(|| AuthorityError::SignatureInvalid("no pinned key verifies".into()))
    };
    assert_eq!(cp.verify_with(signed_by_v2), Ok(()));
    assert_eq!(route.fetches(), 2);

    // A second failure inside the throttle forces no fetch and keeps the refusal.
    let signed_by_unknown = |_: &KeySet| -> Result<(), AuthorityError> {
        Err(AuthorityError::SignatureInvalid("no pinned key verifies".into()))
    };
    clock.set(10 + KEY_SET_FORCED_REFRESH_MIN_SECS as i64 - 1);
    assert!(matches!(cp.verify_with(signed_by_unknown), Err(AuthorityError::SignatureInvalid(_))));
    assert_eq!(route.fetches(), 2);
    // Past the throttle, a failure forces one refresh, and only one.
    clock.set(10 + KEY_SET_FORCED_REFRESH_MIN_SECS as i64);
    assert!(matches!(cp.verify_with(signed_by_unknown), Err(AuthorityError::SignatureInvalid(_))));
    assert_eq!(route.fetches(), 3);

    // A refusal other than a signature failure forces nothing.
    clock.set(10 + 2 * KEY_SET_FORCED_REFRESH_MIN_SECS as i64);
    let expired = |_: &KeySet| -> Result<(), AuthorityError> { Err(AuthorityError::AuthorityExpired("past exp".into())) };
    assert!(matches!(cp.verify_with(expired), Err(AuthorityError::AuthorityExpired(_))));
    assert_eq!(route.fetches(), 3);
}

/// A checkpoint opted into a published key set that obtains none, or holding a malformed pin, raises `KeySetUnavailable`: the engine declines to start and a gateway answers `503`.
// spec: authority.verify.key-set-unavailable@b0b291cd
#[test]
fn an_unobtainable_set_or_a_malformed_pin_raises_key_set_unavailable_and_declines_to_start() {
    let unavailable = |r: Result<StaticPins, AuthorityError>| match r {
        Err(AuthorityError::KeySetUnavailable(m)) => m,
        other => panic!("expected KeySetUnavailable, got {other:?}"),
    };
    for malformed in [
        String::new(),
        " , ".into(),
        "ed25519/zz".into(),
        "ed25519/00".into(),
        "rsa/00ff".into(),
        format!("{},", pin(1)),
        format!("=  {}", pin(1)),
        format!("k={},k={}", pin(1), pin(2)),
    ] {
        unavailable(StaticPins::parse(&malformed));
    }

    let clock = StepClock::new();
    let start = |route: &Arc<Route>| {
        KeyCheckpoint::start(Box::new(PublishedKeySet::new(route.clone(), clock.clone())), clock.clone())
    };
    // The route unreachable, answering a malformed document, or publishing no key: the checkpoint does not start.
    for route in [
        Route::down(),
        Route::serving("not json".into()),
        Route::serving("{\"keys\":[]}".into()),
        Route::serving("{\"keys\":[{\"version\":\"v1\",\"key\":\"ed25519/zz\"}]}".into()),
    ] {
        match start(&route) {
            Err(e @ AuthorityError::KeySetUnavailable(_)) => {
                assert!(e.to_string().starts_with("KeySetUnavailable"), "{e}")
            }
            Err(e) => panic!("expected KeySetUnavailable, got {e:?}"),
            Ok(_) => panic!("a checkpoint without a key set started"),
        }
    }
}

/// After a failed fetch the last-known-good set serves while its age since the last successful fetch stays under 1 h; past that the checkpoint raises `KeySetStale`.
// spec: authority.verify.key-set-stale@e336b7f5
#[test]
fn a_last_known_good_set_serves_under_one_hour_then_raises_key_set_stale() {
    assert_eq!(KEY_SET_STALE_CEILING_SECS, 3600);
    let clock = StepClock::new();
    let route = Route::serving(doc(&[("v1", 1)]));
    let cp = published(&route, &clock);
    route.go_down();

    clock.set(300);
    assert_eq!(versions(&cp.keys().unwrap()), ["v1"], "last-known-good after a failed fetch");
    assert_eq!(route.fetches(), 2);
    // A failed refresh is retried no more often than the forced-refresh throttle.
    clock.set(300 + KEY_SET_FORCED_REFRESH_MIN_SECS as i64 - 1);
    cp.keys().unwrap();
    assert_eq!(route.fetches(), 2);

    clock.set(3599);
    assert_eq!(versions(&cp.keys().unwrap()), ["v1"]);
    clock.set(3600);
    match cp.keys() {
        Err(e @ AuthorityError::KeySetStale(_)) => assert!(e.to_string().starts_with("KeySetStale"), "{e}"),
        other => panic!("expected KeySetStale, got {other:?}"),
    }

    // A successful fetch resets the age.
    route.serve(doc(&[("v2", 2)]));
    clock.set(3600 + KEY_SET_FORCED_REFRESH_MIN_SECS as i64);
    assert_eq!(versions(&cp.keys().unwrap()), ["v2"]);
}

/// A checkpoint holds public key material and no signing secret, and admits with no call to an issuer, identity provider or policy service.
// spec: authority.verify.public-key-only@f21aed64
#[test]
fn a_checkpoint_admits_from_public_keys_alone_with_no_call_out() {
    // The checkpoint's only inputs are a key source and a clock; a published source's
    // only outward call is the key route, made on refresh and never per admission.
    let clock = StepClock::new();
    let route = Route::serving(doc(&[("v1", 1)]));
    let cp = published(&route, &clock);
    let set = cp.keys().unwrap();
    // The key set carries public keys: the pinned bytes are the issuer's verifying key.
    assert_eq!(
        set.get("v1").unwrap().public_key.to_bytes(),
        SigningKey::from_bytes(&[1; 32]).verifying_key().as_bytes()
    );

    let client = SigningKey::from_bytes(&[9; 32]);
    let jkt = jwk_thumbprint(client.verifying_key().as_bytes());
    let req = ProofRequest { method: "GET", target: "https://store.example/v1/tables", body: b"" };
    for i in 0..1000 {
        clock.set(i / 10);
        let proof = sign_proof(&client, &req, clock.now(), &format!("n-{i}"));
        cp.verify_request(&jkt, &proof, &req).unwrap();
        cp.verify_with(|set| set.get("v1").map(|_| ()).ok_or_else(|| AuthorityError::SignatureInvalid(String::new())))
            .unwrap();
    }
    assert_eq!(route.fetches(), 1, "a thousand admissions, no call out past the start fetch");

    // A pinned checkpoint makes no call at all.
    let pinned = KeyCheckpoint::start(Box::new(StaticPins::parse(&pin(1)).unwrap()), clock.clone()).unwrap();
    assert_eq!(pinned.keys().unwrap().len(), 1);
}

/// Immediate retirement removes a key version from the published set and every static pin; a checkpoint drops it at its next refresh.
// spec: authority.revoke.immediate-retire@ba6788ab
#[test]
fn a_retired_version_drops_from_the_checkpoint_at_its_next_refresh() {
    let clock = StepClock::new();
    let route = Route::serving(doc(&[("v1", 1), ("v2", 2)]));
    let cp = published(&route, &clock);
    assert!(cp.keys().unwrap().get("v1").is_some());

    // The project retires v1: the route stops publishing it.
    route.serve(doc(&[("v2", 2)]));
    clock.set(299);
    assert!(cp.keys().unwrap().get("v1").is_some(), "held until the next refresh");
    clock.set(300);
    let set = cp.keys().unwrap();
    assert!(set.get("v1").is_none(), "dropped at the next refresh");
    assert_eq!(versions(&set), ["v2"]);

    // A signature failure forces that refresh early.
    let clock = StepClock::new();
    let route = Route::serving(doc(&[("v1", 1), ("v2", 2)]));
    let cp = published(&route, &clock);
    route.serve(doc(&[("v2", 2)]));
    clock.set(KEY_SET_FORCED_REFRESH_MIN_SECS as i64);
    let refused = |_: &KeySet| -> Result<(), AuthorityError> { Err(AuthorityError::SignatureInvalid(String::new())) };
    let _ = cp.verify_with(refused);
    assert!(cp.keys().unwrap().get("v1").is_none());

    // Static pins: the pin list without the retired version holds no trace of it.
    let pins = StaticPins::parse(&format!("v2={}", pin(2))).unwrap();
    assert!(pins.keys().unwrap().get("v1").is_none());
}

#[test]
fn a_bare_64_hex_pin_reads_as_ed25519_and_an_unknown_scheme_names_the_accepted_forms() {
    let bare = pin(1).strip_prefix("ed25519/").unwrap().to_owned();
    let set = StaticPins::parse(&bare).unwrap().keys().unwrap();
    assert_eq!(set.get(&pin(1)).unwrap().algorithm(), SignatureAlgorithm::Ed25519, "the version is the normalized text");
    assert_eq!(StaticPins::parse(&format!("k1={bare}")).unwrap().keys().unwrap().get("k1").unwrap().public_key.to_bytes(),
        SigningKey::from_bytes(&[1; 32]).verifying_key().as_bytes());
    for bad in [format!("rsa/{bare}"), "00ff".to_owned()] {
        match StaticPins::parse(&bad) {
            Err(AuthorityError::KeySetUnavailable(m)) => {
                assert!(m.contains("ed25519/<hex>") && m.contains("secp256r1/<hex>"), "{m}")
            }
            other => panic!("expected KeySetUnavailable, got {other:?}"),
        }
    }
}
