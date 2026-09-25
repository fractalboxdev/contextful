//! `read.guard`: admission of caller SQL over the engine's own parse, and the template
//! startup checks.

use super::*;
use contextful_core::read::guard::admit;
use serde_json::json;

fn refused_template(manifest_tail: &str) -> String {
    let manifest = format!("{MANIFEST}\n{manifest_tail}");
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), "research").unwrap();
    super::land_rows(&store, "research/notes", "run-0001", json!([{ "note_id": "n1", "tenant": "acme", "author_email": "dana@acme.example" }]));
    match Face::open(store, &manifest, pepper()) {
        Err(ReadFault::Refused(r)) => format!("{}: {r}", r.identifier()),
        Err(other) => panic!("{other}"),
        Ok(_) => panic!("the face opened over {manifest_tail}"),
    }
}

/// Admitted text parses to exactly one read-only SELECT. Attach, copy, install, load, pragma, set, any schema change, any data modification and a piggybacked second statement raise `StatementNotReadOnly`.
// spec: read.guard.single-read-only-statement@7539c519
#[test]
fn only_one_read_only_select_is_admitted() {
    let r = Reads::new();
    let s = r.session(&["research/*"], None, None);
    for text in [
        r#"DELETE FROM "research/notes""#,
        r#"INSERT INTO "research/notes" VALUES (1)"#,
        r#"UPDATE "research/notes" SET title = 'x'"#,
        "ATTACH 'other.db' AS other",
        r#"COPY "research/notes" TO 'out.csv'"#,
        "INSTALL httpfs",
        "LOAD httpfs",
        "PRAGMA database_list",
        "SET threads = 1",
        "CREATE TABLE t (x INTEGER)",
        r#"DROP VIEW "research/notes""#,
        r#"SELECT 1; SELECT note_id FROM "research/notes""#,
        r#"SELECT note_id FROM "research/notes"; DELETE FROM "research/notes""#,
    ] {
        refused_with(r.query(&s, text), "StatementNotReadOnly");
    }
    assert_eq!(r.query(&s, r#"SELECT count(*) AS n FROM "research/notes""#).unwrap().rows.len(), 1);
}

/// The guard walks the syntax tree the engine itself serializes for the statement; read-only-ness is a property of that tree, not a keyword blocklist.
// spec: read.guard.engine-own-parse@bc83988b
#[test]
fn read_only_ness_is_a_property_of_the_engines_tree() {
    let r = Reads::new();
    let s = r.session(&["research/*"], None, None);
    // Keywords inside a literal and a quoted identifier name nothing the engine executes.
    let text = r#"SELECT 'DELETE FROM x; DROP TABLE y' AS "update", note_id FROM "research/notes" WHERE tenant = 'acme' ORDER BY note_id"#;
    let admitted = r.query(&s, text).unwrap();
    assert_eq!(column(&admitted, "update")[0], json!("DELETE FROM x; DROP TABLE y"));
    let tree = r.face.serialize(text).unwrap();
    assert_eq!(tree["error"], json!(false));
    assert_eq!(tree["statements"].as_array().unwrap().len(), 1);
}

/// Every base relation names a view registered for this caller or a common table expression the statement declares. A bare file path is a base relation and falls under this rule.
// spec: read.guard.relation-allowlist@44edb2f1
#[test]
fn base_relations_are_registered_views_or_declared_ctes() {
    let r = Reads::new();
    let s = r.session(&["research/*"], None, None);
    let cte = r.query(&s, r#"WITH acme AS (SELECT * FROM "research/notes" WHERE tenant = 'acme') SELECT count(*) AS n FROM acme"#).unwrap();
    assert_eq!(column(&cte, "n"), [json!("3")]);
    let part = format!("{}/tables/hr/salaries/data/runs/run-0001/ingest-a/part-00000.parquet", r.store.root().display());
    assert!(std::path::Path::new(&part).is_file());
    let message = refused_with(r.query(&s, &format!("SELECT * FROM '{part}'")), "EnforceUnknownRelation");
    assert!(message.contains("part-00000.parquet"), "{message}");
}

/// The walk covers the entire tree — select-list subqueries, union arms, pivot sources — and gathers common-table-expression names across the tree before checking any base relation.
// spec: read.guard.whole-tree-walk@4f71069a
#[test]
fn the_walk_covers_every_subtree() {
    let r = Reads::new();
    let s = r.session(&["research/*"], None, None);
    for text in [
        r#"SELECT note_id, (SELECT count(*) FROM "hr/salaries") AS n FROM "research/notes""#,
        r#"SELECT note_id FROM "research/notes" UNION ALL SELECT employee FROM "hr/salaries""#,
        r#"SELECT * FROM "hr/salaries" PIVOT (count(*) FOR title IN ('x'))"#,
        r#"SELECT * FROM "research/notes" WHERE note_id IN (SELECT employee FROM "hr/salaries")"#,
    ] {
        let message = refused_with(r.query(&s, text), "EnforceUnknownRelation");
        assert!(message.contains("hr/salaries"), "{text}: {message}");
    }
    // A CTE declared deeper in the tree than its first use still counts as declared.
    let text = r#"SELECT (SELECT count(*) FROM later) FROM (WITH later AS (SELECT 1 AS x) SELECT * FROM later)"#;
    let tree = r.face.serialize(text).unwrap();
    assert!(admit(&tree, |_| false).is_ok());
}

/// A base relation naming nothing this connection registered is refused by {{authority.refuse.ungranted-table}}, echoing what the statement asked for and no relation of another caller.
// spec: read.guard.unregistered-relation@87f86239
#[test]
fn an_unregistered_relation_echoes_the_statements_spelling() {
    let r = Reads::new();
    let s = r.session(&["research/*"], None, None);
    let existing = refused_with(r.query(&s, r#"SELECT * FROM "hr/salaries""#), "EnforceUnknownRelation");
    let absent = refused_with(r.query(&s, "SELECT * FROM payroll"), "EnforceUnknownRelation");
    assert!(existing.contains("hr/salaries") && absent.contains("payroll"));
    assert!(!absent.contains("research/"), "no other relation is named: {absent}");
}

/// A table function anywhere in the tree, and a schema-qualified reach into a system catalog, raise `TableFunctionRefused`.
// spec: read.guard.table-function@df263840
#[test]
fn table_functions_and_catalog_reaches_are_refused() {
    let r = Reads::new();
    let s = r.session(&["research/*"], None, None);
    for text in [
        "SELECT * FROM read_parquet('tables/hr/salaries/data/runs/run-0001/ingest-a/part-00000.parquet')",
        "SELECT * FROM duckdb_tables()",
        r#"SELECT note_id FROM "research/notes" WHERE note_id IN (SELECT name FROM duckdb_settings())"#,
        "SELECT * FROM information_schema.tables",
        "SELECT * FROM pg_catalog.pg_class",
        "SELECT * FROM range(10)",
    ] {
        refused_with(r.query(&s, text), "TableFunctionRefused");
    }
}

/// Every schema- or manifest-derived identifier is double-quoted where rendered, admitting any UTF-8 vendor field name as an identifier and never as an expression.
// spec: read.guard.quoted-identifiers@4ed9d1f1
#[test]
fn vendor_field_names_render_as_identifiers() {
    let r = Reads::new();
    let s = r.session(&["research/*"], None, Some("public-cloud:us-east-1"));
    let row = r.query(&s, r#"SELECT * FROM "research/vendor""#).unwrap();
    assert!(row.columns.contains(&"Unit Price (USD)".to_string()), "{:?}", row.columns);
    assert!(row.columns.contains(&"a\"b".to_string()), "{:?}", row.columns);
    assert_eq!(column(&row, "Unit Price (USD)"), [json!("12")]);
    assert_eq!(column(&row, "a\"b"), [json!("quoted")]);
}

/// Manifest validation and face startup refuse a template whose SQL names anything but the store's own tables as plain identifiers, or whose identifier collides with a built-in tool prefix, raising `TemplateNamesForeignRelation`.
// spec: read.guard.template-relation-shape@2b311161
#[test]
fn a_template_naming_a_foreign_relation_is_refused_at_startup() {
    for tail in [
        "[[query_templates]]\nid = \"raw\"\nsql = \"SELECT * FROM read_parquet('x.parquet')\"\n",
        "[[query_templates]]\nid = \"catalog\"\nsql = \"SELECT * FROM information_schema.tables\"\n",
        "[[query_templates]]\nid = \"elsewhere\"\nsql = \"SELECT * FROM payroll\"\n",
        "[[query_templates]]\nid = \"attached\"\nsql = \"SELECT * FROM other.main.notes\"\n",
        "[[query_templates]]\nid = \"context.leak\"\nsql = \"SELECT note_id FROM \\\"research/notes\\\"\"\n",
    ] {
        let refused = refused_template(tail);
        assert!(refused.starts_with("TemplateNamesForeignRelation"), "{tail}: {refused}");
    }
}

/// Template checks are caller-independent and run once at startup; a request pays nothing for them.
// spec: read.guard.startup-time-check@3ed4f65a
#[test]
fn template_checks_run_once_when_the_face_opens() {
    // A placeholder mismatch refuses the face before any credential is seen.
    let refused = refused_template("[[query_templates]]\nid = \"mismatch\"\nsql = \"SELECT note_id FROM \\\"research/notes\\\" WHERE tenant = ? AND note_id = ?\"\nparameters = [\"tenant:string\"]\n");
    assert!(refused.starts_with("TemplateArgumentRejected"), "{refused}");
    let r = Reads::new();
    assert_eq!(r.face.templates().len(), 1);
}
