//! `read.resolve-pin`: a table pinned to a published build reads that build's committed
//! files, and every response touching a published model echoes the build it read.

use super::{at, column, land_rows, loop_subject, read, refused_with, Reads, MANIFEST};
use contextful_context::build::{build, BuildRequest, Built};
use contextful_context::read::ReadOptions;
use contextful_core::pipeline::declare::{collect, ManifestFile};
use contextful_core::pipeline::model::{collect_models, ModelSpec};
use contextful_core::read::pin::{compare, Pins};
use contextful_core::store::bound_time::{Bound, Bounds};
use contextful_policy::enforce::session::{Request, Session};
use contextful_policy::verify::AdmittedAuthority;
use serde_json::{json, Value};

const EVENTS: &str = "lab/events";
const DAILY: &str = "lab/daily";

const MODELS: &str = r#"
[[model]]
id = "lab/daily"
sql = "SELECT day, CAST(count(*) AS BIGINT) AS n FROM \"lab/events\" GROUP BY day"
unique_key = ["day"]

[model.contract]
version = "1.0.0"
columns = [{ name = "day", type = "utf8", nullable = false }, { name = "n", type = "int64", nullable = false }]

[[model]]
id = "lab/constant"
sql = "SELECT 'x' AS id"

[model.contract]
version = "1.0.0"
columns = [{ name = "id", type = "utf8", nullable = false }]
"#;

const COUNTS: &str = r#"SELECT day, n FROM "lab/daily" ORDER BY day"#;

/// A store whose `lab/events` table feeds the published model `lab/daily`, and a reader of `lab/*`.
struct Pinned {
    r: Reads,
    authority: AdmittedAuthority,
}

impl Pinned {
    fn new() -> Pinned {
        let r = Reads::with_manifest(&format!("{MANIFEST}{MODELS}"));
        land_rows(&r.store, EVENTS, "run-0001", json!([{ "day": "d1" }, { "day": "d1" }, { "day": "d2" }]));
        let authority = r.authority(loop_subject("agent://research-loop"), vec![read(&["lab/*"], None)]);
        Pinned { r, authority }
    }

    fn model(id: &str) -> ModelSpec {
        let files = [ManifestFile { path: "contextful.toml".into(), text: MODELS.to_string() }];
        collect_models(&files, &collect(&files).unwrap()).unwrap().into_iter().find(|m| m.spec.id == id).unwrap().spec
    }

    fn build(&self, id: &str, now: &str) -> Built {
        let spec = Pinned::model(id);
        build(&self.r.face, &BuildRequest { model: &spec, site_id: "site-a", started_at: at(now), completed_at: at(now) }).unwrap()
    }

    fn session(&self, bounds: Bounds, pins: &Pins) -> Session {
        self.r.face.session_pinned(&self.authority, &Request::default(), bounds, pins).unwrap()
    }

    fn query(&self, session: &Session, sql: &str) -> Value {
        self.r.face.query(session, sql, ReadOptions::default()).unwrap().to_json()
    }

    fn counts(&self, pins: &Pins) -> Value {
        let s = self.session(Bounds::default(), pins);
        self.query(&s, COUNTS)["rows"].clone()
    }
}

fn resolved_build(response: &Value, table: &str) -> String {
    response["contextful.resolved"][table]["build_id"].as_str().unwrap_or_else(|| panic!("no resolved {table}: {response}")).to_string()
}

/// Every read tool admits `pin`, mapping a table name to a build identifier, and resolves that table to the files its build's committed manifest names. An unnamed table resolves to the latest published state.
// spec: read.resolve-pin.pin-parameter@e1c27d2d
#[test]
fn a_pinned_table_reads_its_build_and_an_unnamed_one_the_latest() {
    let p = Pinned::new();
    let first = p.build(DAILY, "2030-01-11T00:00:00Z");
    land_rows(&p.r.store, EVENTS, "run-0002", json!([{ "day": "d2" }, { "day": "d3" }]));
    let second = p.build(DAILY, "2030-01-11T01:00:00Z");

    let pinned = Pins::default().with(DAILY, &first.build_id);
    assert_eq!(p.counts(&pinned), json!([["d1", "2"], ["d2", "1"]]));
    assert_eq!(p.counts(&Pins::default()), json!([["d1", "2"], ["d2", "2"], ["d3", "1"]]));

    // The listing names the pinned build's committed part, and no part of the later build.
    let s = p.session(Bounds::default(), &pinned);
    let files = p.r.face.files(&s, Bounds::default()).unwrap();
    let paths: Vec<String> = column(&files, "path").iter().map(|v| v.as_str().unwrap().to_string()).collect();
    let daily: Vec<&String> = paths.iter().filter(|f| f.starts_with("tables/lab/daily/")).collect();
    assert_eq!(daily.len(), 1, "{paths:?}");
    assert!(daily[0].contains(&format!("/data/snapshots/{}/", first.build_id)), "{paths:?}");
    assert!(!paths.iter().any(|f| f.contains(&second.build_id)), "{paths:?}");
}

