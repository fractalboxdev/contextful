//! Declared patterns select bounded spans and JSON paths select only addressed values.
use contextful_core::redaction::{CompiledRule, Rule};
use serde_json::{json, Value};

fn rule(matcher: Value, path: Option<&str>) -> Rule {
    serde_json::from_value(json!({"table":"messages", "column":"body", "match":matcher,
        "operation":"replace", "argument":"phone", "json_path":path}))
    .unwrap()
}

fn marker(_: &Rule, _: &str) -> Result<Option<String>, contextful_core::enforce::EnforceError> {
    Ok(Some("[REDACTED:phone]".into()))
}

#[test]
fn phone_spans_preserve_unmatched_unicode_and_repeated_neighbors() {
    let r = CompiledRule::compile(rule(json!({"pattern":"[0-9]{3}-[0-9]{3}-[0-9]{4}"}), None)).unwrap();
    let mut value = json!("☎call 415-555-0100415-555-0101 now✓");
    r.rewrite(&mut value, &marker).unwrap();
    assert_eq!(value, "☎call [REDACTED:phone][REDACTED:phone] now✓");
}

#[test]
fn json_paths_rewrite_only_selected_strings_in_canonical_json() {
    let r = CompiledRule::compile(rule(json!("whole"), Some("$.parts[*].text"))).unwrap();
    let mut value = json!("{\"z\":\"public\",\"parts\":[{\"text\":\"secret\",\"kind\":\"prompt\"},{\"text\":null}]}");
    r.rewrite(&mut value, &marker).unwrap();
    assert_eq!(value, "{\"parts\":[{\"kind\":\"prompt\",\"text\":\"[REDACTED:phone]\"},{\"text\":null}],\"z\":\"public\"}");
}

#[test]
fn projected_json_text_retains_the_remaining_source_path() {
    use contextful_core::redaction::ValueStep;
    let r = CompiledRule::compile(rule(json!("whole"), Some("$.parts.text"))).unwrap();
    let mut value = json!("{\"text\":\"secret\",\"public\":\"unchanged\"}");
    r.rewrite_projected(&mut value, &[ValueStep::Key("parts".into())], &marker).unwrap();
    assert_eq!(value, "{\"public\":\"unchanged\",\"text\":\"[REDACTED:phone]\"}");
}

#[test]
fn array_path_indices_use_decimal_digits_without_a_sign() {
    assert!(CompiledRule::compile(rule(json!("whole"), Some("$.parts[+1]"))).is_err());
}

#[test]
fn unsupported_and_empty_patterns_and_malformed_paths_refuse_at_declaration() {
    for pattern in ["(a)\\1", "(?=a)", "a*", "\\b", "["] {
        assert!(CompiledRule::compile(rule(json!({"pattern":pattern}), None)).is_err(), "{pattern}");
    }
    for path in ["$.parts[", "$..text", "$.parts[?(@.secret)]", "text"] {
        assert!(CompiledRule::compile(rule(json!("whole"), Some(path))).is_err(), "{path}");
    }
}

#[test]
fn greedy_quantifiers_rewrite_one_leftmost_first_span() {
    let r = CompiledRule::compile(rule(json!({"pattern":"a+"}), None)).unwrap();
    let mut value = json!("aaa b aa");
    r.rewrite(&mut value, &marker).unwrap();
    assert_eq!(value, "[REDACTED:phone] b [REDACTED:phone]");
}

#[test]
fn a_rewritten_span_leaves_no_matched_byte_of_its_leftmost_match() {
    for (pattern, text) in [("password=\\S+", "password=hunter2 ok"), ("ab+c|b", "xabbbcx")] {
        let r = CompiledRule::compile(rule(json!({"pattern":pattern}), None)).unwrap();
        let mut value = json!(text);
        r.rewrite(&mut value, &marker).unwrap();
        let rewritten = value.as_str().unwrap();
        assert!(!rewritten.contains("hunter") && !rewritten.contains("unter2") && !rewritten.contains("abb") && !rewritten.contains("bc"), "{pattern}: {rewritten}");
    }
}

#[test]
fn unicode_word_boundaries_refuse_at_declaration() {
    assert!(CompiledRule::compile(rule(json!({"pattern":"\\bkey\\w+"}), None)).is_err());
    assert!(CompiledRule::compile(rule(json!({"pattern":"(?-u:\\b)key\\w+"}), None)).is_ok());
}

#[test]
fn adversarial_alternation_rewrites_every_matched_byte_of_a_mebibyte_in_one_pass() {
    let r = CompiledRule::compile(rule(json!({"pattern":".*[^a]|a"}), None)).unwrap();
    let mut value = json!("a".repeat(1024 * 1024));
    let start = std::time::Instant::now();
    r.rewrite(&mut value, &|_, _| Ok(Some(String::new()))).unwrap();
    assert_eq!(value, "");
    assert!(start.elapsed() < std::time::Duration::from_secs(10), "{:?}", start.elapsed());
}

