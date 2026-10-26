//! `disclosure.reach` and `disclosure.bound-staleness` through the read face: a bound table's
//! relation admits only rows the reader's reachable set names, and a read past the source's
//! budget refuses.

use super::*;
use contextful_context::visibility::{sweep_read, Landing};
use crate::support::sweep_run;
use contextful_core::disclosure::declare::Binding;
use contextful_core::disclosure::sweep::{Directory, GrantLine, GrantsRead, Membership, SweepBlock, SweepKind};
use std::sync::{Arc, Mutex};

const WIKI: &str = r#"
[[pipeline.tables]]
name = "wiki/pages"

[pipeline.tables.visibility]
source            = "wiki"
resource_key      = "page_id"
resource_kind     = "page"
fidelity          = "mirrored"
family            = "item-exception"
max_acl_staleness = "15m"

[[pipeline.tables]]
name = "wiki/notes"

[[acl_sweep]]
source      = "wiki"
enumerate   = "pages"
full        = { every = "1h" }
incremental = { every = "5m" }

[pack.wiki]
tables = ["wiki/pages"]

[pack.wiki.access."wiki/pages"]
resource_key   = "page_id"
resource_kind  = "page"
family         = "item-exception"
level          = { viewer = "read", editor = "write", lister = "list" }
"#;

const PAGES: &str = r#"SELECT page_id FROM "wiki/pages" ORDER BY page_id"#;

struct Wiki {
    reads: Reads,
    now: Arc<Mutex<Instant>>,
    block: SweepBlock,
    bindings: Vec<Binding>,
}

fn grant(resource: &str, principal: &str) -> GrantLine {
    graded(resource, principal, "user", "viewer")
}

fn graded(resource: &str, principal: &str, kind: &str, level: &str) -> GrantLine {
    GrantLine { resource_id: resource.into(), principal: principal.into(), principal_kind: kind.into(), level: level.into() }
}

impl Wiki {
    /// The fixture store holding `wiki/pages` p1–p3 and `wiki/notes`, its face reading now
    /// from a clock the test moves.
    fn new() -> Wiki {
        Wiki::declared(WIKI)
    }

    /// [`Wiki::new`] under `manifest`.
    fn declared(manifest: &str) -> Wiki {
        Wiki::opened(manifest, None)
    }

    /// [`Wiki::declared`], its face holding a result cache of `cache` bytes when given.
    fn opened(manifest: &str, cache: Option<u64>) -> Wiki {
        let mut reads = Reads::with_manifest(manifest);
        land_rows(&reads.store, "wiki/pages", "run-0001", json!([{ "page_id": "p1" }, { "page_id": "p2" }, { "page_id": "p3" }]));
        land_rows(&reads.store, "wiki/notes", "run-0001", json!([{ "note": "n1" }]));
        let now = Arc::new(Mutex::new(at("2030-01-01T00:00:00Z")));
        let clock = now.clone();
        let face = Face::open(reads.store.clone(), manifest, pepper()).unwrap().with_clock(move || *clock.lock().unwrap());
        reads.face = match cache {
            Some(budget) => face.with_result_cache(budget),
            None => face,
        };
        let block = SweepBlock::parse_manifest(manifest).unwrap().remove(0);
        let bindings = TableDecl::parse_pipeline(manifest).unwrap().iter().filter_map(|d| Binding::of(d).unwrap()).collect();
        Wiki { reads, now, block, bindings }
    }

    /// An incremental run reading only `read`, every other governed resource unread.
    fn sweep_only(&self, began: &str, read: &[&str], grants: Vec<GrantLine>) {
        let landing = Landing { node: NodeId::parse("ingest-a").unwrap(), site_id: "site-a".into() };
        let unread = ["p1", "p2", "p3"].iter().filter(|r| !read.contains(r)).map(|r| r.to_string()).collect();
        let grants = GrantsRead { grants, unread, ..GrantsRead::default() };
        sweep_read(&self.reads.store, &landing, &self.block, &self.bindings, SweepKind::Incremental, grants, None, at(began), at(began)).unwrap();
    }

    fn page_read(&self, authority: &AdmittedAuthority) -> Result<Response, ReadFault> {
        let session = self.reads.face.session(authority, &Request::default(), Bounds::default())?;
        self.reads.query(&session, PAGES)
    }

