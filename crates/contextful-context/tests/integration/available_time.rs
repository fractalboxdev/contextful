//! `store.available-time`: availability-bounded scans, vintages, and how landing resolves
//! each row's instant.

use crate::support::{decl, Fixture};
use contextful_context::ContextError;
use contextful_core::store::bound_time::{Bound, Bounds, Vintage};
use contextful_core::store::reconcile::ColumnType;
use contextful_core::store::StoreError;
use serde_json::json;

const EARNINGS: &str = "name = \"earnings\"\nprimary_key = [\"filing_id\"]\ncontent_hash_column = \"digest\"\n[pipeline.tables.valid_time]\nfrom = \"period_end\"\n[pipeline.tables.available_time]\ncolumn = \"accepted_at\"";
const TYPES: [(&str, ColumnType); 2] = [("period_end", ColumnType::Timestamp), ("accepted_at", ColumnType::Timestamp)];

/// Real GDP for one quarter: an advance estimate, a second estimate, and a re-pull of the
/// second estimate under a later release date.
const GDP: &str = "name = \"gdp\"\nprimary_key = [\"series\"]\ncontent_hash_column = \"digest\"\n[pipeline.tables.valid_time]\nfrom = \"period\"\n[pipeline.tables.available_time]\ncolumn = \"released_at\"";
const GDP_TYPES: [(&str, ColumnType); 2] = [("period", ColumnType::Timestamp), ("released_at", ColumnType::Timestamp)];

fn available(s: &str) -> Bounds {
    Bounds { available_as_of: Some(Bound::parse(s).unwrap()), ..Bounds::default() }
}

fn first(bounds: Bounds) -> Bounds {
    Bounds { vintage: Vintage::First, ..bounds }
}

#[cfg(feature = "read")]
/// `available_as_of` wraps the inner source with `<column> <= available_as_of` over the declared column, conjoined with {{store.bound-time.as-of}} and {{store.bound-time.valid-as-of}} where given.
// spec: store.available-time.available-as-of@318cd14f
#[test]
fn a_row_answers_from_its_availability_instant_onward() {
    use crate::support::s;
    let f = Fixture::new();
    let d = decl(EARNINGS);
    // Period end and acceptance land together, in one run committed long after both.
    let row = json!([{ "filing_id": "acme-2030q1", "eps": "1.23", "digest": "d1", "period_end": "2030-03-31T00:00:00Z", "accepted_at": "2030-05-02T16:05:00Z" }]);
    f.land_typed(&d, "run-1", row, "2030-06-01T00:00:00Z", &TYPES).unwrap();
    let select = "SELECT filing_id FROM t";

    assert!(f.query(&d, available("2030-04-15T00:00:00Z"), select).is_empty());
    assert_eq!(f.query(&d, available("2030-05-02T16:05:00Z"), select), [[s("acme-2030q1")]], "the bound is inclusive");
    assert_eq!(f.query(&d, available("2030-05-03T00:00:00Z"), select), [[s("acme-2030q1")]]);
    // `_ingested_at` plays no part: the row committed on 1 June answers a May bound.
    let echo = f.scan(&d, available("2030-05-03T00:00:00Z")).unwrap().bounds.unwrap();
    assert_eq!(echo, json!({ "available_as_of": "2030-05-03T00:00:00.000000000Z", "inclusive": true }));

    // Conjoined with valid time: the row is valid from period end and public from acceptance.
    let both = |valid: &str, avail: &str| Bounds { valid_as_of: Some(Bound::parse(valid).unwrap()), ..available(avail) };
    assert!(f.query(&d, both("2030-03-01T00:00:00Z", "2030-05-03T00:00:00Z"), select).is_empty());
    assert_eq!(f.query(&d, both("2030-04-01T00:00:00Z", "2030-05-03T00:00:00Z"), select), [[s("acme-2030q1")]]);
}

/// An `available_as_of` read naming a table that declares no `available_time` raises `StoreAvailableTimeUndeclared`, naming the table.
#[test]
fn a_scan_bounded_by_availability_refuses_an_undeclaring_table() {
    let f = Fixture::new();
    let d = decl("name = \"prices\"\nprimary_key = [\"ticker\"]");
    f.land(&d, "run-1", json!([{ "ticker": "ACME", "close": 41.5 }]), "2030-06-01T00:00:00Z").unwrap();
    match f.scan(&d, available("2030-05-03T00:00:00Z")) {
        Err(ContextError::Store(StoreError::StoreAvailableTimeUndeclared(m))) => assert!(m.contains("prices"), "{m}"),
        other => panic!("expected StoreAvailableTimeUndeclared, got {:?}", other.map(|s| s.relation)),
    }
}

