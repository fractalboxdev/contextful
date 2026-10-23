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

/// The `predictions` shape carries one column per registration field, `deadline_at` resolved for a horizon; the `outcomes` shape carries one column per observation field. Both key on `prediction_id`.
// spec: read.declare.settlement-shapes@6a955d1b
#[test]
fn predictions_and_outcomes_are_shapes_keyed_on_the_prediction() {
    let predictions = ["prediction_id", "subject", "predicted_at", "horizon_secs", "deadline_at", "resolution_source", "comparator", "statement", "metric"];
    let outcomes = ["prediction_id", "observed_at", "verdict", "resolution_source", "signal", "settling_citation", "metric"];
    for (name, columns) in [("predictions", &predictions[..]), ("outcomes", &outcomes[..])] {
        let d = MemoryDeclarations::parse(&manifest(name, &format!("{columns:?}"))).unwrap();
        assert_eq!(d.tables[0].shape.name(), name);
        assert_eq!(d.tables[0].shape.canonical_columns(), columns);
        assert_eq!(d.tables[0].table_decl().primary_key, Some(vec!["prediction_id".to_string()]));
    }
    for column in ["predicted_at", "deadline_at", "resolution_source"] {
        assert!(contextful_core::memory::settle::LABEL_VIEW.contains(&format!("p.{column}")), "{column}");
    }
    assert!(contextful_core::memory::settle::LABEL_VIEW.contains("o.observed_at"));
}

/// For a `rel_type` outside the allowed union, synthesis writes the candidate edge to the dead-letter table and raises
/// `MemoryUndeclaredRelation`. No row uses the unknown type.
// spec: read.declare.undeclared-relation@c778fa66
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

#[test]
fn shaped_memory_tables_preserve_the_declared_availability() {
    for shape in Shape::ALL {
        let text = format!("[[table]]\nname = \"team_memory\"\nshape = \"{}\"\ncolumns = {:?}\ncontent_hash_column = \"digest\"\n[table.available_time]\ncolumn = \"published_at\"\nzone = \"UTC\"\n", shape.name(), shape.canonical_columns());
        let declarations = MemoryDeclarations::parse(&text).unwrap();
        let table = declarations.tables[0].table_decl();
        let available = table.available_time.unwrap();
        assert_eq!(available.column, "published_at");
        assert_eq!(available.zone.as_deref(), Some("UTC"));
        assert_eq!(table.primary_key, Some(vec![shape.key().to_string()]));
        assert_eq!(table.content_hash_column.as_deref(), Some("digest"));
    }
    let plain = MemoryDeclarations::parse(&manifest("memory_facts", FACT_COLUMNS)).unwrap();
    assert!(plain.tables[0].table_decl().available_time.is_none());
    let malformed = format!("{}\n[table.available_time]\ncolumn = 7\n", manifest("memory_facts", FACT_COLUMNS));
    assert!(matches!(MemoryDeclarations::parse(&malformed), Err(DeclareError::Malformed(_))));
}
