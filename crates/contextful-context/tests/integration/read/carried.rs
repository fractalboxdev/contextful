//! `run.sweep.carried-state` on the read face: a carried table resolves at its own `as_of`
//! while every other table resolves at the read's.

use super::*;
use contextful_context::read::face::Carried;
use contextful_core::store::bound_time::Bound;
use std::collections::BTreeMap;

#[test]
fn a_carried_table_resolves_at_its_own_as_of() {
    let r = Reads::new();
    // A later origin's commit lands one more note on 1 February.
    let decl = TableDecl::parse_pipeline(MANIFEST).unwrap().into_iter().find(|d| d.name == "research/notes").unwrap();
    let row = json!({ "note_id": "n9", "tenant": "acme", "title": "Written by the preceding origin" }).as_object().cloned().unwrap();
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: "run-0009".into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, taint: None },
        committed_at: at("2030-02-01T00:00:00Z"),
    };
    land(&r.store, &decl, &Batch { rows: vec![row], types: std::collections::HashMap::new() }, &ctx).unwrap();
    let authority = r.authority(loop_subject("agent://research-loop"), vec![read(&["research/*"], None)]);
    let sql = r#"SELECT note_id FROM "research/notes" WHERE note_id = 'n9'"#;

    let open = r.face.session(&authority, &Request::default(), Bounds::default()).unwrap();
    assert_eq!(column(&r.query(&open, sql).unwrap(), "note_id"), [json!("n9")]);

    let carried: Carried = BTreeMap::from([("research/notes".to_string(), Bound::parse("2030-01-15T00:00:00Z").unwrap())]);
    let before = r.face.session_carried(&authority, &Request::default(), Bounds::default(), &carried).unwrap();
    assert!(r.query(&before, sql).unwrap().rows.is_empty(), "the carried table resolves before the later commit");
    let (_, carried_snapshots) = r.face.input(&before, sql, Bounds::default()).unwrap();
    let (_, open_snapshots) = r.face.input(&open, sql, Bounds::default()).unwrap();
    assert_ne!(carried_snapshots["research/notes"], open_snapshots["research/notes"], "the snapshot id names the carried resolution");

    // A table the map does not name reads at the read's own bound.
    let other: Carried = BTreeMap::from([("research/contacts".to_string(), Bound::parse("2030-01-15T00:00:00Z").unwrap())]);
    let unaffected = r.face.session_carried(&authority, &Request::default(), Bounds::default(), &other).unwrap();
    assert_eq!(column(&r.query(&unaffected, sql).unwrap(), "note_id"), [json!("n9")]);
}
