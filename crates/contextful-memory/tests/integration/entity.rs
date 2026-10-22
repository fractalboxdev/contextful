//! The entity upsert.

use super::support::*;
use contextful_context::read::Face;
use contextful_context::Store;
use contextful_core::grant::Action;
use contextful_core::memory::MemoryError;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::lay_out::NodeId;
use contextful_memory::claims::objects;
use contextful_memory::entity::{upsert, EntityInput, Upserted};
use contextful_memory::MemoryFault;
use contextful_policy::enforce::mask::Pepper;
use contextful_policy::enforce::session::Request;
use contextful_policy::verify::AdmittedAuthority;
use serde_json::{Map, Value};

const ENTITIES: &str = "[[table]]\nname = \"memory/entities\"\nshape = \"memory_entities\"\ncolumns = [\"entity_id\", \"kind\", \"name\", \"aliases\"]\n";

fn input(id: &str, name: &str, aliases: &[&str]) -> EntityInput {
    EntityInput { entity_id: id.into(), kind: "org".into(), name: name.into(), aliases: aliases.iter().map(|a| a.to_string()).collect() }
}

fn put(f: &Fixture, who: &AdmittedAuthority, into: &str, entity: EntityInput) -> Result<Upserted, MemoryFault> {
    upsert(&f.face, who, into, entity, &NodeId::parse("memory-a").unwrap(), at("2030-01-11T00:00:00Z"), &super::synthesize::admit)
}

/// A fixture whose manifest replaces the entities block with `entities`.
fn with_entities(entities: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), "research").unwrap();
    let face = Face::open(store, &MANIFEST.replace(ENTITIES, entities), Pepper::resolve(|_| None)).unwrap();
    Fixture::over(dir, face)
}

fn stored(f: &Fixture) -> Vec<Map<String, Value>> {
    let session = f.face.session(&f.writer(), &Request::default(), Bounds::default()).unwrap();
    objects(&f.face.rows(&session, "memory/entities", None).unwrap())
}

fn aliases(row: &Map<String, Value>) -> Vec<String> {
    serde_json::from_str(row["aliases"].as_str().unwrap()).unwrap()
}

#[test]
fn an_upsert_needs_an_entities_table_and_a_write_grant() {
    let f = Fixture::new();
    match put(&f, &f.writer(), "memory/facts", input("e1", "Acme", &[])) {
        Err(MemoryFault::Undeclared(why)) => assert!(why.contains("memory/facts"), "{why}"),
        other => panic!("{other:?}"),
    }
    let reader = f.authority("agent://reader", &[Action::Read], &["memory/*"]);
    assert!(matches!(put(&f, &reader, "memory/entities", input("e1", "Acme", &[])), Err(MemoryFault::Denied(_))));

    let landed = put(&f, &f.writer(), "memory/entities", input("e1", "Acme", &["Acme Corp"])).unwrap();
    assert!(!landed.replaced);
    let again = put(&f, &f.writer(), "memory/entities", input("e1", "Acme", &["ACME CORP", "Acme Ltd"])).unwrap();
    assert!(again.replaced);
    assert_eq!(again.aliases, ["Acme Corp", "Acme Ltd"]);
}

#[test]
fn a_name_another_entity_carries_refuses_the_upsert() {
    let f = Fixture::new();
    put(&f, &f.writer(), "memory/entities", input("e1", "Acme", &["Acme Corp"])).unwrap();
    match put(&f, &f.writer(), "memory/entities", input("e2", " acme corp ", &[])) {
        Err(MemoryFault::Memory(MemoryError::EntityAliasConflict(why))) => assert!(why.contains("e1"), "{why}"),
        other => panic!("{other:?}"),
    }
}

/// An upsert stamps the writing agent beside the subject it acts for.
#[test]
fn an_upserted_row_carries_its_authoring_agent() {
    let f = Fixture::new();
    put(&f, &f.writer(), "memory/entities", input("e1", "Acme", &[])).unwrap();
    let rows = stored(&f);
    assert_eq!(rows[0]["_authored_by"], Value::from("user://dana@acme.example"));
    assert_eq!(rows[0]["_authored_agent"], Value::from("agent://synthesizer"));
}

