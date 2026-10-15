//! `connector.record`: the observe halves the inventory holds.

use crate::support::{at, name, proxy_env, Response, Server, SetClock, T0};
use contextful_core::connector::record::{Expiry, Observation, Observe, Unavailable};
use contextful_core::connector::reference::SecretName;
use contextful_core::connector::resolve::{Answer, Provider};
use contextful_core::run::Failure;
use contextful_core::time::Instant;
use contextful_outbound::secrets::assemble_all;
use std::collections::BTreeMap;
use std::sync::Arc;

fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// An adapter whose answer half panics, so any hydration during observation fails the test.
struct Hydrating;

impl Provider for Hydrating {
    fn name(&self) -> &str {
        "manager"
    }
    fn answer(&self, _: &SecretName) -> Result<Option<Answer>, Failure> {
        panic!("observation hydrated a value")
    }
}

impl Observe for Hydrating {
    fn adapter(&self) -> &str {
        "manager"
    }
    fn observe(&self, _: &SecretName, at: Instant) -> Observation {
        Observation::Observed { adapter: "manager".into(), at, present: true, version: Some("3".into()), created: None, expires: Expiry::At(at.plus_secs(86_400)) }
    }
}

/// A manager whose metadata read is denied.
struct Denying;

impl Observe for Denying {
    fn adapter(&self) -> &str {
        "manager"
    }
    fn observe(&self, _: &SecretName, at: Instant) -> Observation {
        Observation::Unavailable { adapter: "manager".into(), at, reason: Unavailable::Denied("metadata read refused (403)".into()) }
    }
}

/// An observe half answers presence, version, creation and expiry, or unavailable with a
/// reason, and holds no hydrated value; the lease provider's would mint, so it answers
/// unsupported and the mint endpoint sees 0 requests.
// spec: connector.record.observe-port@8be52414
#[test]
fn observe_halves_answer_metadata_and_never_hydrate() {
    let _env = proxy_env();
    let server = Server::start(|_| Response::json(200, r#"{"value":"leased","expires_in":600}"#));
    let clock = SetClock::new();
    let now = at(T0);
    let a = assemble_all(
        &vars(&[
            ("CONTEXTFUL_SECRETS_BACKEND", "lease,env"),
            ("CONTEXTFUL_LEASE_SCOPES", "vendor-token"),
            ("CONTEXTFUL_LEASE_PROVIDER_URL", server.url("").as_str()),
            ("LEASE_MINT", "mint"),
            ("SPEECH_VENDOR", "sentinel-env-value"),
        ]),
        Arc::new(clock),
    )
    .unwrap();
    assert_eq!(a.configured(&name("vendor-token")), ["lease", "env"]);
    assert_eq!(a.configured(&name("speech-vendor")), ["env"]);
    let seen: Vec<Observation> = a.observers.iter().map(|o| o.observe(&name("vendor-token"), now)).collect();
    assert!(matches!(&seen[0], Observation::Unavailable { adapter, reason: Unavailable::Unsupported(_), .. } if adapter == "lease"), "{seen:?}");
    assert!(matches!(&seen[1], Observation::Observed { adapter, present: false, .. } if adapter == "env"), "{seen:?}");
    let env = a.observers[1].observe(&name("speech-vendor"), now);
    assert!(matches!(env, Observation::Observed { present: true, .. }));
    assert!(!format!("{env:?}").contains("sentinel-env-value"));
    assert_eq!(server.received("/leases").len(), 0);

    // An adapter whose answer half panics observes to completion.
    let manager: Arc<dyn Observe> = Arc::new(Hydrating);
    assert!(matches!(manager.observe(&name("vendor-token"), now), Observation::Observed { present: true, .. }));
}

/// An unsupported, denied or failed read appears as unavailable with its reason.
// spec: connector.record.observation-unavailable@59a169eb
#[test]
fn a_denied_metadata_read_reports_its_reason() {
    let o = Denying.observe(&name("vendor-token"), at(T0));
    match o {
        Observation::Unavailable { adapter, at: when, reason: Unavailable::Denied(why) } => {
            assert_eq!((adapter.as_str(), when), ("manager", at(T0)));
            assert!(why.contains("403"));
        }
        other => panic!("expected unavailable, got {other:?}"),
    }
}
