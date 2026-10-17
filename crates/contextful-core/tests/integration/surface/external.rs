//! `surface.visualize`: `[[external]]` steps hold typed references to declared work, and a
//! name standing alone among externals, jobs and pipelines.

use contextful_core::pipeline::model::{check_blocks, MANIFEST_BLOCKS};
use contextful_core::surface::external::{bind_externals, parse_externals, External, Known, Ref};

fn known() -> Known {
    Known {
        tables: ["ingest_records", "normalize_records", "summary"].into_iter().map(String::from).collect(),
        pipelines: ["ingest", "normalize"].into_iter().map(String::from).collect(),
        jobs: ["summarize"].into_iter().map(String::from).collect(),
    }
}

fn externals(text: &str) -> Vec<External> {
    parse_externals(text).unwrap()
}

/// A manifest holding `[[external]]` passes the top-level block check, and its typed
/// references bind against declared tables, pipelines and jobs.
#[test]
fn an_external_step_binds_typed_references_to_declared_work() {
    assert!(MANIFEST_BLOCKS.contains(&"external"));
    let text = "[[external]]\nname = \"review\"\nlabel = \"Analyst review\"\ninputs = [\"table:normalize_records\", \"job:summarize\"]\noutputs = [\"pipeline:normalize\"]\n";
    check_blocks("manifest.toml", &toml::from_str(text).unwrap()).unwrap();
    let parsed = externals(text);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].label.as_deref(), Some("Analyst review"));
    assert_eq!(parsed[0].input_refs().unwrap(), vec![Ref::Table("normalize_records".into()), Ref::Job("summarize".into())]);
    assert_eq!(parsed[0].output_refs().unwrap(), vec![Ref::Pipeline("normalize".into())]);
    bind_externals(&parsed, &known()).unwrap();
}

// spec: surface.visualize.external-unbound@9df2570b
/// An input or output lacking a typed prefix, or naming nothing declared, raises
/// `ExternalStepUnbound`.
#[test]
fn an_external_reference_naming_nothing_declared_is_unbound() {
    for (reference, why) in [
        ("normalize_records", "untyped"),
        ("view:normalize_records", "unknown kind"),
        ("table:", "empty name"),
        ("table:missing", "undeclared table"),
        ("pipeline:normalize_records", "a table spelled as a pipeline"),
        ("job:ingest", "a pipeline spelled as a job"),
    ] {
        let text = format!("[[external]]\nname = \"review\"\ninputs = [\"{reference}\"]\n");
        let refused = bind_externals(&externals(&text), &known()).expect_err(why).to_string();
        assert!(refused.starts_with("ExternalStepUnbound:"), "{why}: {refused}");
        let text = format!("[[external]]\nname = \"review\"\noutputs = [\"{reference}\"]\n");
        let refused = bind_externals(&externals(&text), &known()).expect_err(why).to_string();
        assert!(refused.starts_with("ExternalStepUnbound:"), "{why}: {refused}");
    }
}

// spec: surface.visualize.external-collision@e41c3bb9
/// A name repeating another external's, a job's or a pipeline's raises `ExternalStepCollision`.
#[test]
fn an_external_name_shared_with_another_step_collides() {
    for (text, why) in [
        ("[[external]]\nname = \"review\"\n[[external]]\nname = \"review\"\n", "a second external"),
        ("[[external]]\nname = \"summarize\"\n", "a job"),
        ("[[external]]\nname = \"ingest\"\n", "a pipeline"),
    ] {
        let refused = bind_externals(&externals(text), &known()).expect_err(why).to_string();
        assert!(refused.starts_with("ExternalStepCollision:"), "{why}: {refused}");
    }
}

// spec: surface.visualize.external-shape@abe25715
/// A kind, command or schedule key refuses: an external step is never runnable.
#[test]
fn an_external_step_carries_no_kind_command_or_schedule() {
    for key in ["kind = \"build\"", "command = \"make\"", "schedule = \"every 1h\"", "body = \"x\""] {
        let text = format!("[[external]]\nname = \"review\"\n{key}\n");
        let refused = parse_externals(&text).expect_err(key).to_string();
        assert!(refused.contains("unknown field"), "{key}: {refused}");
    }
}