/// A table `pin` maps to `null` resolves as an unnamed table, and a pin map differing from another only by such entries is the same map.
// spec: read.resolve-pin.null-pin@1fbd8509
#[test]
fn a_null_pin_reads_the_latest_build_under_the_unpinned_key() {
    let p = Pinned::new();
    p.build(DAILY, "2030-01-11T00:00:00Z");
    land_rows(&p.r.store, EVENTS, "run-0002", json!([{ "day": "d3" }]));
    let latest = p.build(DAILY, "2030-01-11T01:00:00Z");

    let nulled = Pins::parse(Some(&json!({ DAILY: null }))).unwrap();
    assert_eq!(nulled, Pins::default());
    let s = p.session(Bounds::default(), &Pins::default());
    let unpinned = p.query(&s, COUNTS);
    let misses = p.r.face.pool().counts().session_misses;
    let s = p.session(Bounds::default(), &nulled);
    let answered = p.query(&s, COUNTS);
    assert_eq!(answered["rows"], unpinned["rows"]);
    assert_eq!(resolved_build(&answered, DAILY), latest.build_id);
    assert_eq!(p.r.face.pool().counts().session_misses, misses, "a null pin reuses the unpinned session");

    assert!(Pins::parse(Some(&json!({ DAILY: 7 }))).is_err());
    assert!(Pins::parse(Some(&json!([DAILY]))).is_err());
}

/// An unknown or collected build identifier raises `PinnedBuildUnavailable`, naming the oldest identifier still pinnable. A pin never widens to the latest state.
// spec: read.resolve-pin.unknown-build@dca75dfd
#[test]
fn an_unknown_or_collected_build_is_refused_naming_the_oldest_pinnable() {
    let p = Pinned::new();
    let first = p.build(DAILY, "2030-01-11T00:00:00Z");
    let second = p.build(DAILY, "2030-01-11T01:00:00Z");
    let session = |pins: &Pins| p.r.face.session_pinned(&p.authority, &Request::default(), Bounds::default(), pins);

    let garbage = refused_with(session(&Pins::default().with(DAILY, "snapshot-garbage")), "PinnedBuildUnavailable");
    assert!(garbage.contains(&format!("the oldest pinnable build is `{}`", first.build_id)), "{garbage}");

    // Past the retention window, the next build's collection takes the first build; its
    // parent, the second, stays on disk and is the oldest still pinnable.
    let last = p.build(DAILY, "2030-01-20T00:00:00Z");
    let collected = refused_with(session(&Pins::default().with(DAILY, &first.build_id)), "PinnedBuildUnavailable");
    assert!(collected.contains(&format!("build `{}` is unknown or collected", first.build_id)), "{collected}");
    assert!(collected.contains(&format!("the oldest pinnable build is `{}`", second.build_id)), "{collected}");

    // A landed table publishes no build to pin.
    refused_with(session(&Pins::default().with(EVENTS, &last.build_id)), "PinnedBuildUnavailable");
}

/// A pin and the store's transaction-time bound are upper bounds on one clock; a table named by both resolves to the earlier.
// spec: read.resolve-pin.earlier-bound-wins@fdefcb2a
#[test]
fn a_pin_and_as_of_resolve_to_the_earlier_bound() {
    let p = Pinned::new();
    let first = p.build(DAILY, "2030-01-11T00:00:00Z");
    land_rows(&p.r.store, EVENTS, "run-0002", json!([{ "day": "d3" }]));
    let second = p.build(DAILY, "2030-01-11T02:00:00Z");
    let as_of = |s: &str| Bounds { as_of: Some(Bound::parse(s).unwrap()), valid_as_of: None };

    // The bound precedes the pinned build: the bound wins.
    let s = p.session(as_of("2030-01-11T01:00:00Z"), &Pins::default().with(DAILY, &second.build_id));
    let earlier_bound = p.query(&s, COUNTS);
    assert_eq!(earlier_bound["rows"], json!([["d1", "2"], ["d2", "1"]]));
    assert_eq!(resolved_build(&earlier_bound, DAILY), first.build_id);

    // The pinned build precedes the bound: the pin wins.
    let s = p.session(as_of("2030-01-12T00:00:00Z"), &Pins::default().with(DAILY, &first.build_id));
    let earlier_pin = p.query(&s, COUNTS);
    assert_eq!(earlier_pin["rows"], json!([["d1", "2"], ["d2", "1"]]));
    assert_eq!(resolved_build(&earlier_pin, DAILY), first.build_id);

    let s = p.session(as_of("2030-01-12T00:00:00Z"), &Pins::default());
    assert_eq!(resolved_build(&p.query(&s, COUNTS), DAILY), second.build_id);
}

