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

/// The walk covers the entire tree — select-list subqueries, union arms, pivot sources — and a common-table-expression name resolves only within the scope that declares it and the scopes beneath.
// spec: read.guard.whole-tree-walk@d78811bc
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
    // A CTE resolves in the scope declaring it and beneath; a sibling scope reaching its
    // name reaches a base relation.
    let nested = r.query(&s, r#"WITH acme AS (SELECT * FROM "research/notes") SELECT (SELECT count(*) FROM acme) AS n"#).unwrap();
    assert_eq!(column(&nested, "n"), [json!("4")]);
    let tree = r.face.serialize(r#"SELECT (SELECT count(*) FROM later) FROM (WITH later AS (SELECT 1 AS x) SELECT * FROM later)"#).unwrap();
    assert!(admit(&tree, |_| false).is_err());
}

/// Regression: a CTE declared in one subquery never admits a base relation of the same
/// name in another, whatever that name reaches — a store file by path, the engine's
/// catalog, or another caller's table.
#[test]
fn a_cte_name_in_one_scope_admits_nothing_in_another() {
    let r = Reads::new();
    let s = r.session(&["research/*"], None, None);
    let part = format!("{}/tables/hr/salaries/data/runs/run-0001/ingest-a/part-00000.parquet", r.store.root().display());
    for (name, error) in [
        (part.as_str(), "EnforceUnknownRelation"),
        ("sqlite_master", "TableFunctionRefused"),
        ("duckdb_tables", "TableFunctionRefused"),
        ("hr/salaries", "EnforceUnknownRelation"),
    ] {
        let text = format!(r#"SELECT * FROM (WITH "{name}" AS (SELECT 1 AS x) SELECT x FROM "{name}") s, "{name}""#);
        refused_with(r.query(&s, &text), error);
    }
}

/// Regression: the engine a caller's statement runs on reaches no file, extension or
/// setting beyond the session's relations, whatever the guard admits.
#[test]
fn the_session_engine_is_closed_to_files_and_settings() {
    let r = Reads::new();
    let s = r.session(&["research/*"], None, None);
    let part = format!("{}/tables/hr/salaries/data/runs/run-0001/ingest-a/part-00000.parquet", r.store.root().display());
    for text in [
        format!("SELECT * FROM '{part}'"),
        format!("SELECT * FROM \"{part}\""),
        format!("SELECT * FROM (SELECT * FROM read_parquet('{part}'))"),
        "SELECT * FROM sqlite_master".to_string(),
        "SELECT * FROM duckdb_views()".to_string(),
        "SELECT * FROM information_schema.tables".to_string(),
        "SELECT * FROM pg_catalog.pg_class".to_string(),
        "SELECT * FROM main.sqlite_master".to_string(),
    ] {
        let (id, _) = refusal(r.query(&s, &text));
        assert!(["EnforceUnknownRelation", "TableFunctionRefused"].contains(&id.as_str()), "{text}: {id}");
    }
    // Operator-composed reads run on the same locked connection: a template naming a
    // store table still reads, and no statement reopens the configuration.
    refused_with(r.query(&s, "SET enable_external_access = true"), "StatementNotReadOnly");
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

/// Manifest validation and face startup refuse a template whose SQL names anything but the store's own tables as plain identifiers, or whose identifier opens with a built-in tool prefix — `context.`, `corpus.` or `memory.` — raising `TemplateNamesForeignRelation`.
// spec: read.guard.template-relation-shape@19814a81
#[test]
fn a_template_naming_a_foreign_relation_is_refused_at_startup() {
    for tail in [
        "[[query_templates]]\nid = \"raw\"\nsql = \"SELECT * FROM read_parquet('x.parquet')\"\n",
        "[[query_templates]]\nid = \"catalog\"\nsql = \"SELECT * FROM information_schema.tables\"\n",
        "[[query_templates]]\nid = \"elsewhere\"\nsql = \"SELECT * FROM payroll\"\n",
        "[[query_templates]]\nid = \"attached\"\nsql = \"SELECT * FROM other.main.notes\"\n",
        "[[query_templates]]\nid = \"context.leak\"\nsql = \"SELECT note_id FROM \\\"research/notes\\\"\"\n",
        "[[query_templates]]\nid = \"corpus.leak\"\nsql = \"SELECT note_id FROM \\\"research/notes\\\"\"\n",
        "[[query_templates]]\nid = \"memory.recall\"\nsql = \"SELECT note_id FROM \\\"research/notes\\\"\"\n",
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

/// Caller queries bind typed named or numbered placeholders; positional `?` refuses, and
/// opt-in internals echo the validated bindings.
// spec: read.guard.query-binding@06a8da63
// spec: read.respond.query-internals-parameters@161d7143
#[test]
fn query_parameters_bind_by_declared_type() {
    let r = Reads::new();
    let s = r.session(&["research/*"], None, None);
    let params = |v: Value| v.as_object().unwrap().clone();
    let run = |sql: &str, p: Value| r.face.query_with(&s, sql, &params(p), ReadOptions::default());
    let sql = r#"SELECT note_id FROM "research/notes" WHERE tenant = $tenant AND published_at >= $since ORDER BY note_id"#;
    let typed = json!({ "tenant": { "type": "string", "value": "acme" }, "since": { "type": "string", "value": "2030-01-01" } });
    assert_eq!(column(&run(sql, typed).unwrap(), "note_id"), [json!("n1"), json!("n3")]);
    assert_eq!(column(&run(r#"SELECT note_id FROM "research/notes" WHERE tenant = $1 ORDER BY note_id"#, json!({
        "1": { "type": "string", "value": "acme" }
    })).unwrap(), "note_id"), [json!("n1"), json!("n2"), json!("n3")]);
    assert_eq!(column(&run("SELECT $1 AS result /* ? is a comment */", json!({
        "1": { "type": "string", "value": "valid" }
    })).unwrap(), "result"), [json!("valid")]);
    assert_eq!(column(&run("SELECT $1 AS result, $$?$$ AS literal", json!({
        "1": { "type": "string", "value": "valid" }
    })).unwrap(), "literal"), [json!("?")]);
    assert_eq!(column(&run("SELECT $1 AS result, $tag$?$tag$ AS literal", json!({
        "1": { "type": "string", "value": "valid" }
    })).unwrap(), "literal"), [json!("?")]);
    assert_eq!(column(&run(r"SELECT $1 AS result, E'it\'s ?' AS literal", json!({
        "1": { "type": "string", "value": "valid" }
    })).unwrap(), "literal"), [json!("it's ?")]);
    assert!(run(r#"SELECT note_id FROM "research/notes" WHERE note_id = $id"#, json!({
        "id": { "type": "string", "value": "' OR 1=1 --" }
    })).unwrap().rows.is_empty());
    let all = run(
        "SELECT $n + 1 AS n, $f * 2 AS f, $b AS b, $t AS t",
        json!({
            "n": { "type": "integer", "value": 41 }, "f": { "type": "float", "value": 1.25 },
            "b": { "type": "boolean", "value": true }, "t": { "type": "timestamp", "value": "2030-01-05T00:00:00Z" },
        }),
    )
    .unwrap();
    assert_eq!(all.rows.len(), 1);
    assert_eq!(column(&all, "n"), [json!("42")]);

    let one = |ty: &str, value: Value| json!({ "tenant": { "type": ty, "value": value } });
    let tenant_sql = r#"SELECT note_id FROM "research/notes" WHERE tenant = $tenant"#;
    for (text, p, needle) in [
        (sql, json!({ "tenant": { "type": "string", "value": "acme" } }), "`since`"),
        (tenant_sql, json!({ "tenant": { "type": "string", "value": "acme" }, "extra": { "type": "integer", "value": 1 } }), "`extra`"),
        (tenant_sql, one("decimal", json!("acme")), "`decimal`"),
        (tenant_sql, json!({ "tenant": "acme" }), "`tenant`"),
        (tenant_sql, json!({ "tenant": { "value": "acme" } }), "`tenant`"),
        (tenant_sql, json!({ "tenant": { "type": "string", "value": "acme", "cast": true } }), "`cast`"),
        (tenant_sql, one("integer", json!("2")), "`tenant`"),
        (tenant_sql, one("integer", json!(2.0)), "`tenant`"),
        (tenant_sql, one("boolean", json!(1)), "`tenant`"),
        (tenant_sql, one("timestamp", json!("yesterday")), "`tenant`"),
        (r#"SELECT note_id FROM "research/notes" WHERE tenant = ?"#, json!({}), "positional"),
        (r#"SELECT note_id FROM "research/notes" WHERE tenant = ?"#, json!({ "1": { "type": "string", "value": "acme" } }), "positional"),
        // Numbered placeholders run contiguously from `1`; a gap is a placeholder with no parameter.
        (r#"SELECT note_id FROM "research/notes" WHERE tenant = $2"#, json!({ "2": { "type": "string", "value": "acme" } }), "`1`"),
        (
            r#"SELECT note_id FROM "research/notes" WHERE tenant = $2 AND note_id <> $3"#,
            json!({ "2": { "type": "string", "value": "acme" }, "3": { "type": "string", "value": "n2" } }),
            "`1`",
        ),
    ] {
        let message = refused_with(run(text, p.clone()), "QueryParameterRejected");
        assert!(message.contains(needle), "{p}: {message}");
    }
    // The statement guard answers first: a parameter refusal never masks an ungranted table.
    refused_with(run(r#"SELECT * FROM "hr/salaries" WHERE employee = $e"#, json!({})), "EnforceUnknownRelation");

    // A bound tenant value reaches the scope guard as a literal does.
    let scoped = r.session(&["research/notes"], Some(("research/notes", "acme")), None);
    let tenant = |t: &str| params(json!({ "tenant": { "type": "string", "value": t } }));
    refused_with(r.face.query_with(&scoped, tenant_sql, &tenant("globex"), ReadOptions::default()), "EnforceScopeDenied");
    assert_eq!(r.face.query_with(&scoped, tenant_sql, &tenant("acme"), ReadOptions::default()).unwrap().rows.len(), 3);

    let echoed = r.face.query_with(&s, tenant_sql, &tenant("acme"), ReadOptions { internals: true, ..ReadOptions::default() }).unwrap();
    assert_eq!(echoed.to_json()["contextful.internals"]["parameters"], json!({ "tenant": { "type": "string", "value": "acme" } }));
}

/// Regression: a template's placeholders are `$1`…`$n` or `?` in declaration order, or the
/// declared names; either spelling binds and runs, and any other placeholder set refuses
/// the face at startup.
#[test]
fn a_template_binds_numbered_and_named_placeholders() {
    let tail = r#"
[[query_templates]]
id = "notes_named"
sql = "SELECT note_id FROM \"research/notes\" WHERE tenant = $tenant AND note_id <> $skip ORDER BY note_id"
parameters = ["tenant:string", "skip:string"]

[[query_templates]]
id = "notes_numbered"
sql = "SELECT note_id FROM \"research/notes\" WHERE note_id <> $2 AND tenant = $1 ORDER BY note_id"
parameters = ["tenant:string", "skip:string"]
"#;
    let r = Reads::with_manifest(&format!("{MANIFEST}\n{tail}"));
    let mut grant = read(&["research/*"], None);
    grant.templates = Some(vec!["notes_named".into(), "notes_numbered".into()]);
    let s = r.session_for(loop_subject("agent://research-loop"), vec![grant], None);
    let args = json!({ "tenant": "acme", "skip": "n2" }).as_object().unwrap().clone();
    for id in ["notes_named", "notes_numbered"] {
        let rows = r.face.execute_template(&s, id, &args, ReadOptions::default()).unwrap();
        assert_eq!(column(&rows, "note_id"), [json!("n1"), json!("n3")], "{id}");
    }
    for sql in [
        r#"SELECT note_id FROM \"research/notes\" WHERE tenant = $tenant AND note_id <> $other"#,
        r#"SELECT note_id FROM \"research/notes\" WHERE tenant = $2 AND note_id <> $3"#,
        r#"SELECT note_id FROM \"research/notes\" WHERE tenant = $tenant"#,
    ] {
        let refused = refused_template(&format!(
            "[[query_templates]]\nid = \"off\"\nsql = \"{sql}\"\nparameters = [\"tenant:string\", \"skip:string\"]\n"
        ));
        assert!(refused.starts_with("TemplateArgumentRejected"), "{sql}: {refused}");
    }
}
