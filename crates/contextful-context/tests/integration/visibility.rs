//! `disclosure.mirror`: a sweep lands a source's access tables under `_visibility/<source>/`
//! through the land path.

use crate::support::{at, decl, Fixture};
use contextful_context::visibility::{committed, group_members, identity_links, principals, reach, state, sweep_grants, sweep_read, sweep_run, tombstones, Landing};
use contextful_context::ContextError;
use contextful_core::disclosure::declare::Binding;
use contextful_core::disclosure::sweep::{parse_directory, parse_read, GrantLine, SweepBlock, SweepKind};
use contextful_core::identify::{LinkMethod, NormalizedSubject, Subject};
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

const DRIVE: &str = "[[acl_sweep]]\nsource = \"drive\"\nenumerate = \"files\"\nfull = { every = \"1h\" }\nincremental = { every = \"5m\" }\n";

fn files() -> contextful_core::store::declare::TableDecl {
    decl("name = \"drive/files\"\n[pipeline.tables.visibility]\nsource = \"drive\"\nresource_key = \"file_id\"\nresource_kind = \"file\"\nfidelity = \"mirrored\"\nfamily = \"item-exception\"\nmax_acl_staleness = \"15m\"")
}

fn drive_grant(resource: &str, principal: &str, kind: &str) -> GrantLine {
    GrantLine { resource_id: resource.into(), principal: principal.into(), principal_kind: kind.into(), level: "reader".into() }
}

fn reader(on_behalf_of: &str) -> NormalizedSubject {
    Subject { on_behalf_of: Some(on_behalf_of.into()), agent: Some("agent://drive-loop".into()), ..Subject::default() }.normalize()
}

/// The identity provider's directory export for the drive source: dana's account `u-1001` by
/// SCIM email, erin's `u-1002` by OIDC subject, and `u-1001` in the finance group.
const DIRECTORY: &str = r#"{"source_principal": "u-1001", "subject": "user://dana@acme.example", "method": "scim_email", "confidence": 0.9}
{"source_principal": "u-1002", "subject": "user://erin@acme.example", "method": "oidc_sub", "confidence": 0.7}
{"group": "g-finance", "member": "u-1001", "member_kind": "user"}"#;

struct Drive {
    f: Fixture,
    block: SweepBlock,
    bindings: Vec<Binding>,
}

fn drive() -> Drive {
    let f = Fixture::new();
    let files = files();
    f.land(&files, "run-0001", json!([{"file_id": "f1", "title": "Budget"}, {"file_id": "f2", "title": "Roadmap"}, {"file_id": "f3", "title": "Offsite"}]), "2030-01-01T00:00:00Z").unwrap();
    Drive { f, block: SweepBlock::parse_manifest(DRIVE).unwrap().remove(0), bindings: vec![Binding::of(&files).unwrap().unwrap()] }
}

impl Drive {
    fn sweep(&self, kind: SweepKind, grants: Vec<GrantLine>, directory: Option<&str>, began: &str) -> contextful_core::disclosure::sweep::Commit {
        let directory = directory.map(|d| parse_directory("drive", d).unwrap());
        sweep_run(&self.f.store, &landing(), &self.block, &self.bindings, kind, grants, directory, at(began), at(began)).unwrap()
    }
}

