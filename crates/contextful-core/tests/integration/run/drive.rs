//! `run.journal` for the store-driven kind: the input set a read answers, its pins and the
//! labels a row records under.

use contextful_core::run::drive::{row_label, InputSet, StoreInput, Woken};
use contextful_core::run::RunError;
use serde_json::json;
use std::collections::BTreeMap;

fn snapshots() -> BTreeMap<String, String> {
    BTreeMap::from([("documents".to_string(), "5f1c".to_string())])
}

/// An input statement whose response the face truncates at its row ceiling raises `RunInputTruncated` before any
/// row runs.
// spec: run.journal.input-truncated@f1ffa5fc
#[test]
fn a_truncated_input_response_refuses_and_a_whole_one_keys_each_row() {
    let columns = vec!["doc_id".to_string(), "body".to_string()];
    let rows = vec![vec![json!("d1"), json!("alpha")], vec![json!("d2"), json!("beta")]];
    match InputSet::from_response("2030-01-01T00:00:00Z", snapshots(), &columns, rows.clone(), true) {
        Err(RunError::RunInputTruncated(m)) => assert!(m.contains("2 rows") && m.contains("2030-01-01T00:00:00Z"), "{m}"),
        other => panic!("expected RunInputTruncated, got {other:?}"),
    }
    let set = InputSet::from_response("2030-01-01T00:00:00Z", snapshots(), &columns, rows, false).unwrap();
    let keyed = set.keyed();
    assert_eq!(keyed.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), ["0", "1"]);
    assert_eq!(keyed[1].row["body"], json!("beta"));
    assert_eq!(set.bounds().rows, 2);
    assert_eq!(InputSet::decode(&set.encode()).unwrap(), set);
}

#[test]
fn the_plan_reference_moves_with_the_body_statement_and_as_of_and_labels_scope_to_the_row() {
    let base = StoreInput { body: "score".into(), statement: "SELECT 1".into(), as_of: None };
    let moved = [
        StoreInput { body: "rank".into(), ..base.clone() },
        StoreInput { statement: "SELECT 2".into(), ..base.clone() },
        StoreInput { as_of: Some("2030-01-01T00:00:00Z".into()), ..base.clone() },
    ];
    for m in &moved {
        assert_ne!(m.plan_ref(), base.plan_ref());
        assert_ne!(m.pins(), base.pins());
    }
    assert_eq!(base.plan_ref(), base.clone().plan_ref());
    assert_eq!(row_label("7", "model"), "row/7/model");
    assert_ne!(row_label("7", "model"), row_label("17", "model"));
    for w in [Woken::TimedOut, Woken::Resumed(Vec::new()), Woken::Resumed(b"yes".to_vec())] {
        assert_eq!(Woken::decode(&w.encode()).unwrap(), w);
    }
}

#[test]
fn recorded_body_effects_bind_closed_labels_types_transforms_and_exact_owner_scope() {
    use contextful_core::pipeline::transform::TransformOp;
    use contextful_core::run::effect::{EffectScope, RecordedBodyPlan, RecordedEffect};
    use contextful_core::run::journal::EntryKey;
    use contextful_core::store::reconcile::ColumnType;
    let effect = RecordedEffect { label:"model".into(), table:"scores".into(), types:BTreeMap::from([("score".into(), ColumnType::Utf8)]), transforms:Vec::new(), normalize:None };
    let plan = RecordedBodyPlan::compile(vec![effect.clone()], &["scores".into()]).unwrap();
    assert!(plan.effect("undeclared").is_err());
    assert!(RecordedBodyPlan::compile(vec![effect.clone(), effect.clone()], &["scores".into()]).is_err());
    assert!(RecordedBodyPlan::compile(vec![effect.clone()], &["other".into()]).is_err());
    let normalized = RecordedBodyPlan::compile(vec![RecordedEffect { normalize:Some(contextful_core::pipeline::normalize::Normalize { mode:contextful_core::pipeline::normalize::Mode::Relational, depth:3 }), ..effect.clone() }], &["scores".into()]).unwrap();
    assert_ne!(plan.identity(), normalized.identity(), "normalization participates in paid-result replay authority");
    let mut second = normalized.effects().next().unwrap().clone();
    second.label = "second".into();
    assert!(RecordedBodyPlan::compile(vec![effect.clone(), second.clone()], &["scores".into()]).is_err(), "one root cannot land paid results under two normalization projections");
    second.normalize = None;
    assert!(RecordedBodyPlan::compile(vec![effect.clone(), second], &["scores".into()]).is_ok(), "multiple registered labels share one output projection");
    let renamed = RecordedBodyPlan::compile(vec![RecordedEffect { transforms:vec![TransformOp::Rename { from:"score".into(), to:"kept".into() }], ..effect }], &["scores".into()]).unwrap();
    assert_ne!(plan.identity(), renamed.identity(), "typed transforms participate in paid-call replay authority");
    let key = EntryKey::new("body-owner", &row_label("7", "model"), b"private-call-input");
    let scope = EffectScope::new(&key, &plan.identity());
    let wire = serde_json::to_string(&scope).unwrap();
    assert!(!wire.contains("private-call-input"), "scope binds the hash, not raw effect input");
    for changed in [EntryKey::new("other-owner", &key.step_label, b"private-call-input"), EntryKey::new("body-owner", &row_label("8", "model"), b"private-call-input"), EntryKey::new("body-owner", &row_label("7", "other"), b"private-call-input")] {
        assert_ne!(scope, EffectScope::new(&changed, &plan.identity()));
    }
}