    fn sweep(&self, kind: SweepKind, began: &str, grants: Vec<GrantLine>) {
        let landing = Landing { node: NodeId::parse("ingest-a").unwrap(), site_id: "site-a".into() };
        sweep_run(&self.reads.store, &landing, &self.block, &self.bindings, kind, grants, None, at(began), at(began)).unwrap();
    }

    fn sweep_directory(&self, kind: SweepKind, began: &str, grants: Vec<GrantLine>, directory: Directory) {
        let landing = Landing { node: NodeId::parse("ingest-a").unwrap(), site_id: "site-a".into() };
        sweep_run(&self.reads.store, &landing, &self.block, &self.bindings, kind, grants, Some(directory), at(began), at(began)).unwrap();
    }

    fn at(&self, now: &str) {
        *self.now.lock().unwrap() = at(now);
    }

    fn authority(&self, on_behalf_of: &str) -> AdmittedAuthority {
        let subject = Subject { on_behalf_of: Some(on_behalf_of.into()), ..loop_subject("agent://wiki-loop") };
        self.reads.authority(subject, vec![read(&["wiki/*"], None)])
    }

    fn pages(&self, authority: &AdmittedAuthority) -> Result<Vec<Value>, ReadFault> {
        let session = self.reads.face.session(authority, &Request::default(), Bounds::default())?;
        Ok(column(&self.reads.query(&session, PAGES)?, "page_id"))
    }
}

/// A bound table's relation admits a row only when its `resource_key` value names a resource
/// a current grant in the reader's reachable set names, and a sweep's revocation reaches a
/// pooled session.
#[test]
fn a_bound_table_admits_only_rows_the_reachable_set_names() {
    let w = Wiki::new();
    w.sweep(SweepKind::Full, "2030-01-01T00:00:00Z", vec![grant("p1", "dana@acme.example"), grant("p2", "dana@acme.example"), grant("p3", "erin@acme.example")]);
    w.at("2030-01-01T00:05:00Z");
    let dana = w.authority("user://dana@acme.example");
    let erin = w.authority("user://erin@acme.example");
    assert_eq!(w.pages(&dana).unwrap(), [json!("p1"), json!("p2")]);
    assert_eq!(w.pages(&erin).unwrap(), [json!("p3")]);
    assert!(w.pages(&w.authority("user://kim@acme.example")).unwrap().is_empty());

    // The ids reach the engine as parameters: the relation text names the column and the
    // reach relation, and no resource id as a string literal. The text also carries the
    // store's temporary path, whose random name can hold `p1` unquoted.
    let session = w.reads.face.session(&dana, &Request::default(), Bounds::default()).unwrap();
    let sql = session.relation("wiki/pages").unwrap().sql().to_string();
    assert!(sql.contains("\"page_id\"") && sql.contains("__contextful_reach") && !sql.contains("'p1'"), "{sql}");
    // A table with no visibility block carries no semi-join.
    assert!(!session.relation("wiki/notes").unwrap().sql().contains("__contextful_reach"));

    // The source revokes dana's p2; after the next incremental sweep the same credential's
    // read no longer answers it, though its session was pooled.
    w.sweep(SweepKind::Incremental, "2030-01-01T00:06:00Z", vec![grant("p1", "dana@acme.example"), grant("p3", "erin@acme.example")]);
    w.at("2030-01-01T00:07:00Z");
    assert_eq!(w.pages(&dana).unwrap(), [json!("p1")]);
    assert_eq!(w.pages(&erin).unwrap(), [json!("p3")]);
}

