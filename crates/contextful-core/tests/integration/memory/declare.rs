//! `read.declare`: shapes, canonical columns and the relation vocabulary.

use super::FACT_COLUMNS;
use contextful_core::memory::declare::{DeclareError, MemoryDeclarations, Shape, RESERVED_RELATIONS};
use contextful_core::memory::resolve::{check_edge, Entity};
use contextful_core::memory::synthesize::CandidateEdge;
use contextful_core::memory::MemoryError;

fn manifest(shape: &str, columns: &str) -> String {
    format!("[[table]]\nname = \"team_memory\"\nshape = \"{shape}\"\ncolumns = {columns}\n\n[relation]\ndeclared = [\"reports_to\", \"owns_service\"]\n")
}

/// A table naming a shape and omitting one of its canonical columns raises `MemoryShapeColumnMissing` when the declaration loads.
// spec: read.declare.canonical-column@b27c4dc5
#[test]
fn a_shaped_table_omitting_a_canonical_column_is_refused() {
    let d = MemoryDeclarations::parse(&manifest("memory_facts", FACT_COLUMNS)).unwrap();
    assert_eq!(d.tables[0].shape, Shape::Facts);
    assert_eq!(d.tables[0].table_decl().primary_key, Some(vec!["claim_id".to_string()]));
    let without = FACT_COLUMNS.replace(", \"grant_id\"", "");
    match MemoryDeclarations::parse(&manifest("memory_facts", &without)) {
        Err(DeclareError::Memory(MemoryError::ShapeColumnMissing(why))) => assert!(why.contains("grant_id"), "{why}"),
        other => panic!("{other:?}"),
    }
    for shape in Shape::ALL {
        let cols = format!("{:?}", shape.canonical_columns());
        assert!(MemoryDeclarations::parse(&manifest(shape.name(), &cols)).is_ok(), "{}", shape.name());
        let short = format!("{:?}", &shape.canonical_columns()[1..]);
        assert!(matches!(
            MemoryDeclarations::parse(&manifest(shape.name(), &short)),
            Err(DeclareError::Memory(MemoryError::ShapeColumnMissing(_)))
        ));
    }
    assert!(matches!(MemoryDeclarations::parse(&manifest("memory_notes", FACT_COLUMNS)), Err(DeclareError::Malformed(_))));
}

/// A candidate edge whose `rel_type` falls outside the union lands in the dead-letter table and raises `MemoryUndeclaredRelation`; no row is written under the unknown type.
// spec: read.declare.undeclared-relation@101bc556
#[test]
fn an_edge_outside_the_relation_union_is_refused() {
    let d = MemoryDeclarations::parse(&manifest("memory_edges", r#"["edge_id", "rel_type", "source_id", "target_id", "evidence"]"#)).unwrap();
    let entities = vec![
        Entity { entity_id: "e-dana".into(), name: "Dana".into(), aliases: vec![] },
        Entity { entity_id: "e-lee".into(), name: "Lee".into(), aliases: vec![] },
    ];
    let edge = |rel: &str| CandidateEdge { rel_type: rel.into(), source: "Dana".into(), target: "Lee".into() };
    assert!(d.admits_relation(RESERVED_RELATIONS[0]));
    assert_eq!(check_edge(&edge("reports_to"), &entities, &d).unwrap().source_id, "e-dana");
    assert_eq!(check_edge(&edge("about"), &entities, &d).unwrap().target_id, "e-lee");
    match check_edge(&edge("mentors"), &entities, &d) {
        Err(MemoryError::UndeclaredRelation(why)) => assert!(why.contains("mentors"), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn reserved_memory_relations_match_the_declared_core() {
    assert_eq!(
        RESERVED_RELATIONS.as_slice(),
        ["supports", "contradicts", "supersedes", "about", "derived_from"].as_slice()
    );
    let declarations = MemoryDeclarations::default();
    for relation in RESERVED_RELATIONS {
        assert!(declarations.admits_relation(relation));
    }
    for relation in ["mentions", "part_of", "located_in", "owns", "works_for"] {
        assert!(!declarations.admits_relation(relation), "{relation} needs a declaration");
    }
}