/// Landing a row whose resolved availability instant is null raises `StoreAvailableTimeNull`, naming the table and the run.
// spec: store.available-time.null@6f96e557
#[test]
fn landing_a_row_without_an_availability_instant_is_refused() {
    let f = Fixture::new();
    let d = decl(EARNINGS);
    let rows = json!([
        { "filing_id": "acme-2030q1", "digest": "d1", "period_end": "2030-03-31T00:00:00Z", "accepted_at": "2030-05-02T16:05:00Z" },
        { "filing_id": "acme-2030q2", "digest": "d2", "period_end": "2030-06-30T00:00:00Z", "accepted_at": null },
    ]);
    match f.land_typed(&d, "run-7", rows, "2030-08-01T00:00:00Z", &TYPES) {
        Err(ContextError::Store(StoreError::StoreAvailableTimeNull(m))) => assert!(m.contains("earnings") && m.contains("run-7"), "{m}"),
        other => panic!("expected StoreAvailableTimeNull, got {other:?}"),
    }
    // A row omitting the column resolves no instant either; nothing of the run lands.
    let absent = json!([{ "filing_id": "acme-2030q3", "digest": "d3", "period_end": "2030-09-30T00:00:00Z" }]);
    assert!(matches!(f.land_typed(&d, "run-8", absent, "2030-11-01T00:00:00Z", &TYPES), Err(ContextError::Store(StoreError::StoreAvailableTimeNull(_)))));
    assert!(f.store.try_schema("earnings").unwrap().is_none());
}

/// Three releases of one quarter's GDP, landed together after the last.
#[cfg(feature = "read")]
fn gdp() -> (Fixture, contextful_core::store::declare::TableDecl) {
    let f = Fixture::new();
    let d = decl(GDP);
    let rows = json!([
        { "series": "GDPC1", "period": "2030-01-01T00:00:00Z", "value": "100.0", "digest": "a", "released_at": "2030-04-28T12:30:00Z" },
        { "series": "GDPC1", "period": "2030-01-01T00:00:00Z", "value": "101.5", "digest": "b", "released_at": "2030-05-29T12:30:00Z" },
        { "series": "GDPC1", "period": "2030-01-01T00:00:00Z", "value": "101.5", "digest": "b", "released_at": "2030-06-26T12:30:00Z" },
    ]);
    f.land_typed(&d, "run-1", rows, "2030-07-01T00:00:00Z", &GDP_TYPES).unwrap();
    (f, d)
}

#[cfg(feature = "read")]
const RELEASE: &str = "SELECT value, CAST(released_at AS VARCHAR) FROM t";

#[cfg(feature = "read")]
/// A keyed table declaring `available_time` is a vintage table: it keeps one row per primary key, valid-time `from` where declared, and availability instant, so a restatement lands beside the value it revises.
// spec: store.available-time.vintage-rows@ba551e63
#[test]
fn a_restatement_lands_beside_the_value_it_revises() {
    use crate::support::s;
    let (f, d) = gdp();
    // Both releases stay readable: each answers at its own release date.
    assert_eq!(f.query(&d, available("2030-05-01"), "SELECT value FROM t"), [[s("100.0")]]);
    assert_eq!(f.query(&d, available("2030-06-01"), "SELECT value FROM t"), [[s("101.5")]]);
    assert_eq!(f.query(&d, first(Bounds::default()), "SELECT value FROM t"), [[s("100.0")]]);
}

#[cfg(feature = "read")]
/// A read of a vintage table returns, per primary key and valid-time `from`, the vintage with the latest availability instant at or before `available_as_of`, or the latest of all when the read carries no availability bound.
// spec: store.available-time.vintage-resolution@2d836d46
#[test]
fn a_vintage_read_returns_the_latest_release_the_bound_admits() {
    use crate::support::s;
    let (f, d) = gdp();
    assert!(f.query(&d, available("2030-04-28T12:29:59Z"), RELEASE).is_empty());
    assert_eq!(f.query(&d, available("2030-04-28T12:30:00Z"), RELEASE), [[s("100.0"), s("2030-04-28 12:30:00+00")]]);
    assert_eq!(f.query(&d, available("2030-05-30"), RELEASE), [[s("101.5"), s("2030-05-29 12:30:00+00")]]);
    assert_eq!(f.query(&d, Bounds::default(), "SELECT value FROM t"), [[s("101.5")]]);
}

