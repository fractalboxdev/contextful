//! The query-time layer through the engine: relation order and narrowing, the subject
//! and tenant relations, masks in place, refusals and zone exclusion.

use super::*;
use contextful_core::identify::Subject;
use serde_json::json;

fn contacts(r: &Reads, s: &Session, sql_tail: &str) -> Response {
    r.query(s, &format!(r#"SELECT * FROM "research/contacts" {sql_tail}"#)).unwrap()
}

fn sorted(mut v: Vec<Value>) -> Vec<Value> {
    v.sort_by_key(|x| x.to_string());
    v
}

/// A registered relation applies, in order: mirrored permission semi-join, tenant equality, credential predicate, table policy, project default, table zone, then the column projection carrying masks and zone nulls.
// spec: authority.compose.relation-order@fb165986
#[test]
fn the_relation_applies_tenant_policy_zone_then_projection() {
    let r = Reads::new();
    let s = r.session(&["research/*"], Some(("research/contacts", "acme")), None);
    let sql = s.relation("research/contacts").unwrap().sql().to_string();
    let tenant = sql.find("__contextful_tenant").unwrap();
    let policy = sql.find("__contextful_subject").unwrap();
    assert!(tenant < policy, "{sql}");
    // The projection masks over rows the predicates already selected on cleartext values.
    let rows = contacts(&r, &s, "ORDER BY contact_id");
    assert_eq!(column(&rows, "contact_id"), [json!("c1"), json!("c2")]);
    assert!(column(&rows, "handle").iter().all(|h| h.as_str().unwrap().len() == 32));
    let zoned = r.session(&["research/*"], Some(("research/contacts", "acme")), Some("public-cloud:x"));
    assert!(contacts(&r, &zoned, "").rows.is_empty(), "the zone step removes what the predicates admitted");
}

/// Steps conjoin: a surviving row survives each step alone, and appending a step removes rows and adds none.
// spec: authority.compose.conjunctive-narrowing@4eacf3e3
#[test]
fn each_added_step_only_removes_rows() {
    let r = Reads::new();
    let ids = |s: &Session| sorted(column(&contacts(&r, s, ""), "contact_id"));
    let auditor = r.session_for(loop_subject("agent://auditor"), vec![read(&["research/*"], None)], None);
    let tenant = r.session_for(loop_subject("agent://auditor"), vec![read(&["research/*"], Some(("research/contacts", "acme")))], None);
    let policy = r.session(&["research/*"], None, None);
    let both = r.session(&["research/*"], Some(("research/contacts", "acme")), None);
    let all = ids(&auditor);
    assert_eq!(all.len(), 4);
    for narrower in [ids(&tenant), ids(&policy), ids(&both)] {
        assert!(narrower.iter().all(|id| all.contains(id)));
    }
    let both_ids = ids(&both);
    assert!(both_ids.iter().all(|id| ids(&tenant).contains(id) && ids(&policy).contains(id)));
    assert_eq!(both_ids, [json!("c1"), json!("c2")]);
}

/// Protection is a rewrite in the data plane: predicates compiled into the statement, rewritten projections, a per-credential filter at the sync edge, zone floors. Admission decides entry alone.
// spec: authority.compose.protection-is-a-rewrite@7e5cd474
#[test]
fn protection_is_compiled_into_the_relation() {
    let r = Reads::new();
    let s = r.session(&["research/*"], Some(("research/contacts", "acme")), None);
    let sql = s.relation("research/contacts").unwrap().sql();
    assert!(sql.contains("\"owner\" = (SELECT \"agent\" FROM \"__contextful_subject\")"), "{sql}");
    assert!(sql.contains("contextful_mask_hash(CAST(\"handle\" AS VARCHAR)) AS \"handle\""), "{sql}");
    assert!(!sql.contains(PEPPER), "no key material rides the relation");
    // Admission let the credential in; the rewrite is what narrows its rows.
    assert_eq!(contacts(&r, &s, "").rows.len(), 2);
}

/// Every read of stored rows traverses the layers. A path reaching stored rows outside a registered relation is a defect, never a documented limitation.
// spec: authority.compose.complete-mediation@0978d4bb
#[test]
fn every_row_path_takes_the_session() {
    let r = Reads::new();
    let s = r.session(&["research/*"], Some(("research/contacts", "acme")), None);
    // Statement, template-free preview, description count and ranked arm all agree on
    // what this session sees of the contacts table.
    let statement = sorted(column(&contacts(&r, &s, ""), "contact_id"));
    let preview = r.face.file(&s, "tables/research/contacts/data/runs/run-0001/ingest-a/part-00000.parquet", ReadOptions::default()).unwrap();
    let described = r.face.describe(&s, Some("research/contacts")).unwrap();
    let ranked = r
        .face
        .retrieve(&s, &contextful_context::read::RetrieveRequest { prefix: "research/contacts".into(), query: String::new(), ..Default::default() }, Bounds::default())
        .unwrap();
    assert_eq!(sorted(column(&preview, "contact_id")), statement);
    assert_eq!(described["row_count"], json!(statement.len().to_string()));
    assert_eq!(sorted(column(&ranked, "_row").iter().map(|row| row["contact_id"].clone()).collect()), statement);
}

/// Subject claims reach a predicate as prepared-statement parameters through a session-scoped subject relation. A claim value carrying SQL syntax occupies a parameter slot and reaches no parser.
// spec: authority.filter-rows.subject-relation@7fc13855
#[test]
fn subject_claims_are_parameters_not_text() {
    let r = Reads::new();
    let honest = r.session(&["research/contacts"], None, None);
    assert_eq!(sorted(column(&contacts(&r, &honest, ""), "contact_id")), [json!("c1"), json!("c2"), json!("c4")]);
    let hostile = r.session_for(loop_subject("x' OR '1'='1"), vec![read(&["research/contacts"], None)], None);
    assert!(contacts(&r, &hostile, "").rows.is_empty());
    assert!(!hostile.relation("research/contacts").unwrap().sql().contains("'1'='1"));
}

/// A tenant-scoped grant compiles a bound-parameter byte equality on the tenant column, conjoined by the engine; no consumer statement carries it.
// spec: authority.filter-rows.tenant-equality@f1dd83fa
#[test]
fn a_tenant_scope_is_a_byte_equality_the_engine_conjoins() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], Some(("research/notes", "acme")), None);
    // The caller's statement names no tenant; the relation does.
    let rows = r.query(&s, r#"SELECT tenant, count(*) AS n FROM "research/notes" GROUP BY tenant"#).unwrap();
    assert_eq!(rows.rows, [vec![json!("acme"), json!("3")]]);
    assert!(!s.relation("research/notes").unwrap().sql().contains("'acme'"), "the value is a parameter");
    // Byte equality: a case-folded scope is another tenant.
    let folded = r.session(&["research/notes"], Some(("research/notes", "ACME")), None);
    assert!(r.query(&folded, r#"SELECT * FROM "research/notes""#).unwrap().rows.is_empty());
}

/// A table-policy predicate overriding for a matching subject condition replaces that one predicate; it reaches neither the tenant equality, the mirrored join nor the credential predicate.
// spec: authority.filter-rows.override@0f3b4dc3
#[test]
fn an_exception_replaces_the_table_predicate_alone() {
    let r = Reads::new();
    let auditor = r.session_for(loop_subject("agent://auditor"), vec![read(&["research/contacts"], Some(("research/contacts", "acme")))], None);
    // The auditor sees every owner's acme contacts, and still no globex contact.
    assert_eq!(sorted(column(&contacts(&r, &auditor, ""), "contact_id")), [json!("c1"), json!("c2"), json!("c3")]);
}

/// The masked projection substitutes named columns where they stand; the caller receives the column set and order the unmasked statement produces, and `*` expands to it.
// spec: authority.mask.in-place@c39d40e2
#[test]
fn masks_substitute_columns_where_they_stand() {
    let r = Reads::new();
    let s = r.session(&["research/contacts"], None, None);
    let masked = contacts(&r, &s, "");
    let schema: Vec<String> = r.store.schema("research/contacts").unwrap().columns.iter().map(|c| c.name.clone()).collect();
    assert_eq!(masked.columns, schema);
    let named = r.query(&s, r#"SELECT email, contact_id FROM "research/contacts" ORDER BY contact_id"#).unwrap();
    assert_eq!(named.columns, ["email", "contact_id"]);
    assert!(column(&named, "email").iter().all(|e| e.as_str().unwrap().len() == 5));
}

/// An equality filter on a `drop` column matches nothing; on a `hash` column it matches equal digests.
// spec: authority.mask.equality@d4d69130
#[test]
fn equality_filters_read_the_masked_value() {
    let r = Reads::new();
    let s = r.session(&["research/contacts"], None, None);
    assert!(contacts(&r, &s, "WHERE phone = '555-0100'").rows.is_empty());
    assert!(contacts(&r, &s, "WHERE age = 40").rows.is_empty());
    assert!(contacts(&r, &s, "WHERE handle = 'h1'").rows.is_empty());
    let digest = pepper().digest("h1");
    let matched = contacts(&r, &s, &format!("WHERE handle = '{digest}' ORDER BY contact_id"));
    assert_eq!(column(&matched, "contact_id"), [json!("c1"), json!("c2")]);
}

/// Aggregation reads the masked column.
// spec: authority.mask.aggregates@70b916a8
#[test]
fn aggregates_read_the_masked_column() {
    let r = Reads::new();
    let s = r.session(&["research/contacts"], None, None);
    let agg = r
        .query(&s, r#"SELECT count(DISTINCT handle) AS handles, max(age) AS oldest, min(email) AS first FROM "research/contacts""#)
        .unwrap();
    assert_eq!(column(&agg, "handles"), [json!("2")]);
    assert_eq!(column(&agg, "oldest"), [Value::Null]);
    assert_eq!(column(&agg, "first")[0].as_str().unwrap().len(), 5);
}

/// `drop` nulls the cell, or yields an empty string for a string column.
// spec: authority.mask.drop@df7cb65e
#[test]
fn drop_nulls_a_cell_or_empties_a_string() {
    let r = Reads::new();
    let s = r.session(&["research/contacts"], None, None);
    let rows = contacts(&r, &s, "");
    assert!(column(&rows, "phone").iter().all(|p| p == &json!("")));
    assert!(column(&rows, "age").iter().all(Value::is_null));
}

/// Both layers compute the keyed digest natively; the query layer calls a scalar function holding the pepper in process memory, and a value masked at either layer joins the other.
// spec: authority.mask.native-digest@29d59e18
#[test]
fn a_digest_from_either_layer_joins_the_other() {
    let r = Reads::new();
    let s = r.session(&["research/contacts"], None, None);
    let policy = contextful_policy::enforce::policy::TablePolicy::from_decl(
        &TableDecl::parse_pipeline(MANIFEST).unwrap().into_iter().find(|d| d.name == "research/contacts").unwrap(),
    )
    .unwrap();
    let written = policy.columns["handle"].mask.as_ref().unwrap().apply(&pepper(), "h1").unwrap();
    let joined = contacts(&r, &s, &format!("WHERE handle IN (SELECT '{written}') ORDER BY contact_id"));
    assert_eq!(column(&joined, "contact_id"), [json!("c1"), json!("c2")]);
}

/// One environment variable supplies the key for `hash` and `tokenize`, read by both layers as one resolved value.
// spec: authority.mask.pepper@48c78b9d
#[test]
fn one_resolved_pepper_keys_both_layers() {
    let r = Reads::new();
    let s = r.session(&["research/contacts"], None, None);
    let queried = column(&r.query(&s, r#"SELECT handle FROM "research/contacts" WHERE contact_id = 'c4'"#).unwrap(), "handle");
    assert_eq!(queried, [json!(pepper().digest("h4"))]);
    let other = Pepper::resolve(|v| (v == PEPPER_VAR).then(|| "another".to_string()));
    assert_ne!(queried[0], json!(other.digest("h4")));
}

/// A column mask is one value whose primary and combine halves are private; it drives both the write-time and query-time layers, and no consumer applies the primary alone.
// spec: authority.mask.one-value@337bf787
#[test]
fn a_mask_applies_primary_and_combine_together_in_both_layers() {
    let r = Reads::new();
    let s = r.session(&["research/contacts"], None, None);
    let queried = column(&r.query(&s, r#"SELECT email FROM "research/contacts" WHERE contact_id = 'c1'"#).unwrap(), "email");
    let policy = contextful_policy::enforce::policy::TablePolicy::from_decl(
        &TableDecl::parse_pipeline(MANIFEST).unwrap().into_iter().find(|d| d.name == "research/contacts").unwrap(),
    )
    .unwrap();
    let written = policy.columns["email"].mask.as_ref().unwrap().apply(&pepper(), "dana@acme.example").unwrap();
    assert_eq!(written.len(), 5);
    assert_eq!(queried, [json!(written)]);
    assert!(pepper().digest("dana@acme.example").starts_with(&written));
}

/// A tenant-scoped credential reading another tenant's rows on a granted table raises `EnforceScopeDenied`, as wire code `scope_denied`, HTTP `403` or an in-band tool error, naming the table and both scopes.
// spec: authority.refuse.scope-denied@309f33ad
#[test]
fn reading_another_tenant_is_refused_naming_both_scopes() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], Some(("research/notes", "acme")), None);
    let message = refused_with(r.query(&s, r#"SELECT * FROM "research/notes" WHERE tenant = 'globex'"#), "EnforceScopeDenied");
    assert!(message.contains("research/notes") && message.contains("`acme`") && message.contains("`globex`"), "{message}");
    let fault = r.query(&s, r#"SELECT * FROM "research/notes" WHERE tenant = 'globex'"#).unwrap_err();
    let wire = contextful_policy::enforce::refuse::payload(fault.refusal().unwrap());
    assert_eq!((wire["error"]["code"].clone(), wire["error"]["http"].clone()), (json!("scope_denied"), json!(403)));
}

/// A scope refusal is a typed error, never an empty result.
// spec: authority.refuse.not-empty@06042c11
#[test]
fn a_scope_refusal_is_not_an_empty_result() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], Some(("research/notes", "acme")), None);
    assert!(r.query(&s, r#"SELECT * FROM "research/notes" WHERE tenant = 'globex'"#).is_err());
    // The same predicate a caller could not settle alone reads empty through the relation.
    assert!(r.query(&s, r#"SELECT * FROM "research/notes" WHERE tenant = 'nobody' OR false"#).unwrap().rows.is_empty());
}

/// The requested scope in a refusal echoes the statement's value, never a stored value.
// spec: authority.refuse.echo@c175aa67
#[test]
fn a_refusal_echoes_the_statements_value() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], Some(("research/notes", "acme")), None);
    let message = refused_with(r.query(&s, r#"SELECT * FROM "research/notes" WHERE tenant = 'Globex Holdings'"#), "EnforceScopeDenied");
    assert!(message.contains("`Globex Holdings`"), "{message}");
    assert!(!message.contains("`globex`"), "no stored tenant appears: {message}");
}

/// A relation the caller holds no grant on, whether or not a table of that name exists, raises `EnforceUnknownRelation` and is absent from listings and descriptions.
// spec: authority.refuse.ungranted-table@f5f8b836
#[test]
fn an_ungranted_relation_is_unknown_everywhere() {
    let r = Reads::new();
    let s = r.session(&["research/*"], None, None);
    refused_with(r.query(&s, r#"SELECT * FROM "hr/salaries""#), "EnforceUnknownRelation");
    refused_with(r.query(&s, r#"SELECT * FROM "hr/nothing""#), "EnforceUnknownRelation");
    refused_with(r.face.describe(&s, Some("hr/salaries")), "EnforceUnknownRelation");
    refused_with(r.face.describe(&s, Some("hr/nothing")), "EnforceUnknownRelation");
    let listing = r.face.describe(&s, None).unwrap();
    let names: Vec<&str> = listing["tables"].as_array().unwrap().iter().map(|t| t["table"].as_str().unwrap()).collect();
    assert!(names.contains(&"research/notes") && !names.iter().any(|n| n.starts_with("hr/")), "{names:?}");
}

/// The scope guard walks the engine's parse once, deciding literal equalities and membership lists over the tenant column and bound template parameters destined for it.
// spec: authority.refuse.scope-guard@fce154e6
#[test]
fn the_scope_guard_decides_literals_lists_and_template_parameters() {
    let r = Reads::new();
    let mut grant = read(&["research/notes"], Some(("research/notes", "acme")));
    grant.templates = Some(vec!["notes_for".into()]);
    let s = r.session_for(loop_subject("agent://research-loop"), vec![grant], None);
    refused_with(r.query(&s, r#"SELECT * FROM "research/notes" WHERE tenant IN ('acme', 'globex')"#), "EnforceScopeDenied");
    refused_with(r.query(&s, r#"SELECT * FROM "research/notes" n WHERE n.tenant = 'globex' AND note_id <> 'x'"#), "EnforceScopeDenied");
    assert_eq!(r.query(&s, r#"SELECT * FROM "research/notes" WHERE tenant IN ('acme')"#).unwrap().rows.len(), 3);
    let args = |t: &str| [("tenant".to_string(), json!(t))].into_iter().collect::<serde_json::Map<_, _>>();
    refused_with(r.face.execute_template(&s, "notes_for", &args("globex"), ReadOptions::default()), "EnforceScopeDenied");
    assert_eq!(r.face.execute_template(&s, "notes_for", &args("acme"), ReadOptions::default()).unwrap().rows.len(), 3);
}

/// The guard reads top-level conjuncts of a statement whose own FROM names a scoped table.
// spec: authority.refuse.top-level@f6cdc6f4
#[test]
fn the_guard_reads_top_level_conjuncts_alone() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], Some(("research/notes", "acme")), None);
    // A disjunct, or a scoped table reached through a subquery, is left to the relation.
    let nested = r.query(&s, r#"SELECT * FROM (SELECT * FROM "research/notes") WHERE tenant = 'globex'"#).unwrap();
    assert!(nested.rows.is_empty());
    let disjunct = r.query(&s, r#"SELECT * FROM "research/notes" WHERE tenant = 'globex' OR note_id = 'n4'"#).unwrap();
    assert!(disjunct.rows.is_empty());
}

/// A constraint the guard cannot settle composes as an ordinary conjunct; the engine-applied equality isolates.
// spec: authority.refuse.undecidable@8939fe68
#[test]
fn an_unsettled_constraint_composes_and_the_equality_isolates() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], Some(("research/notes", "acme")), None);
    let computed = r.query(&s, r#"SELECT * FROM "research/notes" WHERE tenant = lower('GLOBEX')"#).unwrap();
    assert!(computed.rows.is_empty());
    let ranged = r.query(&s, r#"SELECT note_id FROM "research/notes" WHERE tenant >= 'a' ORDER BY note_id"#).unwrap();
    assert_eq!(column(&ranged, "note_id"), [json!("n1"), json!("n2"), json!("n3")]);
}

/// The guard visits at most 4096 entries of one parse tree.
// spec: authority.refuse.guard-walk@9ad6ba48
#[test]
fn a_tree_past_the_walk_bound_composes_as_ordinary_conjuncts() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], Some(("research/notes", "acme")), None);
    let small = format!(r#"SELECT * FROM "research/notes" WHERE tenant = 'globex' AND note_id NOT IN ({})"#, (0..10).map(|i| format!("'x{i}'")).collect::<Vec<_>>().join(","));
    refused_with(r.query(&s, &small), "EnforceScopeDenied");
    let large = format!(r#"SELECT * FROM "research/notes" WHERE tenant = 'globex' AND note_id NOT IN ({})"#, (0..2000).map(|i| format!("'x{i}'")).collect::<Vec<_>>().join(","));
    assert!(r.query(&s, &large).unwrap().rows.is_empty());
}

/// A tenant value matching no partition reads empty, like a tenant that wrote nothing; the read path repairs no transformed scope value.
// spec: authority.refuse.drifted-scope@ede634b9
#[test]
fn a_drifted_scope_reads_empty() {
    let r = Reads::new();
    for drifted in ["acme ", " acme", "Acme"] {
        let s = r.session(&["research/notes"], Some(("research/notes", drifted)), None);
        assert!(r.query(&s, r#"SELECT * FROM "research/notes""#).unwrap().rows.is_empty(), "{drifted:?}");
    }
}

/// The calling process declares its zone per request; the store carries none.
// spec: authority.place.caller-zone@5d9858e0
#[test]
fn the_zone_is_declared_per_request() {
    let r = Reads::new();
    let authority = r.authority(loop_subject("agent://research-loop"), vec![read(&["research/*"], None)]);
    let vendor = |zone: Option<&str>| {
        let s = r.face.session(&authority, &Request { zone }, Bounds::default()).unwrap();
        r.query(&s, r#"SELECT item_id FROM "research/vendor""#).unwrap().rows.len()
    };
    assert_eq!(vendor(None), 0, "the credential's on-prem zone");
    assert_eq!(vendor(Some("public-cloud:us-east-1")), 1);
    assert_eq!(vendor(Some("on-prem:hq")), 0);
}

/// Where the table's effective set omits the session's zone, the row leaves the result.
// spec: authority.place.excluded-row@897e2a18
#[test]
fn a_zone_outside_the_table_set_drops_its_rows() {
    let r = Reads::new();
    let s = r.session(&["research/*"], None, Some("private-cloud:vpc-7"));
    for t in ["research/notes", "research/vendor", "research/visits"] {
        let rows = r.query(&s, &format!("SELECT * FROM \"{t}\"")).unwrap();
        assert!(rows.rows.is_empty(), "{t}");
    }
}

/// A cell whose effective set omits the session's zone arrives null, a mask exception notwithstanding.
// spec: authority.place.excluded-cell@b0645ca1
#[test]
fn a_cell_outside_its_column_set_arrives_null() {
    let r = Reads::new();
    let cloud = r.session(&["research/visits"], None, Some("public-cloud:us-east-1"));
    let rows = r.query(&cloud, r#"SELECT visit_id, case_notes FROM "research/visits""#).unwrap();
    assert_eq!(rows.rows, [vec![json!("w1"), Value::Null]]);
    let ward = r.session(&["research/visits"], None, Some("on-prem:ward-3"));
    let rows = r.query(&ward, r#"SELECT case_notes FROM "research/visits""#).unwrap();
    assert_eq!(column(&rows, "case_notes"), [json!("chest pain, stable")]);
}

// An incognito session with no asserted zone serves under `local:device`, and a wider
// assertion refuses.
#[test]
fn an_incognito_session_serves_under_the_fail_closed_pair() {
    let r = Reads::new();
    let subject = Subject { incognito: true, zone: None, ..loop_subject("agent://research-loop") };
    let authority = r.authority(subject, vec![read(&["research/*"], None)]);
    let s = r.face.session(&authority, &Request { zone: None }, Bounds::default()).unwrap();
    assert!(r.query(&s, r#"SELECT * FROM "research/notes""#).unwrap().rows.len() == 3);
    let widened = r.face.session(&authority, &Request { zone: Some("public-cloud:us-east-1") }, Bounds::default());
    refused_with(widened, "EnforceIncognitoWidening");
}