// spec: read.resolve-entity.upsert-fields@e8caa54c
/// Each field lands trimmed, and a field or alias empty after trimming lands nothing.
#[test]
fn an_upsert_trims_its_fields_and_refuses_an_empty_one() {
    let f = Fixture::new();
    for bad in [input(" ", "Acme", &[]), input("e1", "Acme", &["  "]), EntityInput { kind: "\t".into(), ..input("e1", "Acme", &[]) }] {
        match put(&f, &f.writer(), "memory/entities", bad) {
            Err(MemoryFault::Memory(MemoryError::EntityFieldEmpty(_))) => {}
            other => panic!("{other:?}"),
        }
    }
    assert!(stored(&f).is_empty());
    let landed = put(&f, &f.writer(), "memory/entities", EntityInput { kind: " org ".into(), ..input(" e1 ", " Acme ", &[" Acme Corp "]) }).unwrap();
    assert_eq!((landed.entity_id.as_str(), landed.aliases.as_slice()), ("e1", ["Acme Corp".to_string()].as_slice()));
    let rows = stored(&f);
    assert_eq!((&rows[0]["entity_id"], &rows[0]["kind"], &rows[0]["name"]), (&Value::from("e1"), &Value::from("org"), &Value::from("Acme")));
}

// spec: read.resolve-entity.upsert-whole-table@6c02b2e9
/// A session whose read of the table comes back cut refuses, rather than merging and
/// checking against a partial table.
#[test]
fn an_upsert_over_a_truncated_read_lands_nothing() {
    let f = Fixture::new();
    for (id, name) in [("e1", "Acme"), ("e2", "Globex"), ("e3", "Initech")] {
        put(&f, &f.writer(), "memory/entities", input(id, name, &[])).unwrap();
    }
    let capped = f.authority_capped("agent://synthesizer", &[Action::Read, Action::Write], &["memory/*"], 2);
    match put(&f, &capped, "memory/entities", input("e3", "Initech", &["Initrode"])) {
        Err(MemoryFault::Memory(MemoryError::EntityTableTruncated(why))) => assert!(why.contains("memory/entities"), "{why}"),
        other => panic!("{other:?}"),
    }
}

// spec: read.resolve-entity.upsert-availability@ddf489e5
/// An entities table declaring `available_time` keeps vintages, and an upsert carries no
/// source availability instant, so it refuses.
#[test]
fn an_upsert_into_a_table_declaring_availability_refuses() {
    let f = with_entities(&format!("{ENTITIES}content_hash_column = \"digest\"\n[table.available_time]\ncolumn = \"published_at\"\n"));
    match put(&f, &f.writer(), "memory/entities", input("e1", "Acme", &[])) {
        Err(MemoryFault::Memory(MemoryError::EntityAvailabilityDeclared(why))) => assert!(why.contains("published_at"), "{why}"),
        other => panic!("{other:?}"),
    }
}

/// Only the entities table resolution reads takes an upsert.
#[test]
fn an_upsert_into_a_second_entities_table_refuses() {
    let f = with_entities(&format!("{ENTITIES}\n{}", ENTITIES.replace("memory/entities", "memory/people")));
    match put(&f, &f.writer(), "memory/people", input("e1", "Acme", &[])) {
        Err(MemoryFault::Undeclared(why)) => assert!(why.contains("memory/entities"), "{why}"),
        other => panic!("{other:?}"),
    }
}

// spec: read.resolve-entity.upsert-serial@11ad334e
/// Concurrent upserts on one table apply one after another: no alias is lost, and two
/// identities never both claim one mention.
#[test]
fn concurrent_upserts_serialize_on_the_table() {
    let f = Fixture::new();
    put(&f, &f.writer(), "memory/entities", input("e-acme", "Acme", &[])).unwrap();
    let writer = f.writer();
    std::thread::scope(|s| {
        for n in 0..8 {
            let (f, writer) = (&f, &writer);
            s.spawn(move || put(f, writer, "memory/entities", input("e-acme", "Acme", &[&format!("A{n}")])).unwrap());
        }
    });
    let held = aliases(&stored(&f)[0]);
    assert_eq!(held.len(), 8, "{held:?}");

    let outcomes: Vec<_> = std::thread::scope(|s| {
        let spawned: Vec<_> = ["e1", "e2"].iter().map(|id| {
            let (f, writer) = (&f, &writer);
            s.spawn(move || put(f, writer, "memory/entities", input(id, id, &["Shared"])))
        }).collect();
        spawned.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(outcomes.iter().filter(|o| o.is_ok()).count(), 1, "{outcomes:?}");
    assert!(outcomes.iter().any(|o| matches!(o, Err(MemoryFault::Memory(MemoryError::EntityAliasConflict(_))))), "{outcomes:?}");
}