#[cfg(feature = "read")]
/// A read carrying `vintage = "first"` returns, per primary key and valid-time `from`, the vintage with the earliest availability instant, within `available_as_of` where given.
// spec: store.available-time.vintage-first@07270677
#[test]
fn vintage_first_returns_the_initial_release() {
    use crate::support::s;
    let (f, d) = gdp();
    assert_eq!(f.query(&d, first(Bounds::default()), "SELECT value FROM t"), [[s("100.0")]]);
    assert_eq!(f.query(&d, first(available("2030-07-01")), "SELECT value FROM t"), [[s("100.0")]]);
    assert!(f.query(&d, first(available("2030-04-01")), "SELECT value FROM t").is_empty());
}

#[cfg(feature = "read")]
/// A landing row whose `content_hash_column` value equals that of its key's latest vintage lands no new vintage.
// spec: store.available-time.vintage-hash@88f000cf
#[test]
fn an_unchanged_re_pull_lands_no_new_vintage() {
    use crate::support::s;
    let (f, d) = gdp();
    // The 26 June re-pull carries the 29 May digest: the 29 May release still answers after it.
    assert_eq!(f.query(&d, available("2030-06-30"), RELEASE), [[s("101.5"), s("2030-05-29 12:30:00+00")]]);
    assert_eq!(f.query(&d, Bounds::default(), "SELECT count(*) FROM (SELECT 1 FROM t)"), [[s("1")]]);
    // A later release whose digest differs is a restatement.
    let revised = json!([{ "series": "GDPC1", "period": "2030-01-01T00:00:00Z", "value": "101.9", "digest": "c", "released_at": "2030-07-30T12:30:00Z" }]);
    f.land_typed(&d, "run-2", revised, "2030-08-01T00:00:00Z", &GDP_TYPES).unwrap();
    assert_eq!(f.query(&d, Bounds::default(), "SELECT value FROM t"), [[s("101.9")]]);
}

#[cfg(feature = "read")]
/// A fold over a vintage table partitions as {{store.fold.valid-time-line}} or {{store.declare.dedup-view}} prescribes with the availability column added, and drops no vintage.
// spec: store.available-time.vintage-fold-key@6315c6f3
#[test]
fn a_fold_keeps_every_vintage() {
    use crate::support::{at, s};
    let (f, d) = gdp();
    let before: Vec<_> = ["2030-05-01", "2030-06-01", "2030-06-30"].iter().map(|b| f.query(&d, available(b), RELEASE)).collect();
    let initial = f.query(&d, first(Bounds::default()), RELEASE);
    contextful_context::fold::fold(&f.store, &d, at("2030-07-02T00:00:00Z")).unwrap();
    let after: Vec<_> = ["2030-05-01", "2030-06-01", "2030-06-30"].iter().map(|b| f.query(&d, available(b), RELEASE)).collect();
    assert_eq!(after, before);
    assert_eq!(f.query(&d, first(Bounds::default()), RELEASE), initial);
    assert_eq!(initial, [[s("100.0"), s("2030-04-28 12:30:00+00")]]);
}

#[cfg(feature = "read")]
/// A declared `available_time.lag` fills a null column at landing with valid-time `to`, else `from`, plus the duration {{store.available-time.lag-class}} assigns the row, and the engine injects `_available_imputed` true on that row.
// spec: store.available-time.lag-fill@ec377c5a
#[test]
fn a_null_instant_fills_from_valid_time_plus_the_class_lag() {
    use crate::support::s;
    let f = Fixture::new();
    let d = decl(&format!("{EARNINGS}\nlag = {{ class_column = \"form\", classes = {{ \"10-Q\" = \"40d\", \"10-K\" = \"60d\" }} }}"));
    let rows = json!([
        { "filing_id": "q1", "form": "10-Q", "digest": "a", "period_end": "2030-03-31T00:00:00Z", "accepted_at": null },
        { "filing_id": "fy", "form": "10-K", "digest": "b", "period_end": "2030-12-31T00:00:00Z" },
        { "filing_id": "q2", "form": "10-Q", "digest": "c", "period_end": "2030-06-30T00:00:00Z", "accepted_at": "2030-07-20T00:00:00Z", "_available_imputed": true },
    ]);
    f.land_typed(&d, "run-1", rows, "2031-06-01T00:00:00Z", &TYPES).unwrap();
    let got = f.query(&d, Bounds::default(), "SELECT filing_id, CAST(accepted_at AS VARCHAR), _available_imputed FROM t ORDER BY filing_id");
    assert_eq!(
        got,
        [
            [s("fy"), s("2031-03-01 00:00:00+00"), s("true")],
            [s("q1"), s("2030-05-10 00:00:00+00"), s("true")],
            // A sourced instant stays as sent, and a producer cannot set the flag.
            [s("q2"), s("2030-07-20 00:00:00+00"), None],
        ]
    );
}