/// A response touching a published model carries `contextful.resolved`, mapping each such table to `{build_id, watermark}`, pinned or not and zero rows included; the watermark names, per input table, the snapshot id and the committed runs it omits.
// spec: read.resolve-pin.resolved-echo@fae2e0bc
#[test]
fn every_response_touching_a_published_model_echoes_its_build() {
    let p = Pinned::new();
    let first = p.build(DAILY, "2030-01-11T00:00:00Z");
    land_rows(&p.r.store, EVENTS, "run-0002", json!([{ "day": "d3" }]));
    let second = p.build(DAILY, "2030-01-11T01:00:00Z");
    let s = p.session(Bounds::default(), &Pins::default());

    let none = p.query(&s, r#"SELECT day FROM "lab/daily" WHERE n < 0"#);
    assert_eq!(none["rows"], json!([]));
    let entry = &none["contextful.resolved"][DAILY];
    assert_eq!(entry["build_id"], json!(second.build_id), "{none}");
    let frontier = &entry["watermark"]["inputs"][EVENTS];
    assert_eq!(frontier, &serde_json::to_value(&second.watermark.inputs[EVENTS]).unwrap(), "{none}");
    assert_eq!(frontier["runs"].as_array().unwrap().len(), 2, "{none}");

    // A pinned read echoes the pinned build; a read touching no model carries no block.
    let s = p.session(Bounds::default(), &Pins::default().with(DAILY, &first.build_id));
    assert_eq!(resolved_build(&p.query(&s, COUNTS), DAILY), first.build_id);
    let joined = p.query(&s, r#"SELECT e.day FROM "lab/events" e JOIN "lab/daily" d ON e.day = d.day"#);
    assert_eq!(joined["contextful.resolved"].as_object().unwrap().len(), 1, "{joined}");
    let landed = p.query(&s, r#"SELECT day FROM "lab/events""#);
    assert!(landed.get("contextful.resolved").is_none(), "{landed}");

    // A description reads the build, and says which.
    let described = p.r.face.describe(&s, Some(DAILY), Bounds::default()).unwrap();
    assert_eq!(resolved_build(&described, DAILY), first.build_id);
}

/// The watermark is null for a materialization carrying none, distinct from a watermark of zero.
// spec: read.resolve-pin.absent-watermark@b1fd0803
#[test]
fn a_build_over_no_input_echoes_a_null_watermark() {
    let p = Pinned::new();
    let constant = p.build("lab/constant", "2030-01-11T00:00:00Z");
    p.build(DAILY, "2030-01-11T00:00:00Z");
    let s = p.session(Bounds::default(), &Pins::default());
    let both = p.query(&s, r#"SELECT c.id, d.day FROM "lab/constant" c, "lab/daily" d"#);
    let resolved = &both["contextful.resolved"];
    assert_eq!(resolved["lab/constant"], json!({ "build_id": constant.build_id, "watermark": null }), "{both}");
    assert!(resolved[DAILY]["watermark"].is_object(), "{both}");
}

/// A consumer compares resolved build identifiers across every query of one derivation and fails the derivation where two differ.
// spec: read.resolve-pin.consumer-comparison@5f8e99d9
#[test]
fn a_derivation_spanning_two_builds_fails_and_a_pinned_one_holds() {
    let p = Pinned::new();
    let first = p.build(DAILY, "2030-01-11T00:00:00Z");
    let s = p.session(Bounds::default(), &Pins::default());
    let before = p.query(&s, COUNTS);
    land_rows(&p.r.store, EVENTS, "run-0002", json!([{ "day": "d3" }]));
    let second = p.build(DAILY, "2030-01-11T01:00:00Z");

    let s = p.session(Bounds::default(), &Pins::default());
    let after = p.query(&s, r#"SELECT count(*) AS days FROM "lab/daily""#);
    let stitched = compare([&before, &after]).unwrap_err();
    assert_eq!((stitched.model.as_str(), stitched.first.as_str(), stitched.second.as_str()), (DAILY, first.build_id.as_str(), second.build_id.as_str()));

    let pinned = Pins::default().with(DAILY, &resolved_build(&before, DAILY));
    let s = p.session(Bounds::default(), &pinned);
    let held = p.query(&s, r#"SELECT count(*) AS days FROM "lab/daily""#);
    assert_eq!(held["rows"], json!([["2"]]));
    assert_eq!(compare([&before, &held]), Ok(()));
}