/// A sweep lands a source's access data as ordinary rows of `_visibility/<source>/resources`, `grants`, `principals`, `group_members`, `identity_links`, `tombstones` and `freshness`, through the land path and its write lock.
// spec: disclosure.mirror.grants-table@2f7e28a8
#[test]
fn a_sweep_lands_all_seven_access_tables_as_ordinary_rows() {
    let d = drive();
    let grants = vec![drive_grant("f1", "g-finance", "group"), drive_grant("f2", "u-1002", "user"), drive_grant("f3", "u-1002", "user")];
    let first = d.sweep(SweepKind::Full, grants, Some(DIRECTORY), "2030-01-01T00:10:00Z");
    d.sweep(SweepKind::Full, vec![drive_grant("f1", "g-finance", "group"), drive_grant("f2", "u-1002", "user")], None, "2030-01-01T00:20:00Z");

    let tables = d.f.store.tables().unwrap();
    for t in ["resources", "grants", "principals", "group_members", "identity_links", "tombstones", "freshness"] {
        let name = format!("_visibility/drive/{t}");
        assert!(tables.contains(&name), "{t}: {tables:?}");
        // Each scans like any other table, its runs named by sweep id.
        let scanned = d.f.scan(&decl(&format!("name = \"{name}\"")), Default::default()).unwrap();
        assert!(scanned.files.iter().any(|f| f.contains("sweep-")), "{t}: {:?}", scanned.files);
    }
    assert!(d.f.scan(&decl("name = \"_visibility/drive/identity_links\""), Default::default()).unwrap().files.iter().any(|f| f.contains(&first.freshness.sweep_id)));

    let links = identity_links(&d.f.store, "drive").unwrap();
    assert_eq!(
        links.iter().map(|l| (l.source_principal.as_str(), l.on_behalf_of.as_str(), l.method, l.confidence)).collect::<Vec<_>>(),
        [("u-1001", "user://dana@acme.example", LinkMethod::ScimEmail, 0.9), ("u-1002", "user://erin@acme.example", LinkMethod::OidcSub, 0.7)]
    );
    let members = group_members(&d.f.store, "drive").unwrap();
    assert_eq!(members.iter().map(|m| (m.group.as_str(), m.member.as_str(), m.member_kind.as_str())).collect::<Vec<_>>(), [("g-finance", "u-1001", "user")]);
    assert_eq!(members[0].observed_at, at("2030-01-01T00:10:00Z"), "a carried membership keeps the instant its read began");
    assert_eq!(tombstones(&d.f.store, "drive").unwrap().len(), 1);

    // A producer landing a link is refused; the sweep is the namespace's one writer.
    match d.f.land(&decl("name = \"_visibility/drive/identity_links\""), "run-0002", json!([{"source_principal": "u-9", "subject": "user://mallory@acme.example"}]), "2030-01-01T00:30:00Z") {
        Err(ContextError::Store(StoreError::StoreReservedTableName(_))) => {}
        other => panic!("{other:?}"),
    }
}

