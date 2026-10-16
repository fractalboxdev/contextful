//! The knowledge card through the face: one reference resolved against a `memory_entities`
//! table, with the claims recall serves about the identity it names.

use super::*;
use contextful_context::read::explain::ExplainRequest;
use contextful_core::store::reconcile::ColumnType;

const MEMORY: &str = "[[table]]\nname = \"memory/entities\"\nshape = \"memory_entities\"\ncolumns = [\"entity_id\", \"kind\", \"name\", \"aliases\"]\n\n[[table]]\nname = \"memory/facts\"\nshape = \"memory_facts\"\ncolumns = [\"claim_id\", \"subject\", \"predicate\", \"object\", \"scope\", \"tier\", \"confidence\", \"valid_from\", \"valid_to\", \"evidence\", \"superseded_by\", \"grant_id\", \"agent\"]\n";

/// A store holding three identities, two sharing the alias `GX`, and one claim about
/// `e-acme` citing a landed source row.
fn fixture() -> Reads {
    let r = Reads::with_manifest(&format!("{MANIFEST}\n{MEMORY}"));
    land_rows(
        &r.store,
        "memory/entities",
        "entities-1",
        json!([
            { "entity_id": "e-acme", "kind": "company", "name": "Acme Holdings", "aliases": "[\"ACME\",\"アクメ\"]" },
            { "entity_id": "e-globex", "kind": "company", "name": "Globex", "aliases": "[\"GX\"]" },
            { "entity_id": "e-gx", "kind": "fund", "name": "GX Partners", "aliases": "[\"gx\"]" },
        ]),
    );
    land_rows(&r.store, "research/sources", "src-1", json!([{ "source_id": "s0" }]));
    let evidence = serde_json::to_string(&json!([{ "table": "research/sources", "run": "src-1", "seq": 0 }])).unwrap();
    let row = json!({ "claim_id": "c-1", "subject": "e-acme", "predicate": "headquartered_in", "object": "Osaka", "scope": null,
        "tier": "derived", "confidence": 0.8, "valid_from": "2030-01-05T00:00:00Z", "valid_to": null, "evidence": evidence,
        "superseded_by": null, "grant_id": "g", "agent": null });
    let types = [("valid_from", ColumnType::Timestamp), ("valid_to", ColumnType::Timestamp), ("confidence", ColumnType::Float64),
        ("scope", ColumnType::Utf8), ("superseded_by", ColumnType::Utf8), ("agent", ColumnType::Utf8)]
        .iter()
        .map(|(c, t)| (c.to_string(), t.clone()))
        .collect();
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: "memory-1".into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, taint: None },
        committed_at: at("2030-01-10T00:00:00Z"),
    };
    let decl = TableDecl { primary_key: Some(vec!["claim_id".into()]), ..TableDecl::named("memory/facts") };
    land(&r.store, &decl, &Batch { rows: vec![row.as_object().unwrap().clone()], types }, &ctx).unwrap();
    r
}

fn card(r: &Reads, session: &Session, reference: &str) -> Value {
    let request = ExplainRequest { claims: Some("memory/facts".into()), ..ExplainRequest::new("memory/entities", reference, at("2030-06-01T00:00:00Z")) };
    r.face.explain(session, &request).unwrap()
}

/// `memory.explain` takes `table`, `reference` and an optional `claims` table, resolves `reference` against the names and aliases of `table`, and returns that identity's `entity_id`, `kind`, `name` and `aliases`.
// spec: read.resolve-entity.knowledge-card@c823b26f
#[test]
fn an_alias_in_another_script_resolves_to_one_card() {
    let r = fixture();
    let s = r.session(&["memory/*", "research/*"], None, None);
    let got = card(&r, &s, "アクメ");
    assert_eq!(got["entity"], json!({ "entity_id": "e-acme", "kind": "company", "name": "Acme Holdings", "aliases": ["ACME", "アクメ"] }), "{got}");
    assert_eq!(got["candidates"], json!([]));
    assert_eq!(card(&r, &s, "acme")["entity"]["entity_id"], json!("e-acme"));
}

