//! `authority.grant`: the action vocabulary, table patterns and their one matcher,
//! tenant scope, templates, pipelines as resources, and ceilings.

use contextful_core::grant::{
    authorize_template, describe_pipeline, least_row_ceiling, list_pipelines, list_templates, raw_read_covers,
    tenant_child_expiry, trace_run, AggregateGrant, Action, Grant, TablePattern, TenantScope,
    AGGREGATE_GROUP_CEILING_FLOOR, TENANT_CHILD_LIFETIME_MINUTES,
};
use contextful_core::read::respond::FACE_ROW_CEILING;
use contextful_core::AuthorityError;

fn pattern(s: &str) -> TablePattern {
    TablePattern::parse(s).unwrap()
}

fn grant(actions: &[Action], tables: &[&str]) -> Grant {
    Grant {
        actions: actions.to_vec(),
        tables: tables.iter().map(|t| pattern(t)).collect(),
        tenant: None,
        aggregate: None,
        templates: None,
        max_rows: None,
        max_duration_ms: None,
        max_response_bytes: None,
    }
}

fn aggregate() -> AggregateGrant {
    AggregateGrant {
        min_group_size: 10,
        max_contributor_share: 0.5,
        functions: vec!["count".to_string()],
        max_groups: 100,
        max_rows: None,
    }
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// An action outside the vocabulary raises `GrantActionUnknown` at the mint and at admission.
// spec: authority.grant.unknown-action@72f9bc2b
#[test]
fn unknown_action() {
    for (word, action) in [("read", Action::Read), ("write", Action::Write), ("execute", Action::Execute), ("admin", Action::Admin)] {
        assert_eq!(Action::parse(word).unwrap(), action);
    }
    for word in ["delete", "Read", " read", ""] {
        assert!(matches!(Action::parse(word), Err(AuthorityError::GrantActionUnknown(_))), "{word:?}");
    }
    // Decoding a grant, as the mint and admission both do, refuses through the same parser.
    let e = serde_json::from_str::<Grant>(r#"{"actions":["delete"],"tables":["*"]}"#).unwrap_err();
    assert!(e.to_string().contains("GrantActionUnknown"), "{e}");
}

#[test]
fn forget_round_trips_as_a_separate_action_and_confers_no_read() {
    let forget = Action::parse("forget").expect("the explicit erasure action belongs to the vocabulary");
    assert_ne!(forget, Action::Read);
    assert_ne!(forget, Action::Write);
    assert_ne!(forget, Action::Execute);
    assert_ne!(forget, Action::Admin);
    let g = grant(&[forget], &["research/*"]);
    let json = serde_json::to_string(&g).unwrap();
    assert!(json.contains("\"forget\""));
    assert_eq!(serde_json::from_str::<Grant>(&json).unwrap(), g);
    assert!(!raw_read_covers(&[g], "research/filings"));
}

/// A table pattern is `*`, covering every table; a prefix ending in `*`, covering every name beginning with that prefix; or any other string, matched exactly. A concrete pattern never covers `*`.
// spec: authority.grant.pattern-forms@8bb6d690
#[test]
fn pattern_forms() {
    assert_eq!(pattern("*"), TablePattern::All);
    assert_eq!(pattern("research/*"), TablePattern::Prefix("research/".to_string()));
    assert_eq!(pattern("research/filings"), TablePattern::Exact("research/filings".to_string()));

    // The coverage table in Shapes, row by row: research/*, research/filings, *.
    let columns = [pattern("research/*"), pattern("research/filings"), pattern("*")];
    let rows = [
        ("research", [false, false, true]),
        ("research/filings", [true, true, true]),
        ("research/filings/eu", [true, false, true]),
        ("sales/invoices", [false, false, true]),
    ];
    for (name, expected) in rows {
        for (p, want) in columns.iter().zip(expected) {
            assert_eq!(p.covers_name(name), want, "{p:?} over {name}");
        }
    }

    // Pattern over pattern: a concrete pattern never covers `*`.
    assert!(pattern("*").covers(&pattern("*")));
    assert!(pattern("*").covers(&pattern("research/*")));
    assert!(!pattern("research/*").covers(&pattern("*")));
    assert!(!pattern("research/filings").covers(&pattern("*")));
    assert!(pattern("research/*").covers(&pattern("research/filings/*")));
    assert!(pattern("research/*").covers(&pattern("research/filings")));
    assert!(!pattern("research/filings/*").covers(&pattern("research/*")));
    assert!(!pattern("research/filings").covers(&pattern("research/filings/*")));
    assert!(pattern("research/filings").covers(&pattern("research/filings")));
}

/// A pattern with `*` anywhere but its final position raises `GrantPatternMalformed`.
// spec: authority.grant.malformed-pattern@890599b7
#[test]
fn malformed_pattern() {
    for s in ["*research", "re*search", "research/*/eu", "**", "a*b*"] {
        assert!(matches!(TablePattern::parse(s), Err(AuthorityError::GrantPatternMalformed(_))), "{s:?}");
    }
    let e = serde_json::from_str::<Grant>(r#"{"actions":["read"],"tables":["re*search"]}"#).unwrap_err();
    assert!(e.to_string().contains("GrantPatternMalformed"), "{e}");
}

/// A grant names actions, table patterns, and optionally a tenant scope, aggregate constraints, a template allowlist and a row ceiling. An absent constraint leaves its dimension unconstrained; an absent allowlist confers no template.
// spec: authority.grant.fields@732f6077
#[test]
fn fields() {
    let g: Grant = serde_json::from_str(r#"{"actions":["read"],"tables":["research/*"]}"#).unwrap();
    assert_eq!(g, grant(&[Action::Read], &["research/*"]));
    // No tenant: every tenant's rows; no aggregate: raw reads; no row ceiling: only the face ceiling applies.
    assert!(g.tenant.is_none());
    assert!(raw_read_covers(std::slice::from_ref(&g), "research/filings"));
    assert_eq!(least_row_ceiling([g.max_rows, None, None, None]), FACE_ROW_CEILING);
    // No allowlist: no template, even a declared one.
    let declared = strings(&["quarterly_rollup"]);
    assert!(matches!(
        authorize_template(std::slice::from_ref(&g), "quarterly_rollup", &declared),
        Err(AuthorityError::GrantTemplateNotAllowed(_))
    ));
}

/// The grant's tenant value, the partition key on disk and the consumer's tenant identifier are identical bytes, compared by bound-parameter equality with no trimming, case folding, collation or Unicode normalization.
// spec: authority.grant.tenant-bytes@3052ed66
#[test]
fn tenant_bytes() {
    let scope = TenantScope { table: "research/filings".to_string(), value: "acme-é".to_string() };
    assert!(scope.admits("acme-é".as_bytes()));
    assert!(!scope.admits("acme-é ".as_bytes()), "no trimming");
    assert!(!scope.admits("ACME-É".as_bytes()), "no case folding");
    assert!(!scope.admits("acme-e\u{301}".as_bytes()), "no Unicode normalization");
}

/// A tenant-scoped grant on a table declaring no bare outermost partition column raises `GrantTenantUnbindable` at admission.
// spec: authority.grant.tenant-unbindable@97205616
#[test]
fn tenant_unbindable() {
    let scope = TenantScope { table: "research/filings".to_string(), value: "acme-eu".to_string() };
    let bound = scope.bind(Some("org_id")).unwrap();
    assert_eq!((bound.table, bound.column, bound.value), ("research/filings", "org_id", "acme-eu"));

    let e = scope.bind(None).unwrap_err();
    assert!(matches!(&e, AuthorityError::GrantTenantUnbindable(m) if m.contains("research/filings")));
}

/// A tenant-scoped child credential is derived per query with a lifetime of 15 min.
// spec: authority.grant.tenant-child-lifetime@aee8ff46
#[test]
fn tenant_child_lifetime() {
    assert_eq!(TENANT_CHILD_LIFETIME_MINUTES, 15);
    let now = 1_770_000_000;
    assert_eq!(tenant_child_expiry(now, now + 86_400), now + 15 * 60);
    // A parent expiring sooner bounds the child, which never outlives it.
    assert_eq!(tenant_child_expiry(now, now + 60), now + 60);
}

/// A template allowlist denies by default: absent authorizes none, `*` authorizes every template the manifest declares, and a child names only identifiers its parent named or covered.
// spec: authority.grant.template-allowlist@417f09a0
#[test]
fn template_allowlist() {
    let declared = strings(&["quarterly_rollup", "top_filers"]);
    let none = grant(&[Action::Read], &["*"]);
    let star = Grant { templates: Some(strings(&["*"])), ..none.clone() };
    let one = Grant { templates: Some(strings(&["quarterly_rollup"])), ..none.clone() };

    assert!(!none.allows_template("quarterly_rollup", &declared));
    assert!(star.allows_template("quarterly_rollup", &declared));
    assert!(star.allows_template("top_filers", &declared));
    assert!(!star.allows_template("undeclared", &declared), "`*` reaches declared templates only");
    assert!(one.allows_template("quarterly_rollup", &declared));
    assert!(!one.allows_template("top_filers", &declared));

    // Child against parent.
    assert!(one.templates_within(&star));
    assert!(one.templates_within(&one));
    assert!(none.templates_within(&one), "an absent child allowlist names nothing");
    assert!(!star.templates_within(&one));
    assert!(!one.templates_within(&none));
    let other = Grant { templates: Some(strings(&["top_filers"])), ..none.clone() };
    assert!(!other.templates_within(&one));
}

/// Invoking a template outside the allowlist raises `GrantTemplateNotAllowed`, and an uncovered template is absent from the projected tool listing.
// spec: authority.grant.template-not-allowed@6404ada9
#[test]
fn template_not_allowed() {
    let declared = strings(&["quarterly_rollup", "top_filers"]);
    let grants = vec![Grant { templates: Some(strings(&["quarterly_rollup"])), ..grant(&[Action::Read], &["*"]) }];

    assert!(authorize_template(&grants, "quarterly_rollup", &declared).is_ok());
    let e = authorize_template(&grants, "top_filers", &declared).unwrap_err();
    assert!(matches!(&e, AuthorityError::GrantTemplateNotAllowed(m) if m.contains("top_filers")));
    assert_eq!(list_templates(&grants, &declared), vec!["quarterly_rollup"]);
}

/// A pipeline identifier is a read resource: listing it, describing it and tracing its runs each need a read grant covering it. A grant over its landed tables confers none.
// spec: authority.grant.pipeline-resource@236558da
#[test]
fn pipeline_resource() {
    let landed = vec![grant(&[Action::Read], &["research/filings"])];
    assert!(describe_pipeline(&landed, "filings_ingest").is_err(), "the landed table confers nothing");

    let covering = vec![grant(&[Action::Read], &["filings_*"])];
    assert!(describe_pipeline(&covering, "filings_ingest").is_ok());
    assert!(trace_run(&covering, "filings_ingest").is_ok());

    let write_only = vec![grant(&[Action::Write, Action::Execute], &["filings_*"])];
    assert!(describe_pipeline(&write_only, "filings_ingest").is_err());
}

/// Describing an uncovered pipeline raises `GrantPipelineNotCovered`, naming it. Listing filters to covered identifiers and raises it when none is covered; a project declaring no pipelines lists empty.
#[test]
fn pipeline_not_covered() {
    let grants = vec![grant(&[Action::Read], &["filings_*"])];
    let e = describe_pipeline(&grants, "sales_sync").unwrap_err();
    assert!(matches!(&e, AuthorityError::GrantPipelineNotCovered(m) if m.contains("sales_sync")));

    let declared = strings(&["filings_ingest", "sales_sync"]);
    assert_eq!(list_pipelines(&grants, &declared).unwrap(), vec!["filings_ingest"]);

    let none = vec![grant(&[Action::Read], &["research/*"])];
    let e = list_pipelines(&none, &declared).unwrap_err();
    assert!(matches!(&e, AuthorityError::GrantPipelineNotCovered(_)));
    assert!(!e.to_string().contains("sales_sync"), "the empty listing names no declared pipeline");

    assert!(list_pipelines(&none, &[]).unwrap().is_empty());
}

/// Tracing a run gates on the pipeline resolved from the run record and raises `GrantRunTraceDenied` without naming that pipeline.
#[test]
fn run_trace_denied() {
    let grants = vec![grant(&[Action::Read], &["filings_*"])];
    assert!(trace_run(&grants, "filings_ingest").is_ok());
    let e = trace_run(&grants, "payroll_sync").unwrap_err();
    assert!(matches!(e, AuthorityError::GrantRunTraceDenied(_)));
    assert!(!e.to_string().contains("payroll"), "{e}");
}

/// A read's row ceiling is the least of the grant's, the request's, the template's, each touched table's published ceiling and {{read.respond.face-ceiling}}; an undeclared component imposes none, and the face ceiling is always declared.
// spec: authority.grant.row-ceiling@dea6c0fd
#[test]
fn row_ceiling() {
    assert_eq!(least_row_ceiling([Some(5000), None, Some(100), Some(1000)]), 100);
    assert_eq!(least_row_ceiling([None, Some(7), None, None]), 7);
    // The face ceiling is always a component, declared or not.
    assert_eq!(FACE_ROW_CEILING, 10_000);
    assert_eq!(least_row_ceiling([None, None, None, None]), FACE_ROW_CEILING);
    assert_eq!(least_row_ceiling([Some(50_000), Some(u64::MAX), None, Some(20_000)]), FACE_ROW_CEILING);
}

/// An aggregate grant carries a minimum group size, a maximum single-contributor share, permitted functions, a groups-per-query ceiling and a row ceiling. A write-only or aggregate-only grant contributes no table to a raw read.
// spec: authority.grant.aggregate@570046e6
#[test]
fn aggregate_only_grants_contribute_no_raw_table() {
    let raw = grant(&[Action::Read], &["research/*"]);
    assert!(raw_read_covers(std::slice::from_ref(&raw), "research/filings"));

    let aggregate_only = Grant { aggregate: Some(aggregate()), ..raw.clone() };
    assert!(!raw_read_covers(std::slice::from_ref(&aggregate_only), "research/filings"));

    let write_only = grant(&[Action::Write], &["research/*"]);
    assert!(!raw_read_covers(std::slice::from_ref(&write_only), "research/filings"));

    assert!(raw_read_covers(&[aggregate_only, write_only, raw], "research/filings"));
}

/// An aggregate grant's groups-per-query ceiling is at least 1 entries; a query producing more groups than the ceiling is cut at it.
// spec: authority.grant.group-ceiling@f0632e46
#[test]
fn group_ceiling() {
    assert_eq!(AGGREGATE_GROUP_CEILING_FLOOR, 1);
    let three = AggregateGrant { max_groups: 3, ..aggregate() };
    assert_eq!(three.cut_groups(vec![1, 2, 3, 4, 5]), vec![1, 2, 3]);
    assert_eq!(three.cut_groups(vec![1, 2]), vec![1, 2]);

    let zero = AggregateGrant { max_groups: 0, ..aggregate() };
    assert_eq!(zero.group_ceiling(), AGGREGATE_GROUP_CEILING_FLOOR);
    assert_eq!(zero.cut_groups(vec!["a", "b"]), vec!["a"]);
}
