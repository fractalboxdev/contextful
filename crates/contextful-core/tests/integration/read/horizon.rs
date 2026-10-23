//! `read.horizon` and `authority.grant.horizon`: the availability bound a credential imposes.

use contextful_core::grant::{Action, Grant, TablePattern};
use contextful_core::read::error::ReadError;
use contextful_core::read::horizon::{horizon, inherit, within};
use contextful_core::store::bound_time::{Bound, Bounds};
use contextful_core::time::Instant;

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

fn read(h: Option<&str>) -> Grant {
    Grant {
        actions: vec![Action::Read],
        tables: vec![TablePattern::parse("research/*").unwrap()],
        tenant: None,
        aggregate: None,
        templates: None,
        max_rows: None,
        max_duration_ms: None,
        max_response_bytes: None,
        horizon: h.map(at),
    }
}

/// A grant may carry `horizon`, an instant bounding the availability time and `since` of every read its holder makes, under {{read.horizon.inherited-bound}} and {{read.horizon.exceeds-horizon}}; an absent horizon leaves availability unbounded.
// spec: authority.grant.horizon@909b876f
#[test]
fn a_grant_carries_a_horizon_and_an_absent_one_bounds_nothing() {
    let g: Grant = serde_json::from_value(serde_json::json!({ "actions": ["read"], "tables": ["research/*"], "horizon": "2015-01-01T00:00:00Z" })).unwrap();
    assert_eq!(g.horizon, Some(at("2015-01-01T00:00:00Z")));
    assert!(!serde_json::to_string(&read(None)).unwrap().contains("horizon"));

    assert_eq!(horizon(&[read(None)]), None);
    assert_eq!(inherit(None, Bounds::default()).unwrap(), Bounds::default());
    // The credential's horizon is the earliest any grant carries.
    let grants = [read(None), read(Some("2016-01-01T00:00:00Z")), read(Some("2015-01-01T00:00:00Z"))];
    assert_eq!(horizon(&grants), Some(at("2015-01-01T00:00:00Z")));
}

/// A call supplying `available_as_of` or `since` later than its credential's horizon raises `ReadBoundExceedsHorizon`, naming the argument; `as_of`, `valid_as_of` and `observed_at` pass unbounded by it.
// spec: read.horizon.exceeds-horizon@80b68d06
#[test]
fn a_bound_later_than_the_horizon_is_refused() {
    let h = Some(at("2015-01-01T00:00:00Z"));
    let available = |s: &str| Bounds { available_as_of: Some(Bound::parse(s).unwrap()), ..Bounds::default() };
    match inherit(h, available("2016-01-01T00:00:00Z")) {
        Err(ReadError::ReadBoundExceedsHorizon(m)) => assert!(m.contains("available_as_of"), "{m}"),
        other => panic!("expected ReadBoundExceedsHorizon, got {other:?}"),
    }
    match within(h, "since", Some(at("2015-06-01T00:00:00Z"))) {
        Err(ReadError::ReadBoundExceedsHorizon(m)) => assert!(m.contains("since"), "{m}"),
        other => panic!("expected ReadBoundExceedsHorizon, got {other:?}"),
    }
    // At the horizon itself, and a date-only bound ending there, pass.
    assert!(inherit(h, available("2015-01-01T00:00:00Z")).is_ok());
    assert!(inherit(h, available("2014-12-31")).is_ok());
    assert!(within(h, "since", Some(at("2014-06-01T00:00:00Z"))).is_ok());

    // `as_of` and `valid_as_of` pass beyond it, and the availability bound defaults to the horizon.
    let future = Bounds {
        as_of: Some(Bound::parse("2030-01-01T00:00:00Z").unwrap()),
        valid_as_of: Some(Bound::parse("2031-01-01T00:00:00Z").unwrap()),
        available_as_of: None,
        ..Bounds::default()
    };
    let inherited = inherit(h, future).unwrap();
    assert_eq!(inherited.as_of, future.as_of);
    assert_eq!(inherited.valid_as_of, future.valid_as_of);
    assert_eq!(inherited.available_as_of, Some(Bound { at: at("2015-01-01T00:00:00Z"), inclusive: true }));
}

/// One checkpoint of an embedder family, embedding every text as its own marker vector.
struct Checkpoint(&'static str);

impl contextful_core::read::embed::Embedder for Checkpoint {
    fn model(&self) -> &str {
        self.0
    }
    fn dimensions(&self) -> usize {
        1
    }
    fn embed(&self, _text: &str) -> Vec<f32> {
        vec![1.0]
    }
}

/// Under a horizon, an embedder declaring a checkpoint family selects its newest checkpoint whose
/// {{connector.infer.cutoffs}} post-training cutoff precedes that horizon. The response names the selected checkpoint.
// spec: read.horizon.embedder-route@7c33aa07
#[test]
fn an_embedder_family_routes_to_its_newest_checkpoint_before_the_horizon() {
    use contextful_core::connector::model_ref::{Cutoff, ModelRef};
    use contextful_core::read::embed::{CheckpointFamily, Embedder};
    let checkpoint = |name: &'static str, cutoff: Option<&str>| {
        (ModelRef { snapshot: name.into(), pretrain_cutoff: None, posttrain_cutoff: cutoff.map(|c| Cutoff::parse(c).unwrap()) }, Checkpoint(name))
    };
    let family = CheckpointFamily::new(vec![
        checkpoint("embed-2013", Some("2013-06:documented")),
        checkpoint("embed-2014", Some("2014-11:documented")),
        checkpoint("embed-2016", Some("2016-02:estimated")),
        checkpoint("embed-live", None),
    ]);
    let now = at("2030-01-01T00:00:00Z");

    // The 2014-11 cutoff ends on 1 December 2014, before the horizon; the 2016 one does not.
    let routed = family.route(Some(at("2015-01-01T00:00:00Z")), now).unwrap();
    assert_eq!(routed.model(), "embed-2014", "the vectors name the routed checkpoint");
    assert_eq!(family.route(Some(at("2014-12-01T00:00:00Z")), now).unwrap().model(), "embed-2014", "a cutoff ending at the horizon precedes it");
    assert_eq!(family.route(Some(at("2014-11-30T00:00:00Z")), now).unwrap().model(), "embed-2013");
    // No checkpoint precedes a horizon before every cutoff; an undeclared cutoff ends at the call.
    assert!(family.route(Some(at("2013-01-01T00:00:00Z")), now).is_none());
    // Without a horizon the family embeds with its newest checkpoint.
    assert_eq!(family.route(None, now).unwrap().model(), "embed-live");
}
