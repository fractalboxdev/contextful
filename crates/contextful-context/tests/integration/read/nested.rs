//! Struct, list and map columns through the read face: a nested value lands, folds and
//! reads back as the JSON it arrived as, and a struct's later field reads null on earlier rows.

use super::*;
use contextful_context::fold::fold;
use contextful_core::store::reconcile::ColumnType;

const NESTED: &str = r#"
[[pipeline.tables]]
name = "lab/spans"
primary_key = ["span_id"]

[[pipeline.tables]]
name = "lab/points"

[[pipeline.tables]]
name = "lab/masked"

[pipeline.tables.policy.columns]
events = { strategy = "drop" }
"#;

fn events() -> ColumnType {
    ColumnType::list(ColumnType::structure([
        ("name", ColumnType::Utf8),
        ("time", ColumnType::Timestamp),
        ("attrs", ColumnType::map(ColumnType::Utf8)),
    ]))
}

fn land_in(
    r: &Reads,
    table: &str,
    run: &str,
    rows: Value,
    types: &[(&str, ColumnType)],
    at_: &str,
) {
    let decl = TableDecl::parse_pipeline(NESTED)
        .unwrap()
        .into_iter()
        .find(|d| d.name == table)
        .unwrap();
    let rows = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_object().unwrap().clone())
        .collect();
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection {
            run_id: run.into(),
            site_id: "site-a".into(),
            batch_seq: Some(0),
            authored_by: None,
            taint: None,
        },
        committed_at: at(at_),
    };
    let types = types
        .iter()
        .map(|(c, t)| (c.to_string(), t.clone()))
        .collect();
    land(&r.store, &decl, &Batch { rows, types }, &ctx).unwrap();
}

fn fold_table(r: &Reads, table: &str, at_: &str) {
    let decl = TableDecl::parse_pipeline(NESTED)
        .unwrap()
        .into_iter()
        .find(|d| d.name == table)
        .unwrap();
    fold(&r.store, &decl, at(at_)).unwrap();
}

fn nested_reads() -> Reads {
    let r = Reads::with_manifest(&format!("{MANIFEST}{NESTED}"));
    land_in(
        &r,
        "lab/spans",
        "run-0001",
        spans(),
        &[("events", events())],
        "2030-01-10T00:00:00Z",
    );
    r
}

fn reopen(r: &mut Reads) {
    r.face = Face::open(r.store.clone(), &format!("{MANIFEST}{NESTED}"), pepper()).unwrap();
}

fn spans() -> Value {
    json!([
        {"span_id": "s1", "events": [
            {"name": "start", "time": "2030-01-09T10:00:00Z", "attrs": {"http.method": "GET", "peer": "a"}},
            {"name": "retry", "time": "2030-01-09T10:00:01.5Z", "attrs": {}},
        ]},
        {"span_id": "s2", "events": [{"name": "end", "time": null, "attrs": null}, null]},
        {"span_id": "s3", "events": []},
        {"span_id": "s4", "events": null},
    ])
}