/// The wire payload of the refusal a read returned.
fn wire<T: std::fmt::Debug>(r: Result<T, ReadFault>) -> Value {
    match r {
        Err(ReadFault::Refused(refusal)) => contextful_policy::enforce::refuse::payload(&refusal),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// A read raises `VisibilityAccessStale` (HTTP 503, carrying source, lag, budget and last observation) when a touched table's source lag exceeds its budget, the source has no completed full run, or the public-status watermark is past budget.
// spec: disclosure.bound-staleness.access-stale@7ef1054e
#[test]
fn a_read_past_the_budget_refuses_through_the_face() {
    let w = Wiki::new();
    let dana = w.authority("user://dana@acme.example");
    // No completed full run.
    let message = refused_with(w.pages(&dana), "VisibilityAccessStale");
    assert!(message.contains("no completed full run"), "{message}");
    let payload = wire(w.pages(&dana));
    assert_eq!((payload["error"]["http"].clone(), payload["error"]["code"].clone()), (json!(503), json!("visibility_access_stale")));
    // A table with no visibility block still reads.
    let session = w.reads.face.session(&dana, &Request::default(), Bounds::default()).unwrap();
    assert_eq!(column(&w.reads.query(&session, r#"SELECT note FROM "wiki/notes""#).unwrap(), "note"), [json!("n1")]);

    w.sweep(SweepKind::Full, "2030-01-01T00:00:00Z", vec![grant("p1", "dana@acme.example")]);
    w.at("2030-01-01T00:15:00Z");
    assert_eq!(w.pages(&dana).unwrap(), [json!("p1")]);
    w.at("2030-01-01T00:15:01Z");
    let message = refused_with(w.pages(&dana), "VisibilityAccessStale");
    for part in ["`wiki`", "lag 901s", "budget 900s", "last observation 2030-01-01T00:00:00Z"] {
        assert!(message.contains(part), "{part}: {message}");
    }
    assert_eq!(wire(w.pages(&dana))["error"]["http"], json!(503));
    // The next sweep inside the budget clears it.
    w.sweep(SweepKind::Incremental, "2030-01-01T00:16:00Z", vec![grant("p1", "dana@acme.example")]);
    assert_eq!(w.pages(&dana).unwrap(), [json!("p1")]);
}

/// Registering a `federated` table on a read face raises `VisibilityFederatedRegistered`.
// spec: disclosure.declare-fidelity.federated-registered@9998ddc3
#[test]
fn a_federated_table_registers_on_no_read_face() {
    let mut reads = Reads::new();
    let federated = WIKI.replace("fidelity          = \"mirrored\"", "fidelity          = \"federated\"");
    land_rows(&reads.store, "wiki/notes", "run-0001", json!([{ "note": "n1" }]));
    // Declaring a federated table leaves the face open: every other table still reads.
    reads.face = Face::open(reads.store.clone(), &federated, pepper()).unwrap();
    assert!(!reads.face.tables().unwrap().contains(&"wiki/pages".to_string()));
    let subject = Subject { on_behalf_of: Some("user://dana@acme.example".into()), ..loop_subject("agent://wiki-loop") };
    let authority = reads.authority(subject, vec![read(&["wiki/*"], None)]);
    let session = reads.face.session(&authority, &Request::default(), Bounds::default()).unwrap();
    assert!(session.relation("wiki/pages").is_none());
    assert_eq!(column(&reads.query(&session, r#"SELECT note FROM "wiki/notes""#).unwrap(), "note"), [json!("n1")]);
    // Naming the federated table refuses with the federated refusal.
    let message = refused_with(reads.face.rows(&session, "wiki/pages", None), "VisibilityFederatedRegistered");
    assert!(message.contains("wiki/pages"), "{message}");
}

/// A whole-table read behind derive sources and memory synthesis refuses past the source's
/// budget, as a statement does.
#[test]
fn a_whole_table_read_past_the_budget_refuses() {
    let w = Wiki::new();
    let dana = w.authority("user://dana@acme.example");
    w.sweep(SweepKind::Full, "2030-01-01T00:00:00Z", vec![grant("p1", "dana@acme.example")]);
    w.at("2030-01-01T00:05:00Z");
    let session = w.reads.face.session(&dana, &Request::default(), Bounds::default()).unwrap();
    assert!(w.reads.face.rows(&session, "wiki/pages", None).is_ok());
    w.at("2030-01-01T00:15:01Z");
    let session = w.reads.face.session(&dana, &Request::default(), Bounds::default()).unwrap();
    refused_with(w.reads.face.rows(&session, "wiki/pages", None), "VisibilityAccessStale");
    refused_with(w.reads.face.source_rows(&session, "wiki/pages", None, &["page_id"]), "VisibilityAccessStale");
    // A table with no visibility block still reads whole.
    assert!(w.reads.face.rows(&session, "wiki/notes", None).is_ok());
}

/// A closure reaching its depth or node bound raises `VisibilityClosureDepth` with the non-retryable HTTP status 422 and returns no set.
///
/// The refusal reaches only a read touching the bound table; the session opens and every
/// other table reads.
// spec: disclosure.reach.closure-overflow@0e6f4120
#[test]
fn a_closure_overflow_refuses_only_reads_of_its_table() {
    let w = Wiki::new();
    let mut members = vec![Membership { group: "g0".into(), member: "dana@acme.example".into(), member_kind: "user".into() }];
    for i in 1..=10 {
        members.push(Membership { group: format!("g{i}"), member: format!("g{}", i - 1), member_kind: "group".into() });
    }
    w.sweep_directory(SweepKind::Full, "2030-01-01T00:00:00Z", vec![grant("p1", "erin@acme.example")], Directory { links: Vec::new(), members });
    w.at("2030-01-01T00:05:00Z");
    let dana = w.authority("user://dana@acme.example");
    let session = w.reads.face.session(&dana, &Request::default(), Bounds::default()).unwrap();
    assert_eq!(column(&w.reads.query(&session, r#"SELECT note FROM "wiki/notes""#).unwrap(), "note"), [json!("n1")]);
    let message = refused_with(w.pages(&dana), "VisibilityClosureDepth");
    assert!(message.contains("8 hops"), "{message}");
    refused_with(w.reads.face.rows(&session, "wiki/pages", None), "VisibilityClosureDepth");
    let payload = wire(w.pages(&dana));
    assert_eq!((payload["error"]["http"].clone(), payload["error"]["code"].clone()), (json!(422), json!("visibility_closure_depth")));
    // A reader whose closure stays inside the bound reads the table.
    assert_eq!(w.pages(&w.authority("user://erin@acme.example")).unwrap(), [json!("p1")]);

    // A closure past 10000 nodes refuses alike, though one hop deep.
    let wide = (0..10_000).map(|i| Membership { group: format!("w{i}"), member: "erin@acme.example".into(), member_kind: "user".into() }).collect();
    w.sweep_directory(SweepKind::Full, "2030-01-01T00:06:00Z", vec![grant("p1", "erin@acme.example")], Directory { links: Vec::new(), members: wide });
    let erin = w.authority("user://erin@acme.example");
    let message = refused_with(w.pages(&erin), "VisibilityClosureDepth");
    assert!(message.contains("10000 nodes"), "{message}");
    assert_eq!(wire(w.pages(&erin))["error"]["http"], json!(422));
}

/// A sub-agent's reachable set equals or narrows its parent's; no agent reads beyond the principal it acts for.
// spec: authority.identify.agent-ceiling@5f5932d3
#[test]
fn every_agent_acting_for_a_principal_reaches_that_principal_set_and_no_more() {
    let w = Wiki::new();
    w.sweep(SweepKind::Full, "2030-01-01T00:00:00Z", vec![grant("p1", "dana@acme.example"), grant("p3", "erin@acme.example"), grant("p2", "agent://wiki-helper")]);
    w.at("2030-01-01T00:05:00Z");
    let acting = |agent: &str| {
        let subject = Subject { on_behalf_of: Some("user://dana@acme.example".into()), ..loop_subject(agent) };
        w.reads.authority(subject, vec![read(&["wiki/*"], None)])
    };
    let parent = w.pages(&acting("agent://wiki-loop")).unwrap();
    assert_eq!(parent, [json!("p1")]);
    // A helper agent acting for dana reaches dana's set: a grant naming the agent itself
    // confers nothing, and erin's page stays out.
    assert_eq!(w.pages(&acting("agent://wiki-helper")).unwrap(), parent);
    // A narrower credential for the same principal reads a subset.
    let narrow = w.reads.authority(Subject { on_behalf_of: Some("user://dana@acme.example".into()), ..loop_subject("agent://wiki-helper") }, vec![read(&["wiki/notes"], None)]);
    let session = w.reads.face.session(&narrow, &Request::default(), Bounds::default()).unwrap();
    assert!(session.relation("wiki/pages").is_none());
}

/// The semi-join admits a resource through a grant whose level the table's pack mapping
/// projects onto read, write or admin, and through a grant to the `public` principal.
#[test]
fn a_bound_table_admits_rows_by_mapped_level_and_public_grant() {
    let w = Wiki::new();
    w.sweep(
        SweepKind::Full,
        "2030-01-01T00:00:00Z",
        vec![graded("p1", "dana@acme.example", "user", "editor"), graded("p2", "dana@acme.example", "user", "lister"), graded("p3", "anyone", "public", "viewer")],
    );
    w.at("2030-01-01T00:05:00Z");
    // dana's `lister` grant on p2 confers no read; the public p3 reads for everyone.
    assert_eq!(w.pages(&w.authority("user://dana@acme.example")).unwrap(), [json!("p1"), json!("p3")]);
    assert_eq!(w.pages(&w.authority("user://kim@acme.example")).unwrap(), [json!("p3")]);

    // With no pack mapping the table, a level reads verbatim: `viewer` confers nothing,
    // `read` does.
    let unmapped = WIKI.split("[pack.wiki]").next().unwrap().to_string();
    let w = Wiki::declared(&unmapped);
    w.sweep(SweepKind::Full, "2030-01-01T00:00:00Z", vec![graded("p1", "dana@acme.example", "user", "viewer"), graded("p2", "dana@acme.example", "user", "read")]);
    w.at("2030-01-01T00:05:00Z");
    assert_eq!(w.pages(&w.authority("user://dana@acme.example")).unwrap(), [json!("p2")]);
}

/// A read touching a table served under {{disclosure.bound-staleness.public-only}} answers with a `contextful.visibility` block holding `degraded` true and the narrowed tables under `narrowed`.
// spec: disclosure.bound-staleness.degraded-block@19246301
#[test]
fn a_public_only_table_past_its_budget_serves_its_public_rows_marked_degraded() {
    let manifest = WIKI
        .replace("max_acl_staleness = \"15m\"", "max_acl_staleness = \"15m\"\non_stale          = \"public_only\"")
        .replace("name = \"wiki/pages\"\n", "name = \"wiki/pages\"\nresult_cache = \"10m\"\n");
    let w = Wiki::opened(&manifest, Some(1 << 20));
    let grants = || vec![graded("p1", "anyone", "public", "viewer"), grant("p2", "dana@acme.example"), grant("p3", "erin@acme.example")];
    w.sweep(SweepKind::Full, "2030-01-01T00:00:00Z", grants());
    // An incremental run re-observes the public p1 alone; the watermark holds at midnight.
    w.sweep_only("2030-01-01T00:12:00Z", &["p1"], vec![graded("p1", "anyone", "public", "viewer")]);
    let dana = w.authority("user://dana@acme.example");
    w.at("2030-01-01T00:13:00Z");
    let whole = w.page_read(&dana).unwrap();
    assert_eq!(column(&whole, "page_id"), [json!("p1"), json!("p2")]);
    assert!(!whole.blocks.contains_key("contextful.visibility"), "{:?}", whole.blocks);
    let cache = w.reads.face.results().unwrap();
    let before = cache.counts();
    assert_eq!(before.entries, 1);

    // Past the budget, with p1 observed 8 minutes ago and no run since the whole read, the
    // read narrows to the public rows through a fresh session, and the result cache holds
    // none of it.
    w.at("2030-01-01T00:20:00Z");
    let narrowed = w.page_read(&dana).unwrap();
    assert_eq!(column(&narrowed, "page_id"), [json!("p1")]);
    assert_eq!(narrowed.blocks.get("contextful.visibility"), Some(&json!({ "degraded": true, "narrowed": ["wiki/pages"] })));
    let after = cache.counts();
    assert_eq!((after.bypassed, after.entries, after.hits), (before.bypassed + 1, before.entries, before.hits));
    // A read touching no narrowed table carries no block.
    let session = w.reads.face.session(&dana, &Request::default(), Bounds::default()).unwrap();
    let notes = w.reads.query(&session, r#"SELECT note FROM "wiki/notes""#).unwrap();
    assert!(!notes.blocks.contains_key("contextful.visibility"));
    // A whole-table read narrows alike.
    let rows = w.reads.face.rows(&session, "wiki/pages", None).unwrap();
    assert_eq!(column(&rows, "page_id"), [json!("p1")]);
    assert_eq!(rows.blocks.get("contextful.visibility"), Some(&json!({ "degraded": true, "narrowed": ["wiki/pages"] })));

    // Once the public-status watermark passes the budget, the read refuses.
    w.at("2030-01-01T00:27:01Z");
    let message = refused_with(w.page_read(&dana), "VisibilityAccessStale");
    assert!(message.contains("public-status watermark"), "{message}");
    // A sweep inside the budget restores the whole read, unmarked.
    w.sweep(SweepKind::Incremental, "2030-01-01T00:27:30Z", grants());
    let restored = w.page_read(&dana).unwrap();
    assert_eq!(column(&restored, "page_id"), [json!("p1"), json!("p2")]);
    assert!(!restored.blocks.contains_key("contextful.visibility"));
}
