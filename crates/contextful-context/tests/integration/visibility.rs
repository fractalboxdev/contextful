//! `disclosure.mirror`: a sweep lands a source's access tables under `_visibility/<source>/`
//! through the land path.

use crate::support::{at, decl, Fixture};
use contextful_context::visibility::{committed, principals, state, sweep_grants, tombstones, Landing};
use contextful_context::ContextError;
use contextful_core::disclosure::declare::Binding;
use contextful_core::disclosure::sweep::{GrantLine, SweepBlock, SweepKind};
use contextful_core::disclosure::VisibilityError;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::StoreError;
use serde_json::json;

const MANIFEST: &str = "[[acl_sweep]]\nsource = \"wiki\"\nenumerate = \"pages\"\nfull = { every = \"1h\" }\nincremental = { every = \"5m\" }\n";

fn pages() -> contextful_core::store::declare::TableDecl {
    decl("name = \"wiki/pages\"\n[pipeline.tables.visibility]\nsource = \"wiki\"\nresource_key = \"page_id\"\nresource_kind = \"page\"\nfidelity = \"mirrored\"\nfamily = \"item-exception\"\nmax_acl_staleness = \"15m\"")
}

fn grant(resource: &str, principal: &str) -> GrantLine {
    GrantLine { resource_id: resource.into(), principal: principal.into(), principal_kind: "user".into(), level: "viewer".into() }
}

fn landing() -> Landing {
    Landing { node: NodeId::parse("ingest-a").unwrap(), site_id: "site-a".into() }
}

/// A sweep lands `resources`, `grants`, `principals`, `tombstones` and `freshness` as ordinary
/// rows under `_visibility/<source>/`, through the land path and its write lock.
#[test]
fn a_sweep_lands_access_rows_under_the_visibility_namespace() {
    let f = Fixture::new();
    let pages = pages();
    f.land(&pages, "run-0001", json!([{"page_id": "p1", "title": "Roadmap"}, {"page_id": "p2", "title": "Payroll"}]), "2030-01-01T00:00:00Z").unwrap();
    let block = SweepBlock::parse_manifest(MANIFEST).unwrap().remove(0);
    let bindings = vec![Binding::of(&pages).unwrap().unwrap()];

    let full = sweep_grants(&f.store, &landing(), &block, &bindings, SweepKind::Full, vec![grant("p1", "dana"), grant("p2", "dana")], at("2030-01-01T00:10:00Z"), at("2030-01-01T00:10:01Z")).unwrap();
    assert_eq!(full.freshness.epoch, 1);
    assert_eq!(full.freshness.budget_seconds, Some(900));
    let tables = f.store.tables().unwrap();
    for t in ["resources", "grants", "principals", "freshness"] {
        assert!(tables.contains(&format!("_visibility/wiki/{t}")), "{t}: {tables:?}");
    }
    // Ordinary rows: each table scans like any other, its run carrying the sweep id.
    let freshness = f.scan(&decl("name = \"_visibility/wiki/freshness\""), Default::default()).unwrap();
    assert_eq!(freshness.files.len(), 1);
    assert!(freshness.files[0].contains(&full.freshness.sweep_id), "{:?}", freshness.files);

    let held = state(&f.store, "wiki").unwrap();
    assert_eq!(held.resources.len(), 2);
    assert_eq!(held.grants.len(), 2);
    assert!(held.resources.iter().all(|r| r.resource_kind == "page" && r.visibility_class == "mirrored"));
    let fresh = held.freshness.unwrap();
    assert_eq!((fresh.epoch, fresh.watermark_at), (1, Some(at("2030-01-01T00:10:00Z"))));

    // The incremental run withdraws dana's p2: the grant row leaves the current set and a
    // tombstone lands.
    let inc = sweep_grants(&f.store, &landing(), &block, &bindings, SweepKind::Incremental, vec![grant("p1", "dana")], at("2030-01-01T00:15:00Z"), at("2030-01-01T00:15:01Z")).unwrap();
    assert_eq!(inc.freshness.epoch, 2);
    let held = state(&f.store, "wiki").unwrap();
    assert_eq!(held.grants.iter().map(|g| g.resource_id.as_str()).collect::<Vec<_>>(), ["p1"]);
    assert_eq!(held.freshness.as_ref().unwrap().last_incremental_at, Some(at("2030-01-01T00:15:00Z")));
    let revoked = tombstones(&f.store, "wiki").unwrap();
    assert_eq!(revoked.len(), 1);
    assert_eq!((revoked[0].scope, revoked[0].resource_id.as_str(), revoked[0].principal.as_deref()), ("grant", "p2", Some("dana")));
    assert_eq!(principals(&f.store, "wiki").unwrap().len(), 1);

    // A producer landing into the namespace is refused; the sweep is its one writer.
    match f.land(&decl("name = \"_visibility/wiki/grants\""), "run-0002", json!([{"resource_id": "p2", "principal": "mallory"}]), "2030-01-01T00:20:00Z") {
        Err(ContextError::Store(StoreError::StoreReservedTableName(_))) => {}
        other => panic!("{other:?}"),
    }
}