/// `available_time.lag = { class_column = "<column>", classes = { <class> = "<n>d" } }` maps each row's class to a duration; a row whose class names no entry resolves no instant and refuses as {{store.available-time.null}}.
// spec: store.available-time.lag-class@5d5921ab
#[test]
fn a_row_whose_class_names_no_lag_is_refused() {
    let f = Fixture::new();
    let d = decl(&format!("{EARNINGS}\nlag = {{ class_column = \"form\", classes = {{ \"10-Q\" = \"40d\" }} }}"));
    let rows = json!([{ "filing_id": "x", "form": "8-K", "digest": "a", "period_end": "2030-03-31T00:00:00Z" }]);
    match f.land_typed(&d, "run-3", rows, "2030-06-01T00:00:00Z", &TYPES) {
        Err(ContextError::Store(StoreError::StoreAvailableTimeNull(m))) => assert!(m.contains("earnings") && m.contains("run-3"), "{m}"),
        other => panic!("expected StoreAvailableTimeNull, got {other:?}"),
    }
}

#[cfg(feature = "read")]
/// A date-typed availability value resolves at landing to the start of the following day in the declared `zone`.
// spec: store.available-time.date-end@49ac6e34
#[test]
fn a_date_only_instant_resolves_to_the_next_day_in_its_zone() {
    use crate::support::s;
    let f = Fixture::new();
    let d = decl("name = \"headlines\"\n[pipeline.tables.available_time]\ncolumn = \"published\"\nzone = \"America/New_York\"");
    f.land(&d, "run-1", json!([{ "id": "h1", "published": "2030-05-02" }, { "id": "h2", "published": "2030-05-02T13:00:00Z" }]), "2030-06-01T00:00:00Z").unwrap();
    let got = f.query(&d, Bounds::default(), "SELECT id, CAST(published AS VARCHAR) FROM t ORDER BY id");
    assert_eq!(got, [[s("h1"), s("2030-05-03 04:00:00+00")], [s("h2"), s("2030-05-02 13:00:00+00")]]);
    // A May 2 headline is not public on the evening of May 2 in New York.
    assert_eq!(f.query(&d, available("2030-05-03T03:59:59Z"), "SELECT id FROM t"), [[s("h2")]]);

    let utc = decl("name = \"filings\"\n[pipeline.tables.available_time]\ncolumn = \"filed\"");
    f.land(&utc, "run-1", json!([{ "id": "f1", "filed": "2030-05-02" }]), "2030-06-01T00:00:00Z").unwrap();
    assert_eq!(f.query(&utc, Bounds::default(), "SELECT CAST(filed AS VARCHAR) FROM t"), [[s("2030-05-03 00:00:00+00")]]);
}

#[cfg(feature = "read")]
fn staged(run: &str, at: &str) -> contextful_context::land::RunContext {
    contextful_context::land::RunContext {
        node: contextful_core::store::lay_out::NodeId::parse("ingest-a").unwrap(),
        injection: contextful_core::store::reserve::Injection { run_id: run.into(), site_id: "site-a".into(), batch_seq: None, authored_by: None, taint: None },
        committed_at: crate::support::at(at),
    }
}

#[cfg(feature = "read")]
fn rows(v: serde_json::Value) -> contextful_context::land::Batch {
    contextful_context::land::Batch { rows: v.as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect(), types: Default::default() }
}