#[test]
fn overlapping_matches_rewrite_as_one_span_and_adjacent_matches_stay_separate() {
    let r = CompiledRule::compile(rule(json!({"pattern":"key=[a-z]+|[a-z]+=[0-9]+"}), None)).unwrap();
    let mut value = json!("key=abc=123 tail");
    r.rewrite(&mut value, &marker).unwrap();
    assert_eq!(value, "[REDACTED:phone] tail");
}

#[test]
fn ordinary_patterns_rewrite_a_mebibyte_in_one_pass() {
    let r = CompiledRule::compile(rule(json!({"pattern":"[0-9]{3}-[0-9]{3}-[0-9]{4}"}), None)).unwrap();
    let mut value = json!("call 415-555-0100 now ".repeat(48 * 1024));
    r.rewrite(&mut value, &marker).unwrap();
    assert!(!value.as_str().unwrap().contains("415"));
}

#[test]
fn pipeline_removal_rejects_unknown_tables_and_bounds_all_declared_rules() {
    use contextful_core::pipeline::declare::PipelineSpec;
    let declared = |rules: Value| -> PipelineSpec {
        serde_json::from_value(json!({"id":"feed", "source":{"name":"http"},
            "tables":["messages"], "journal":false, "redaction":rules}))
        .unwrap()
    };
    let mut unknown = serde_json::to_value(rule(json!("whole"), None)).unwrap();
    unknown["table"] = json!("missing");
    assert!(declared(json!([unknown])).validate().is_err(), "an unbound rule cannot disappear");
    let valid = serde_json::to_value(rule(json!("whole"), None)).unwrap();
    assert!(declared(json!(vec![valid.clone(); 256])).validate().is_ok());
    assert!(declared(json!(vec![valid; 257])).validate().is_err(), "the pipeline owns the aggregate bound");
}

#[test]
fn relational_lineage_rewrites_children_before_deriving_persisted_identities() {
    use contextful_core::pipeline::normalize::NormalizedGroup;
    let normalize = |secret: &str| {
        let row = json!({"body":{"parts":[{"text":secret,"public":"keep"}]},"public":"root"});
        let mut group = NormalizedGroup::new(vec![row.as_object().unwrap().clone()], "messages", "run-a", 5);
        let r = CompiledRule::compile(rule(json!("whole"), Some("$.parts[*].text"))).unwrap();
        let mut seen = Vec::new();
        group
            .rewrite_cells(&mut |cell, value| {
                if cell.source_column == "body" {
                    seen.push((cell.table.clone(), cell.column.clone()));
                    r.rewrite_projected(value, &cell.path, &marker)?;
                }
                Ok(())
            })
            .unwrap();
        assert!(seen.contains(&("messages_body_parts".into(), "text".into())));
        let tables = group.into_tables();
        assert_eq!(tables["messages_body_parts"][0]["text"], "[REDACTED:phone]");
        assert_eq!(tables["messages_body_parts"][0]["public"], "keep");
        tables
    };
    assert_eq!(normalize("415-555-0100"), normalize("415-555-0999"), "no identity retains a digest of either removed value");
}

#[test]
fn source_and_expanded_automaton_limits_refuse_before_matching() {
    assert!(CompiledRule::compile(rule(json!({"pattern":"a".repeat(4097)}), None)).is_err());
    assert!(CompiledRule::compile(rule(json!({"pattern":"a{100000}"}), None)).is_err());
}

#[test]
fn an_index_cannot_derive_a_copy_from_a_removed_column() {
    use contextful_core::store::declare::TableDecl;
    use contextful_core::store::StoreError;
    let mut removal = serde_json::to_value(rule(json!("whole"), None)).unwrap();
    removal["operation"] = json!("drop");
    removal.as_object_mut().unwrap().remove("argument");
    let decl: TableDecl = serde_json::from_value(json!({"name":"messages", "primary_key":["id"],
        "redaction":[removal], "indexes":[{"kind":"fulltext","column":"body"}]}))
    .unwrap();
    assert!(matches!(decl.validate_index_declaration(), Err(StoreError::StoreIndexOverRedactedColumn(_))));
}

#[test]
fn typed_pinned_plans_bind_the_real_rule_and_journal_decision() {
    use contextful_core::run::plan::Plan;
    use contextful_core::run::RunError;
    let declaration = r#"
pipeline = "feed"
table = "messages"
redaction = [{table="messages",column="body",match="whole",operation="replace",argument="phone"}]
[connector]
id="vendor"
version="1"
command=["vendor"]
"#;
    assert!(Plan::compile(declaration.as_bytes()).unwrap().spec.journal, "typed rules reach mandatory destination preparation admission");
    let plan = Plan::compile(format!("journal=false\n{declaration}").as_bytes()).unwrap();
    assert_eq!(plan.spec.redaction[0].column, "body");
    assert!(plan.spec.redact.is_empty(), "typed plans carry no sentinel column");
    let mismatched = declaration.replace("table=\"messages\"", "table=\"another\"");
    assert!(matches!(Plan::compile(format!("journal=false\n{mismatched}").as_bytes()), Err(RunError::Invalid(_))));
}
