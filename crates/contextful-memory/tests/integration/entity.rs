//! The entity upsert.

use super::support::*;
use contextful_core::grant::Action;
use contextful_core::memory::MemoryError;
use contextful_core::store::lay_out::NodeId;
use contextful_memory::entity::{upsert, EntityInput};
use contextful_memory::MemoryFault;

fn input(id: &str, name: &str, aliases: &[&str]) -> EntityInput {
    EntityInput { entity_id: id.into(), kind: "org".into(), name: name.into(), aliases: aliases.iter().map(|a| a.to_string()).collect() }
}

#[test]
fn an_upsert_needs_an_entities_table_and_a_write_grant() {
    let f = Fixture::new();
    let node = NodeId::parse("memory-a").unwrap();
    let now = at("2030-01-11T00:00:00Z");
    match upsert(&f.face, &f.writer(), "memory/facts", input("e1", "Acme", &[]), &node, now, &super::synthesize::admit) {
        Err(MemoryFault::Undeclared(why)) => assert!(why.contains("memory/facts"), "{why}"),
        other => panic!("{other:?}"),
    }
    let reader = f.authority("agent://reader", &[Action::Read], &["memory/*"]);
    assert!(matches!(upsert(&f.face, &reader, "memory/entities", input("e1", "Acme", &[]), &node, now, &super::synthesize::admit), Err(MemoryFault::Denied(_))));

    let landed = upsert(&f.face, &f.writer(), "memory/entities", input("e1", "Acme", &["Acme Corp"]), &node, now, &super::synthesize::admit).unwrap();
    assert!(!landed.replaced);
    let again = upsert(&f.face, &f.writer(), "memory/entities", input("e1", "Acme", &["ACME CORP", "Acme Ltd"]), &node, at("2030-01-12T00:00:00Z"), &super::synthesize::admit).unwrap();
    assert!(again.replaced);
    assert_eq!(again.aliases, ["Acme Corp", "Acme Ltd"]);
}

#[test]
fn a_name_another_entity_carries_refuses_the_upsert() {
    let f = Fixture::new();
    let node = NodeId::parse("memory-a").unwrap();
    upsert(&f.face, &f.writer(), "memory/entities", input("e1", "Acme", &["Acme Corp"]), &node, at("2030-01-11T00:00:00Z"), &super::synthesize::admit).unwrap();
    match upsert(&f.face, &f.writer(), "memory/entities", input("e2", " acme corp ", &[]), &node, at("2030-01-12T00:00:00Z"), &super::synthesize::admit) {
        Err(MemoryFault::Memory(MemoryError::EntityAliasConflict(why))) => assert!(why.contains("e1"), "{why}"),
        other => panic!("{other:?}"),
    }
}