#[cfg(feature = "read")]
/// A backfill lands each row's availability column as {{store.available-time.declaration}} resolves it from the source, while `_ingested_at` stays the commit instant {{run.own.stage-instant}} writes.
// spec: run.backfill.available-instant@b9dce9fd
#[test]
fn a_backfill_of_old_history_answers_bounds_at_the_sources_instants() {
    use crate::support::s;
    use contextful_context::land::{commit_parts, stage_part, Position};
    let f = Fixture::new();
    let d = decl("name = \"headlines\"\n[pipeline.tables.available_time]\ncolumn = \"published\"");
    // Two chunks of 2014 history, staged in 2026 and committed on 1 June 2026.
    let ctx = staged("backfill-1", "2026-06-01T00:00:00Z");
    let a = stage_part(&f.store, &d, &rows(json!([{ "id": "h1", "published": "2014-03-02T14:00:00Z" }])), &ctx.node, &ctx.injection, 0, 0).unwrap();
    let b = stage_part(&f.store, &d, &rows(json!([{ "id": "h2", "published": "2014-09-15" }])), &ctx.node, &ctx.injection, 1, 1).unwrap();
    let position = Position { pipeline_id: Some("headlines".into()), ..Position::default() };
    commit_parts(&f.store, &d, &[a.name, b.name], &ctx, &position, &|| Ok(()), &|_| Ok(())).unwrap();

    let got = f.query(&d, Bounds::default(), "SELECT id, CAST(published AS VARCHAR), CAST(_ingested_at AS VARCHAR) FROM t ORDER BY id");
    assert_eq!(got, [[s("h1"), s("2014-03-02 14:00:00+00"), s("2026-06-01 00:00:00+00")], [s("h2"), s("2014-09-16 00:00:00+00"), s("2026-06-01 00:00:00+00")]]);
    assert_eq!(f.query(&d, available("2014-05-01"), "SELECT id FROM t"), [[s("h1")]]);
}

#[cfg(feature = "read")]
/// A seeded row lands its availability instant and `_ingested_at` as {{run.backfill.available-instant}} lands a backfilled row's.
// spec: run.seed.available-instant@05480a7c
#[test]
fn a_seeded_row_lands_its_sources_instant_beside_the_commit_instant() {
    use crate::support::s;
    use contextful_context::land::{commit_parts, stage_part, Position};
    let f = Fixture::new();
    let d = decl("name = \"estimates\"\n[pipeline.tables.available_time]\ncolumn = \"as_of\"\nlive_since = \"2020-01-01T00:00:00Z\"");
    let ctx = staged("seed-1", "2026-06-01T00:00:00Z");
    let part = stage_part(&f.store, &d, &rows(json!([{ "id": "e1", "as_of": "2015-02-01T00:00:00Z" }])), &ctx.node, &ctx.injection, 0, 0).unwrap();
    commit_parts(&f.store, &d, &[part.name], &ctx, &Position { pipeline_id: Some("estimates".into()), ..Position::default() }, &|| Ok(()), &|_| Ok(())).unwrap();
    let got = f.query(&d, available("2015-03-01"), "SELECT CAST(as_of AS VARCHAR), CAST(_ingested_at AS VARCHAR), _available_reconstructed FROM t");
    assert_eq!(got, [[s("2015-02-01 00:00:00+00"), s("2026-06-01 00:00:00+00"), s("true")]]);
}

#[cfg(feature = "read")]
/// `available_time.live_since` names the instant a source began publishing live; the engine injects `_available_reconstructed` true on each row available before it.
// spec: store.available-time.live-since@ecae7219
#[test]
fn a_row_available_before_live_since_is_marked_reconstructed() {
    use crate::support::s;
    let f = Fixture::new();
    let d = decl("name = \"estimates\"\n[pipeline.tables.available_time]\ncolumn = \"as_of\"\nlive_since = \"2030-01-01T00:00:00Z\"");
    let rows = json!([{ "id": "old", "as_of": "2029-06-01T00:00:00Z" }, { "id": "live", "as_of": "2030-01-01T00:00:00Z" }]);
    f.land_typed(&d, "run-1", rows, "2030-06-01T00:00:00Z", &[("as_of", ColumnType::Timestamp)]).unwrap();
    let got = f.query(&d, Bounds::default(), "SELECT id, _available_reconstructed FROM t ORDER BY id");
    assert_eq!(got, [[s("live"), None], [s("old"), s("true")]]);
}
