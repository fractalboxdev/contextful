//! Ownership answers over memory edges, read through the caller's session.

use super::support::*;
use contextful_context::land::{land, Batch, RunContext};
use contextful_context::read::Face;
use contextful_context::Store;
use contextful_core::grant::Action;
use contextful_core::store::bound_time::{Bound, Bounds};
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reserve::Injection;
use contextful_memory::owners::{owners, Owner};
use contextful_memory::MemoryFault;
use contextful_policy::enforce::mask::Pepper;
use contextful_policy::enforce::session::Request;
use contextful_policy::verify::AdmittedAuthority;
use serde_json::json;
use std::collections::HashMap;

const EDGES: &str = "memory/edges";

/// Attach `principal` to `artifact` under `rel_type`, committed at `when`.
fn attach(f: &Fixture, run: &str, rel_type: &str, artifact: &str, principal: &str, when: &str) {
    let row = json!({ "edge_id": run, "rel_type": rel_type, "source_id": artifact, "target_id": principal, "evidence": "[]" });
    let ctx = RunContext {
        node: NodeId::parse("memory-a").unwrap(),
        injection: Injection { run_id: run.into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, taint: None },
        committed_at: at(when),
    };
    let decl = TableDecl { primary_key: Some(vec!["edge_id".into()]), ..TableDecl::named(EDGES) };
    land(f.face.store(), &decl, &Batch { rows: vec![row.as_object().unwrap().clone()], types: HashMap::new() }, &ctx).unwrap();
}

fn fixture() -> Fixture {
    let manifest = format!("{MANIFEST}\n[[table]]\nname = \"{EDGES}\"\nshape = \"memory_edges\"\ncolumns = [\"edge_id\", \"rel_type\", \"source_id\", \"target_id\", \"evidence\"]\n");
    let dir = tempfile::tempdir().unwrap();
    let face = Face::open(Store::open(dir.path(), "research").unwrap(), &manifest, Pepper::resolve(|_| None)).unwrap();
    let f = Fixture::over(dir, face);
    attach(&f, "e1", "owned_by", "doc://plan", "user://bea", "2030-01-01T00:00:00Z");
    attach(&f, "e2", "owned_by", "doc://plan", "user://cal", "2030-01-02T00:00:00Z");
    attach(&f, "e3", "owned_by", "doc://plan", "user://abe", "2030-01-02T00:00:00Z");
    attach(&f, "e4", "about", "doc://plan", "user://dee", "2030-01-03T00:00:00Z");
    attach(&f, "e5", "owned_by", "doc://other", "user://eve", "2030-01-03T00:00:00Z");
    f
}

fn answer(f: &Fixture, who: &AdmittedAuthority, bounds: Bounds) -> Result<Vec<String>, MemoryFault> {
    let session = f.face.session(who, &Request::default(), bounds).unwrap();
    Ok(owners(&f.face, &session, EDGES, "doc://plan")?.into_iter().map(|o: Owner| o.principal).collect())
}

fn reader(f: &Fixture) -> AdmittedAuthority {
    f.authority("agent://research-loop", &[Action::Read], &["research/*", "memory/*"])
}

/// An artifact's ownership answer returns the target of every `owned_by` edge from it visible to the caller, newest `_ingested_at` first, with principal id breaking equal-instant ties.
// spec: read.resolve-entity.ownership-answer@cef62f8d
#[test]
fn an_ownership_answer_returns_every_attached_principal_newest_first() {
    let f = fixture();
    let owned = owners(&f.face, &f.face.session(&reader(&f), &Request::default(), Bounds::default()).unwrap(), EDGES, "doc://plan").unwrap();
    let principals: Vec<&str> = owned.iter().map(|o| o.principal.as_str()).collect();
    assert_eq!(principals, ["user://abe", "user://cal", "user://bea"]);
    assert_eq!(owned[0].attached_at, at("2030-01-02T00:00:00Z"));
    assert_eq!(owned[2].attached_at, at("2030-01-01T00:00:00Z"));
}

/// An external graph engine is a derived index of admitted memory edges; an entity or ownership answer resolves through the store's enforced rows at the read frontier.
// spec: read.resolve-entity.graph-index@41df09f1
#[test]
fn an_ownership_answer_reads_enforced_rows_at_the_frontier() {
    let f = fixture();
    let before = Bounds { as_of: Some(Bound { at: at("2030-01-01T12:00:00Z"), inclusive: true }), valid_as_of: None };
    assert_eq!(answer(&f, &reader(&f), before).unwrap(), ["user://bea"], "an attachment committed past the frontier is absent");
    let outsider = f.authority("agent://outsider", &[Action::Read], &["research/*"]);
    assert!(answer(&f, &outsider, Bounds::default()).is_err(), "a caller the edges table is not granted to reads no owner");
    let claims = owners(&f.face, &f.face.session(&reader(&f), &Request::default(), Bounds::default()).unwrap(), "memory/facts", "doc://plan");
    assert!(matches!(claims, Err(MemoryFault::Undeclared(_))), "{claims:?}");
}
