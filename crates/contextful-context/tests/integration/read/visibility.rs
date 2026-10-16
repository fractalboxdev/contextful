//! `disclosure.reach` and `disclosure.bound-staleness` through the read face: a bound table's
//! relation admits only rows the reader's reachable set names, and a read past the source's
//! budget refuses.

use super::*;
use contextful_context::visibility::{sweep_grants, Landing};
use contextful_core::disclosure::declare::Binding;
use contextful_core::disclosure::sweep::{GrantLine, SweepBlock, SweepKind};
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
"#;

const PAGES: &str = r#"SELECT page_id FROM "wiki/pages" ORDER BY page_id"#;

struct Wiki {
    reads: Reads,
    now: Arc<Mutex<Instant>>,
    block: SweepBlock,
    bindings: Vec<Binding>,
}

fn grant(resource: &str, principal: &str) -> GrantLine {
    GrantLine { resource_id: resource.into(), principal: principal.into(), principal_kind: "user".into(), level: "viewer".into() }
}

impl Wiki {
    /// The fixture store holding `wiki/pages` p1–p3 and `wiki/notes`, its face reading now
    /// from a clock the test moves.
    fn new() -> Wiki {
        let mut reads = Reads::with_manifest(WIKI);
        land_rows(&reads.store, "wiki/pages", "run-0001", json!([{ "page_id": "p1" }, { "page_id": "p2" }, { "page_id": "p3" }]));
        land_rows(&reads.store, "wiki/notes", "run-0001", json!([{ "note": "n1" }]));
        let now = Arc::new(Mutex::new(at("2030-01-01T00:00:00Z")));
        let clock = now.clone();
        reads.face = Face::open(reads.store.clone(), WIKI, pepper()).unwrap().with_clock(move || *clock.lock().unwrap());
        let block = SweepBlock::parse_manifest(WIKI).unwrap().remove(0);
        let bindings = TableDecl::parse_pipeline(WIKI).unwrap().iter().filter_map(|d| Binding::of(d).unwrap()).collect();
        Wiki { reads, now, block, bindings }
    }

    fn sweep(&self, kind: SweepKind, began: &str, grants: Vec<GrantLine>) {
        let landing = Landing { node: NodeId::parse("ingest-a").unwrap(), site_id: "site-a".into() };
        sweep_grants(&self.reads.store, &landing, &self.block, &self.bindings, kind, grants, at(began), at(began)).unwrap();
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

/// The semi-join opening {{authority.compose.relation-order}} admits a row of a bound table when its `resource_key` value names a resource with a read-conferring, unexpired, untombstoned grant in the reader's reachable set.
// spec: disclosure.reach.semi-join@6f8b5b64
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
    // reach relation, and no resource id.
    let session = w.reads.face.session(&dana, &Request::default(), Bounds::default()).unwrap();
    let sql = session.relation("wiki/pages").unwrap().sql().to_string();
    assert!(sql.contains("\"page_id\"") && sql.contains("__contextful_reach") && !sql.contains("p1"), "{sql}");
    // A table with no visibility block carries no semi-join.
    assert!(!session.relation("wiki/notes").unwrap().sql().contains("__contextful_reach"));

    // The source revokes dana's p2; after the next incremental sweep the same credential's
    // read no longer answers it, though its session was pooled.
    w.sweep(SweepKind::Incremental, "2030-01-01T00:06:00Z", vec![grant("p1", "dana@acme.example"), grant("p3", "erin@acme.example")]);
    w.at("2030-01-01T00:07:00Z");
    assert_eq!(w.pages(&dana).unwrap(), [json!("p1")]);
    assert_eq!(w.pages(&erin).unwrap(), [json!("p3")]);
}

/// A read touching a bound table past its source's budget, or before its first full run,
/// refuses through the face; the face's clock is the instant it reads as now.
#[test]
fn a_read_past_the_budget_refuses_through_the_face() {
    let w = Wiki::new();
    let dana = w.authority("user://dana@acme.example");
    // No completed full run.
    let message = refused_with(w.pages(&dana), "VisibilityAccessStale");
    assert!(message.contains("no completed full run"), "{message}");
    // A table with no visibility block still reads.
    let session = w.reads.face.session(&dana, &Request::default(), Bounds::default()).unwrap();
    assert_eq!(column(&w.reads.query(&session, r#"SELECT note FROM "wiki/notes""#).unwrap(), "note"), [json!("n1")]);

    w.sweep(SweepKind::Full, "2030-01-01T00:00:00Z", vec![grant("p1", "dana@acme.example")]);
    w.at("2030-01-01T00:15:00Z");
    assert_eq!(w.pages(&dana).unwrap(), [json!("p1")]);
    w.at("2030-01-01T00:15:01Z");
    let message = refused_with(w.pages(&dana), "VisibilityAccessStale");
    assert!(message.contains("lag 901s") && message.contains("budget 900s"), "{message}");
    // The next sweep inside the budget clears it.
    w.sweep(SweepKind::Incremental, "2030-01-01T00:16:00Z", vec![grant("p1", "dana@acme.example")]);
    assert_eq!(w.pages(&dana).unwrap(), [json!("p1")]);
}

/// Registering a `federated` table on a read face raises `VisibilityFederatedRegistered`.
// spec: disclosure.declare-fidelity.federated-registered@9998ddc3
#[test]
fn a_federated_table_registers_on_no_read_face() {
    let reads = Reads::new();
    let federated = WIKI.replace("fidelity          = \"mirrored\"", "fidelity          = \"federated\"");
    match Face::open(reads.store.clone(), &federated, pepper()) {
        Err(ReadFault::Refused(r)) => {
            assert_eq!(r.identifier(), "VisibilityFederatedRegistered");
            assert!(r.to_string().contains("wiki/pages"), "{r}");
        }
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("a federated table registered"),
    }
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