/// An orphan grant refuses at commit and lands no access row.
#[test]
fn an_orphan_grant_lands_nothing() {
    let f = Fixture::new();
    let pages = pages();
    f.land(&pages, "run-0001", json!([{"page_id": "p1"}]), "2030-01-01T00:00:00Z").unwrap();
    let block = SweepBlock::parse_manifest(MANIFEST).unwrap().remove(0);
    let bindings = vec![Binding::of(&pages).unwrap().unwrap()];
    match sweep_grants(&f.store, &landing(), &block, &bindings, SweepKind::Full, vec![grant("p9", "dana")], at("2030-01-01T00:10:00Z"), at("2030-01-01T00:10:01Z")) {
        Err(ContextError::Visibility(VisibilityError::OrphanGrant { .. })) => {}
        other => panic!("{other:?}"),
    }
    let tables = f.store.tables().unwrap();
    assert_eq!(tables, ["wiki/pages"]);
    assert!(tables.iter().all(|t| !t.starts_with("_visibility/")));
    assert!(state(&f.store, "wiki").unwrap().freshness.is_none());
}

/// A run that stops after landing `tombstones` and before `freshness` changes nothing a
/// reader resolves: its grants, principals and tombstones join nothing, and its retry lands
/// the revocation once.
#[test]
fn a_run_stopped_before_freshness_revokes_nothing() {
    let f = Fixture::new();
    let pages = pages();
    f.land(&pages, "run-0001", json!([{"page_id": "p1"}, {"page_id": "p2"}]), "2030-01-01T00:00:00Z").unwrap();
    let block = SweepBlock::parse_manifest(MANIFEST).unwrap().remove(0);
    let bindings = vec![Binding::of(&pages).unwrap().unwrap()];
    let full = sweep_grants(&f.store, &landing(), &block, &bindings, SweepKind::Full, vec![grant("p1", "dana"), grant("p2", "erin")], at("2030-01-01T00:10:00Z"), at("2030-01-01T00:10:01Z")).unwrap();
    let stopped = sweep_grants(&f.store, &landing(), &block, &bindings, SweepKind::Full, vec![grant("p1", "dana")], at("2030-01-01T00:20:00Z"), at("2030-01-01T00:20:01Z")).unwrap();
    assert_eq!(stopped.tombstones.len(), 1);

    // The stopped run's `freshness` never committed.
    let runs = f.table_dir("_visibility/wiki/freshness").join("data/runs").join(&stopped.freshness.sweep_id);
    std::fs::remove_dir_all(&runs).unwrap();
    assert_eq!(committed(&f.store, "wiki").unwrap().into_iter().collect::<Vec<_>>(), [full.freshness.sweep_id.clone()]);

    let held = state(&f.store, "wiki").unwrap();
    assert_eq!(held.freshness.as_ref().map(|f| f.epoch), Some(1));
    assert_eq!(held.grants.iter().map(|g| g.principal.as_str()).collect::<Vec<_>>(), ["dana", "erin"]);
    assert_eq!(principals(&f.store, "wiki").unwrap().iter().map(|p| p.principal.as_str()).collect::<Vec<_>>(), ["dana", "erin"]);
    assert!(tombstones(&f.store, "wiki").unwrap().is_empty());

    // The retry commits, and its one revocation is the only tombstone a reader sees.
    let retry = sweep_grants(&f.store, &landing(), &block, &bindings, SweepKind::Full, vec![grant("p1", "dana")], at("2030-01-01T00:30:00Z"), at("2030-01-01T00:30:01Z")).unwrap();
    assert_eq!(retry.freshness.epoch, 2);
    let revoked = tombstones(&f.store, "wiki").unwrap();
    assert_eq!(revoked.iter().map(|t| (t.resource_id.as_str(), t.principal.as_deref())).collect::<Vec<_>>(), [("p2", Some("erin"))]);
    assert_eq!(state(&f.store, "wiki").unwrap().grants.len(), 1);
}
