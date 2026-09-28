//! `read.guard` templates: declaration, projection and binding.

use super::at;
use contextful_core::read::template::{parse_templates, Bound, ParamType};
use contextful_core::read::ReadError;
use serde_json::{json, Map, Value};

const MANIFEST: &str = r#"
[[query_templates]]
id         = "orders_by_region"
sql        = "SELECT region, sum(total_cents) FROM orders WHERE placed_at >= ? AND region = ? GROUP BY region"
parameters = ["since:timestamp", "region:string"]
max_rows   = 500

[[query_templates]]
id         = "flags"
sql        = "SELECT * FROM orders WHERE priority = ? AND weight > ? AND rush = ?"
parameters = ["priority:integer", "weight:float", "rush:boolean"]
"#;

fn args(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn rejected(r: Result<Vec<Bound>, ReadError>, needle: &str) {
    match r {
        Err(ReadError::TemplateArgumentRejected(why)) => assert!(why.contains(needle), "{why}"),
        other => panic!("expected TemplateArgumentRejected, got {other:?}"),
    }
}

/// A template declares an identifier, one SQL statement, positional parameters written `name:type` over integer, float, string, timestamp and boolean, and an optional row ceiling.
// spec: read.guard.template-declaration@e896b0c1
#[test]
fn a_template_declares_an_id_a_statement_typed_parameters_and_a_ceiling() {
    let t = parse_templates(MANIFEST).unwrap();
    assert_eq!(t[0].id, "orders_by_region");
    assert!(t[0].sql.starts_with("SELECT region"));
    let types: Vec<(&str, ParamType)> = t[0].parameters.iter().map(|p| (p.name.as_str(), p.ty)).collect();
    assert_eq!(types, [("since", ParamType::Timestamp), ("region", ParamType::String)]);
    assert_eq!(t[0].max_rows, Some(500));
    assert_eq!(t[1].max_rows, None);
    assert!(parse_templates("[[query_templates]]\nid = \"x\"\nsql = \"SELECT 1\"\nparameters = [\"n:decimal\"]\n").is_err());
    assert!(parse_templates("[[query_templates]]\nid = \"x\"\nsql = \"SELECT 1\"\nparameters = [\"n\"]\n").is_err());
}

/// A template parameter named `as_of`, `valid_as_of` or `zone` refuses the manifest; those names carry the read's bounds and zone on every template tool.
// spec: read.guard.template-reserved-parameter@dd4b75b1
#[test]
fn a_parameter_named_for_a_read_argument_refuses_the_manifest() {
    for reserved in ["as_of:timestamp", "valid_as_of:timestamp", "zone:string"] {
        let manifest = format!("[[query_templates]]\nid = \"x\"\nsql = \"SELECT 1 WHERE ? IS NOT NULL\"\nparameters = [\"{reserved}\"]\n");
        let refused = parse_templates(&manifest).unwrap_err();
        assert!(refused.0.contains(reserved.split(':').next().unwrap()), "{refused:?}");
    }
    assert!(parse_templates("[[query_templates]]\nid = \"x\"\nsql = \"SELECT 1 WHERE ? IS NOT NULL\"\nparameters = [\"as_of_day:timestamp\"]\n").is_ok());
}

/// Every manifest template projects into a tool named by its identifier, whose declared positional parameters form a typed schema with every field required.
// spec: read.register.template-projection@2fd44db4
#[test]
fn a_template_projects_into_a_tool_with_every_field_required() {
    let t = parse_templates(MANIFEST).unwrap();
    assert_eq!(
        t[0].tool(),
        json!({
            "name": "orders_by_region",
            "inputSchema": {
                "type": "object",
                "properties": { "since": { "type": "string", "format": "date-time" }, "region": { "type": "string" } },
                "required": ["since", "region"],
                "additionalProperties": false,
            },
            "limits": { "max_rows": 500 },
        }),
    );
    assert!(t[1].tool().get("limits").is_none(), "no ceiling declared, none advertised");
}

/// A missing, unknown or type-mismatched argument raises `TemplateArgumentRejected` ahead of execution, with no coercion. Placeholders cover exactly the declared parameters.
// spec: read.guard.template-binding@1c6a418b
#[test]
fn arguments_bind_strictly_by_declared_type() {
    let t = parse_templates(MANIFEST).unwrap();
    assert_eq!(
        t[0].bind(&args(json!({ "region": "emea", "since": "2026-01-01T00:00:00Z" }))).unwrap(),
        [Bound::Timestamp(at("2026-01-01T00:00:00Z")), Bound::Text("emea".into())],
    );
    rejected(t[0].bind(&args(json!({ "region": "emea" }))), "`since` is missing");
    rejected(t[0].bind(&args(json!({ "region": "emea", "since": "2026-01-01T00:00:00Z", "limit": 5 }))), "`limit`");
    rejected(t[0].bind(&args(json!({ "region": "emea", "since": "yesterday" }))), "`since`");
    // No coercion: a numeral string is not an integer, an integral float is not an integer, 1 is not a boolean.
    rejected(t[1].bind(&args(json!({ "priority": "2", "weight": 1.5, "rush": true }))), "`priority`");
    rejected(t[1].bind(&args(json!({ "priority": 2.0, "weight": 1.5, "rush": true }))), "`priority`");
    rejected(t[1].bind(&args(json!({ "priority": 2, "weight": 1.5, "rush": 1 }))), "`rush`");
    assert_eq!(
        t[1].bind(&args(json!({ "priority": 2, "weight": 3, "rush": false }))).unwrap(),
        [Bound::Integer(2), Bound::Float(3.0), Bound::Boolean(false)],
    );
}