#[test]
fn opaque_body_emission_replay_rejects_a_foreign_scope_even_when_an_adapter_accepts_bytes() {
    use contextful_core::run::effect::{EffectAdmission, EffectScope, EmissionSummary, PreparedEmission, RecordedBodyPlan, RecordedEffect};
    use contextful_core::run::journal::EntryKey;
    use contextful_core::run::ports::Types;
    use contextful_core::run::Failure;
    struct Permissive;
    impl EffectAdmission for Permissive {
        fn identity(&self, _: &RecordedEffect) -> Result<String, Failure> { Ok("canonical-test-authority".into()) }
        fn prepare(&self, _: &RecordedEffect, rows:Vec<serde_json::Map<String, serde_json::Value>>, _:Types, _: &EffectScope) -> Result<serde_json::Value, Failure> { Ok(json!(rows)) }
        fn admit(&self, _: &RecordedEffect, _: &EffectScope, _: &serde_json::Value) -> Result<EmissionSummary, Failure> { Ok(EmissionSummary { rows:1, columns:Default::default(), types:Default::default() }) }
    }
    let plan = RecordedBodyPlan::compile(vec![RecordedEffect { label:"model".into(), table:"scores".into(), types:Default::default(), transforms:Vec::new(), normalize:None }], &["scores".into()]).unwrap();
    let scope = EffectScope::new(&EntryKey::new("owner-a", &row_label("0", "model"), b"public"), "composite-plan");
    let emission = PreparedEmission::prepare(&plan, "model", &scope, br#"[{"score":"rewritten"}]"#, &Permissive).unwrap();
    let bytes = emission.encode().unwrap();
    assert!(PreparedEmission::replay(&plan, "model", &scope, &bytes, &Permissive).is_ok());
    let other = EffectScope::new(&EntryKey::new("owner-b", &row_label("0", "model"), b"public"), "composite-plan");
    assert!(PreparedEmission::replay(&plan, "model", &other, &bytes, &Permissive).is_err());
    assert!(PreparedEmission::replay(&plan, "undeclared", &scope, &bytes, &Permissive).is_err());
    assert!(PreparedEmission::replay(&plan, "model", &scope, br#"[{"score":"raw"}]"#, &Permissive).is_err(), "free source bytes supply no body envelope");
    let private = b"[\"malformed-private-model-canary\"]";
    let failure = PreparedEmission::prepare(&plan, "model", &scope, private, &Permissive).err().unwrap();
    assert!(!failure.message.contains("malformed-private-model-canary"), "typed refusal cannot retain a rejected model value: {}", failure.message);
    let prepared = PreparedEmission::prepare(&plan, "model", &scope, br#"[{"score":"rewritten","note":"AKIA0123456789ABCDEF"}]"#, &Permissive).unwrap();
    assert_eq!(prepared.masked_cells().get("note"), Some(&1));
    let guarded = prepared.encode().unwrap();
    assert!(PreparedEmission::replay(&plan, "model", &scope, &guarded, &Permissive).unwrap().masked_cells().is_empty(), "replay neither repeats masking nor reports another paid-call mask");
    let guarded = String::from_utf8(guarded).unwrap();
    assert!(!guarded.contains("AKIA0123456789ABCDEF"), "retained body cells pass the ordinary secret guard before the paid record: {guarded}");
    assert!(guarded.contains(contextful_core::pipeline::guard::MARKER));
}

#[test]
fn registered_prepared_bodies_expose_closed_effects_without_changing_raw_body_calls() {
    use contextful_core::run::drive::{Emitted, InputRow, PreparedEmitted, RecordedRowBody, RecordedRowCalls, RowBody, RowCalls, RowStop};
    use contextful_core::run::effect::RecordedEffect;
    struct Registered;
    impl RecordedRowBody for Registered {
        fn effects(&self) -> Vec<RecordedEffect> { vec![RecordedEffect { label:"model".into(), table:"scores".into(), types:Default::default(), transforms:Vec::new(), normalize:None }] }
        fn run_recorded(&self, _: &InputRow, _: &dyn RecordedRowCalls) -> Result<PreparedEmitted, RowStop> { Ok(Default::default()) }
    }
    impl RowBody for Registered {
        fn run(&self, _: &InputRow, _: &dyn RowCalls) -> Result<Emitted, RowStop> { panic!("prepared dispatch cannot fall back to raw emission") }
        fn recorded(&self) -> Option<&dyn RecordedRowBody> { Some(self) }
    }
    let body: &dyn RowBody = &Registered;
    assert_eq!(body.recorded().unwrap().effects()[0].label, "model");
}