/// A card naming `claims` carries the claims {{read.recall.keyed}} returns for subject `entity_id` at the call instant, under {{read.recall.keyed-gate}}.
// spec: read.resolve-entity.card-claims@17ad044e
#[test]
fn a_card_carries_the_live_claims_about_its_identity() {
    let r = fixture();
    let s = r.session(&["memory/*", "research/*"], None, None);
    let got = card(&r, &s, "Acme Holdings");
    let claims = &got["claims"];
    let at = |c: &str| claims["columns"].as_array().unwrap().iter().position(|x| x == c).unwrap();
    assert_eq!(claims["rows"].as_array().unwrap().len(), 1, "{got}");
    assert_eq!(claims["rows"][0][at("claim_id")], json!("c-1"));
    assert_eq!(claims["rows"][0][at("subject")], json!("e-acme"));
    // A caller who cannot read the cited row receives the identity and no claim from it.
    let memory_only = r.session(&["memory/*"], None, None);
    let gated = card(&r, &memory_only, "Acme Holdings");
    assert_eq!(gated["entity"]["entity_id"], json!("e-acme"));
    assert!(gated["claims"]["rows"].as_array().unwrap().is_empty(), "{gated}");
}

/// A `reference` matching two or more identities with no exact canonical name separating them returns their `entity_id`s as candidates and no identity; a `reference` matching none returns an empty card.
// spec: read.resolve-entity.card-candidates@f82234fc
#[test]
fn an_ambiguous_reference_returns_candidates_and_an_unknown_one_an_empty_card() {
    let r = fixture();
    let s = r.session(&["memory/*", "research/*"], None, None);
    let ambiguous = card(&r, &s, "GX");
    assert_eq!(ambiguous["entity"], Value::Null, "{ambiguous}");
    assert_eq!(ambiguous["candidates"], json!(["e-globex", "e-gx"]));
    assert!(ambiguous.get("claims").is_none(), "{ambiguous}");
    // An exact canonical name is the key separating identities.
    assert_eq!(card(&r, &s, "Globex")["entity"]["entity_id"], json!("e-globex"));
    let unknown = card(&r, &s, "Initech");
    assert_eq!(unknown, json!({ "entity": null, "candidates": [] }));
}

/// The card reads `table` and `claims` through the caller's session; a session registering no `claims` relation receives the identity alone, with no claims and no mention of that table.
// spec: read.resolve-entity.card-authorization@8694f890
#[test]
fn a_caller_without_a_claims_grant_receives_the_identity_alone() {
    let r = fixture();
    let s = r.session(&["memory/entities", "research/*"], None, None);
    let got = card(&r, &s, "ACME");
    assert_eq!(got, json!({ "entity": { "entity_id": "e-acme", "kind": "company", "name": "Acme Holdings", "aliases": ["ACME", "アクメ"] }, "candidates": [] }));
    assert!(!got.to_string().contains("memory/facts"));
    // Without a grant on the entities table the card refuses as any unregistered relation.
    let none = r.session(&["research/*"], None, None);
    let request = ExplainRequest::new("memory/entities", "ACME", at("2030-06-01T00:00:00Z"));
    refused_with(r.face.explain(&none, &request), "EnforceUnknownRelation");
}

/// A granted `table` declaring a shape other than `memory_entities`, or no memory shape, raises `MemoryExplainNotEntities`.
// spec: read.resolve-entity.card-not-entities@ba35fc4b
#[test]
fn a_table_declaring_no_entities_shape_is_refused() {
    let r = fixture();
    let s = r.session(&["memory/*", "research/*"], None, None);
    for table in ["memory/facts", "research/notes"] {
        let request = ExplainRequest::new(table, "ACME", at("2030-06-01T00:00:00Z"));
        refused_with(r.face.explain(&s, &request), "MemoryExplainNotEntities");
    }
}
