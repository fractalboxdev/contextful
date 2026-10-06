//! `read.resolve-entity`: mentions and edge endpoints.

use contextful_core::memory::declare::MemoryDeclarations;
use contextful_core::memory::resolve::{check_edge, resolve, resolve_mention, Entity, Resolution};
use contextful_core::memory::synthesize::CandidateEdge;
use contextful_core::memory::MemoryError;

fn entities() -> Vec<Entity> {
    vec![
        Entity { entity_id: "e-1".into(), name: "Acme".into(), aliases: vec!["Acme Corp".into()] },
        Entity { entity_id: "e-2".into(), name: "Acme Labs".into(), aliases: vec!["acme".into()] },
        Entity { entity_id: "e-3".into(), name: "Globex".into(), aliases: vec!["GX".into()] },
        Entity { entity_id: "e-4".into(), name: "Initech".into(), aliases: vec!["IT".into()] },
        Entity { entity_id: "e-5".into(), name: "Initrode".into(), aliases: vec!["it".into()] },
    ]
}

/// A mention matching two canonical identities with no deterministic key separating them is recorded as an ambiguous skip, raises `MemoryEntityAmbiguous`, and dead-letters its candidate claim.
// spec: read.resolve-entity.ambiguous-mention@7f7ee2ca
#[test]
fn a_mention_two_identities_share_is_ambiguous() {
    let e = entities();
    // The exact canonical name separates `Acme` from the alias `acme`.
    assert_eq!(resolve("Acme", &e), Resolution::One("e-1".into()));
    assert_eq!(resolve("gx", &e), Resolution::One("e-3".into()));
    assert_eq!(resolve("Umbrella", &e), Resolution::Unresolved);
    assert_eq!(resolve("it", &e), Resolution::Ambiguous(vec!["e-4".into(), "e-5".into()]));
    match resolve_mention("it", &e) {
        Err(MemoryError::EntityAmbiguous(why)) => assert!(why.contains("e-4") && why.contains("e-5"), "{why}"),
        other => panic!("{other:?}"),
    }
}

/// A candidate edge whose source or target resolves to no identity is dead-lettered, raising `MemoryEdgeEndpointUnresolved`.
// spec: read.resolve-entity.edge-endpoint@7554e147
#[test]
fn an_edge_with_an_unresolved_endpoint_is_refused() {
    let d = MemoryDeclarations::default();
    let edge = |s: &str, t: &str| CandidateEdge { rel_type: "about".into(), source: s.into(), target: t.into() };
    assert!(check_edge(&edge("Acme", "Globex"), &entities(), &d).is_ok());
    for (s, t, end) in [("Umbrella", "Globex", "source"), ("Acme", "Umbrella", "target")] {
        match check_edge(&edge(s, t), &entities(), &d) {
            Err(MemoryError::EdgeEndpointUnresolved(why)) => assert!(why.contains(end) && why.contains("Umbrella"), "{why}"),
            other => panic!("{other:?}"),
        }
    }
}