/// Directory provisioning from the identity provider writes source-account links. A value a source's profile interface returns at request time is data, not a link.
// spec: authority.identify.directory-link@c4344cda
#[test]
fn links_come_from_the_directory_read_alone() {
    let d = drive();
    // A grants read names principals and writes no link. Dana reaches f1 through the `user`
    // principal her subject spells, which needs no link, and not f2 through the unlinked `u-1001`.
    d.sweep(SweepKind::Full, vec![drive_grant("f1", "dana@acme.example", "user"), drive_grant("f2", "u-1001", "user")], None, "2030-01-01T00:10:00Z");
    assert_eq!(principals(&d.f.store, "drive").unwrap().len(), 2);
    assert!(identity_links(&d.f.store, "drive").unwrap().is_empty());
    assert_eq!(reach(&d.f.store, "drive", &reader("user://dana@acme.example")).unwrap().into_iter().collect::<Vec<_>>(), ["f1"]);

    // The directory read writes the links, and a later run given none keeps them.
    d.sweep(SweepKind::Full, vec![drive_grant("f2", "u-1001", "user")], Some(DIRECTORY), "2030-01-01T00:20:00Z");
    d.sweep(SweepKind::Incremental, vec![drive_grant("f2", "u-1001", "user")], None, "2030-01-01T00:25:00Z");
    assert_eq!(identity_links(&d.f.store, "drive").unwrap().len(), 2);
    assert_eq!(reach(&d.f.store, "drive", &reader("user://dana@acme.example")).unwrap().into_iter().collect::<Vec<_>>(), ["f2"]);

    // A directory read without dana's account withdraws her link and her reach with it.
    d.sweep(SweepKind::Incremental, vec![drive_grant("f2", "u-1001", "user")], Some(r#"{"source_principal": "u-1002", "subject": "user://erin@acme.example", "method": "oidc_sub", "confidence": 0.7}"#), "2030-01-01T00:30:00Z");
    assert!(reach(&d.f.store, "drive", &reader("user://dana@acme.example")).unwrap().is_empty());
}

/// A subject whose source principal resolves to no link reads from that source only the resources {{disclosure.reach.public-grant}} opens.
// spec: authority.identify.unlinked-principal@98585b08
#[test]
fn a_subject_with_no_link_reaches_only_the_public_resources_of_the_source() {
    let d = drive();
    let grants = vec![drive_grant("f1", "g-finance", "group"), drive_grant("f2", "u-1002", "user"), drive_grant("f3", "u-1003", "user")];
    d.sweep(SweepKind::Full, grants, Some(DIRECTORY), "2030-01-01T00:10:00Z");
    assert_eq!(reach(&d.f.store, "drive", &reader("user://dana@acme.example")).unwrap().into_iter().collect::<Vec<_>>(), ["f1"]);
    assert_eq!(reach(&d.f.store, "drive", &reader("user://erin@acme.example")).unwrap().into_iter().collect::<Vec<_>>(), ["f2"]);
    // `u-1003` holds a grant on f3, and no link maps it onto fay: she reaches nothing.
    assert!(reach(&d.f.store, "drive", &reader("user://fay@acme.example")).unwrap().is_empty());
    // A grant to the `public` principal opens f3 to her, and nothing else.
    let opened = vec![drive_grant("f1", "g-finance", "group"), drive_grant("f2", "u-1002", "user"), drive_grant("f3", "u-1003", "user"), drive_grant("f3", "anyone", "public")];
    d.sweep(SweepKind::Full, opened, None, "2030-01-01T00:20:00Z");
    assert_eq!(reach(&d.f.store, "drive", &reader("user://fay@acme.example")).unwrap().into_iter().collect::<Vec<_>>(), ["f3"]);
    // A source with no committed run reaches nothing either.
    assert!(reach(&d.f.store, "wiki", &reader("user://dana@acme.example")).unwrap().is_empty());
}

/// The run directories `table` holds.
fn run_dirs(f: &Fixture, table: &str) -> Vec<String> {
    let dir = f.table_dir(table).join("data/runs");
    let mut out: Vec<String> = std::fs::read_dir(&dir).map(|d| d.map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect()).unwrap_or_default();
    out.sort();
    out
}

/// Before it lands, a run collects every run of its source's `resources`, `grants`, `principals`, `group_members` and `identity_links` but the current one, so each of those tables holds at most two runs.
// spec: disclosure.sweep.superseded-run@74a31809
#[test]
fn a_run_collects_every_superseded_run_of_the_current_state_tables() {
    let d = drive();
    let mut sweeps = Vec::new();
    for (n, began) in ["2030-01-01T00:10:00Z", "2030-01-01T00:20:00Z", "2030-01-01T00:30:00Z", "2030-01-01T00:40:00Z"].into_iter().enumerate() {
        let directory = (n == 0).then_some(DIRECTORY);
        let c = d.sweep(SweepKind::Full, vec![drive_grant("f1", "g-finance", "group"), drive_grant("f2", "u-1002", "user")], directory, began);
        sweeps.push(c.freshness.sweep_id);
    }
    let (previous, current) = (&sweeps[2], &sweeps[3]);
    for t in ["resources", "grants", "principals", "group_members", "identity_links"] {
        assert_eq!(run_dirs(&d.f, &format!("_visibility/drive/{t}")), [previous.clone(), current.clone()].into_iter().collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>(), "{t}");
    }
    // Tombstones and freshness accumulate across committed runs.
    assert_eq!(run_dirs(&d.f, "_visibility/drive/freshness").len(), 4);

    // The current rows read whole after collection, the carried directory rows among them.
    let held = state(&d.f.store, "drive").unwrap();
    assert_eq!(held.freshness.as_ref().map(|f| f.epoch), Some(4));
    assert_eq!(held.grants.len(), 2);
    assert_eq!(held.resources.len(), 3);
    assert_eq!(identity_links(&d.f.store, "drive").unwrap().len(), 2);
    assert_eq!(group_members(&d.f.store, "drive").unwrap().len(), 1);
    assert_eq!(reach(&d.f.store, "drive", &reader("user://dana@acme.example")).unwrap().into_iter().collect::<Vec<_>>(), ["f1"]);
}

/// A first committed run collects the runs that stopped before `freshness` ahead of it.
#[test]
fn a_first_committed_run_collects_every_stopped_run_before_it() {
    let f = Fixture::new();
    let pages = pages();
    f.land(&pages, "run-0001", json!([{"page_id": "p1"}]), "2030-01-01T00:00:00Z").unwrap();
    let block = SweepBlock::parse_manifest(MANIFEST).unwrap().remove(0);
    let bindings = vec![Binding::of(&pages).unwrap().unwrap()];
    // Two first runs land their rows and stop before `freshness`.
    for began in ["2030-01-01T00:10:00Z", "2030-01-01T00:20:00Z"] {
        let stopped = sweep_grants(&f.store, &landing(), &block, &bindings, SweepKind::Full, vec![grant("p1", "dana")], at(began), at(began)).unwrap();
        std::fs::remove_dir_all(f.table_dir("_visibility/wiki/freshness").join("data/runs").join(&stopped.freshness.sweep_id)).unwrap();
        assert!(state(&f.store, "wiki").unwrap().freshness.is_none());
    }
    // The second stopped run collected the first.
    assert_eq!(run_dirs(&f, "_visibility/wiki/resources").len(), 1);
    let first = sweep_grants(&f.store, &landing(), &block, &bindings, SweepKind::Full, vec![grant("p1", "dana")], at("2030-01-01T00:30:00Z"), at("2030-01-01T00:30:01Z")).unwrap();
    for t in ["resources", "grants", "principals"] {
        assert_eq!(run_dirs(&f, &format!("_visibility/wiki/{t}")), std::slice::from_ref(&first.freshness.sweep_id), "{t}");
    }
    assert_eq!(state(&f.store, "wiki").unwrap().grants.len(), 1);
}

/// A webhook run whose file names no resource lands nothing, and `watermark_at` stays where it stood.
// spec: disclosure.sweep.empty-delivery@1dba1906
#[test]
fn an_empty_webhook_delivery_lands_nothing_and_holds_the_watermark() {
    let f = Fixture::new();
    let pages = pages();
    f.land(&pages, "run-0001", json!([{"page_id": "p1"}]), "2030-01-01T00:00:00Z").unwrap();
    let manifest = MANIFEST.replace("[[acl_sweep]]\n", "[[acl_sweep]]\nevents = { stream = \"wiki.permissions\", gap_detectable = true }\n");
    let block = SweepBlock::parse_manifest(&manifest).unwrap().remove(0);
    let bindings = vec![Binding::of(&pages).unwrap().unwrap()];
    let full = sweep_grants(&f.store, &landing(), &block, &bindings, SweepKind::Full, vec![grant("p1", "dana")], at("2030-01-01T00:10:00Z"), at("2030-01-01T00:10:01Z")).unwrap();
    match sweep_read(&f.store, &landing(), &block, &bindings, SweepKind::Webhook, parse_read("").unwrap(), None, at("2030-01-01T00:20:00Z"), at("2030-01-01T00:20:01Z")) {
        Err(ContextError::Invalid(m)) => assert!(m.contains("wiki") && m.contains("names no resource"), "{m}"),
        other => panic!("{other:?}"),
    }
    let held = state(&f.store, "wiki").unwrap().freshness.unwrap();
    assert_eq!((held.sweep_id, held.epoch, held.watermark_at), (full.freshness.sweep_id, 1, Some(at("2030-01-01T00:10:00Z"))));
    // A delivery naming p1 bare applies and moves the watermark.
    let hook = sweep_read(&f.store, &landing(), &block, &bindings, SweepKind::Webhook, parse_read("{\"resource_id\":\"p1\"}").unwrap(), None, at("2030-01-01T00:30:00Z"), at("2030-01-01T00:30:01Z")).unwrap();
    assert_eq!(hook.freshness.watermark_at, Some(at("2030-01-01T00:30:00Z")));
    assert!(state(&f.store, "wiki").unwrap().grants.is_empty());
}