/// A list is a JSON array, and a struct or a map a JSON object keyed by field name or key text, each item, field or value encoded as its own cell.
// spec: read.respond.nested-values@5aa9305f
#[test]
fn a_nested_column_lands_folds_and_reads_back_as_it_arrived() {
    let mut r = nested_reads();
    let s = r.session(&["lab/*"], None, None);
    // Each struct reads with every declared field, a field the value omitted as null.
    let expected: Vec<Value> = spans()
        .as_array()
        .unwrap()
        .iter()
        .map(|row| match &row["events"] {
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|e| match e {
                        Value::Object(o) => {
                            json!({"name": o["name"], "time": o["time"], "attrs": o["attrs"]})
                        }
                        other => other.clone(),
                    })
                    .collect(),
            ),
            other => other.clone(),
        })
        .collect();
    let read = |r: &Reads| {
        column(
            &r.query(
                &s,
                r#"SELECT span_id, events FROM "lab/spans" ORDER BY span_id"#,
            )
            .unwrap(),
            "events",
        )
    };
    assert_eq!(read(&r), expected);
    assert_eq!(
        r.query(&s, r#"SELECT typeof(events) AS t FROM "lab/spans" LIMIT 1"#).unwrap().rows[0][0],
        json!("STRUCT(\"name\" VARCHAR, \"time\" TIMESTAMP WITH TIME ZONE, attrs MAP(VARCHAR, VARCHAR))[]")
    );
    // A query reaches into the nesting without a join.
    let names = r.query(&s, r#"SELECT span_id, events[1].attrs['peer'] AS peer FROM "lab/spans" WHERE span_id = 's1'"#).unwrap();
    assert_eq!(names.rows[0], [json!("s1"), json!("a")]);

    fold_table(&r, "lab/spans", "2030-01-11T00:00:00Z");
    reopen(&mut r);
    let s = r.session(&["lab/*"], None, None);
    assert_eq!(
        column(
            &r.query(
                &s,
                r#"SELECT span_id, events FROM "lab/spans" ORDER BY span_id"#
            )
            .unwrap(),
            "events"
        ),
        expected
    );
    // The rows tool serializes the same projection.
    let all = r.face.rows(&s, "lab/spans", None).unwrap();
    assert_eq!(column(&all, "events").len(), 4);
    // A caller's own list, struct and map encode the same way.
    let own = r
        .query(
            &s,
            "SELECT ['a', 'b'] AS l, {'k': 1} AS st, MAP {'x': [1.5]} AS m",
        )
        .unwrap();
    assert_eq!(
        own.rows[0],
        [json!(["a", "b"]), json!({"k": 1}), json!({"x": [1.5]})]
    );
}

#[test]
fn a_run_adding_a_struct_field_reads_null_for_it_on_earlier_rows() {
    let mut r = Reads::with_manifest(&format!("{MANIFEST}{NESTED}"));
    let v1 = ColumnType::structure([("service", ColumnType::Utf8)]);
    let v2 = ColumnType::structure([
        ("service", ColumnType::Utf8),
        ("version", ColumnType::Int32),
    ]);
    land_in(
        &r,
        "lab/points",
        "run-0001",
        json!([{"id": "p1", "resource": {"service": "api"}}]),
        &[("resource", v1)],
        "2030-01-10T00:00:00Z",
    );
    land_in(
        &r,
        "lab/points",
        "run-0002",
        json!([{"id": "p2", "resource": {"service": "db", "version": 3}}]),
        &[("resource", v2.clone())],
        "2030-01-10T00:01:00Z",
    );
    assert_eq!(
        r.store
            .schema("lab/points")
            .unwrap()
            .get("resource")
            .unwrap()
            .ty,
        v2
    );
    let expected = [
        json!({"service": "api", "version": null}),
        json!({"service": "db", "version": 3}),
    ];
    for folded in [false, true] {
        if folded {
            fold_table(&r, "lab/points", "2030-01-11T00:00:00Z");
            reopen(&mut r);
        }
        let s = r.session(&["lab/*"], None, None);
        let rows = r
            .query(&s, r#"SELECT id, resource FROM "lab/points" ORDER BY id"#)
            .unwrap();
        assert_eq!(column(&rows, "resource"), expected, "folded: {folded}");
    }
}

#[test]
fn lists_of_int64_and_float64_read_as_float64() {
    let mut r = Reads::with_manifest(&format!("{MANIFEST}{NESTED}"));
    land_in(
        &r,
        "lab/points",
        "run-0001",
        json!([{"id": "p1", "bounds": [1, 2]}]),
        &[("bounds", ColumnType::list(ColumnType::Int64))],
        "2030-01-10T00:00:00Z",
    );
    land_in(
        &r,
        "lab/points",
        "run-0002",
        json!([{"id": "p2", "bounds": [0.5]}]),
        &[("bounds", ColumnType::list(ColumnType::Float64))],
        "2030-01-10T00:01:00Z",
    );
    assert_eq!(
        r.store
            .schema("lab/points")
            .unwrap()
            .get("bounds")
            .unwrap()
            .ty,
        ColumnType::list(ColumnType::Float64)
    );
    for folded in [false, true] {
        if folded {
            fold_table(&r, "lab/points", "2030-01-11T00:00:00Z");
            reopen(&mut r);
        }
        let s = r.session(&["lab/*"], None, None);
        let rows = r
            .query(
                &s,
                r#"SELECT id, bounds, typeof(bounds) AS t FROM "lab/points" ORDER BY id"#,
            )
            .unwrap();
        assert_eq!(
            column(&rows, "bounds"),
            [json!([1.0, 2.0]), json!([0.5])],
            "folded: {folded}"
        );
        assert_eq!(
            column(&rows, "t"),
            [json!("DOUBLE[]"), json!("DOUBLE[]")],
            "folded: {folded}"
        );
    }
}

/// A binary column masks by `drop` or by `hash` over its bytes, a vector or nested column by `drop` alone over the whole column; another strategy on either raises `EnforceStrategyOutsideType` at manifest load.
#[test]
fn a_nested_column_masks_whole_by_drop_alone() {
    let r = Reads::with_manifest(&format!("{MANIFEST}{NESTED}"));
    land_in(
        &r,
        "lab/masked",
        "run-0001",
        spans(),
        &[("events", events())],
        "2030-01-10T00:00:00Z",
    );
    let mut r = r;
    reopen(&mut r);
    let s = r.session(&["lab/*"], None, None);
    let rows = r
        .query(
            &s,
            r#"SELECT span_id, events FROM "lab/masked" ORDER BY span_id"#,
        )
        .unwrap();
    assert_eq!(
        column(&rows, "events"),
        [Value::Null, Value::Null, Value::Null, Value::Null]
    );
    for strategy in ["hash", "tokenize", "truncate:3"] {
        let manifest = format!(
            "{MANIFEST}{}",
            NESTED.replace("strategy = \"drop\"", &format!("strategy = \"{strategy}\""))
        );
        let message = refused_with(
            Face::open(r.store.clone(), &manifest, pepper()).map(|_| ()),
            "EnforceStrategyOutsideType",
        );
        assert!(
            message.contains("lab/masked") && message.contains("List<Struct"),
            "{message}"
        );
    }
}
