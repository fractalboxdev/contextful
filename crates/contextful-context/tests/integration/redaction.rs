//! Declared removal rules bind the writer even when its caller supplies a bare table.

use arrow_array::Array;
use contextful_context::land::{commit_parts, land_batches, stage_part, Batch, Position, RunContext};
use contextful_context::Store;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reserve::Injection;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json::json;

fn declared() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("contextful.toml"),
        r#"
[project]
name = "research"
[[pipeline.tables]]
name = "messages"
redaction = [{ table = "messages", column = "body", match = "whole", operation = "replace", argument = "phone" }]
"#,
    )
    .unwrap();
    let store = Store::open(dir.path(), "research").unwrap();
    (dir, store)
}

#[test]
// spec: authority.redact.effect-scope@a6bc8d71
fn effect_recordings_require_the_signed_owner_key_and_cannot_upgrade_source_payloads() {
    use contextful_core::run::effect::EffectScope;
    use contextful_core::run::journal::EntryKey;
    let (_dir, store) = declared();
    let batch = Batch { rows:vec![json!({"body":"private-model-result"}).as_object().unwrap().clone()], types:Default::default() };
    let key = EntryKey::new("job-owner", "row/7/model", b"private-model-input");
    let scope = EffectScope::new(&key, "canonical-body-plan");
    let prepared = store.prepare_effect_recording("messages", &batch, None, "job-owner", &scope).unwrap();
    assert_eq!(prepared.summary().unwrap().rows, 1, "summary belongs to the admitted rewritten root, not caller metadata");
    let bytes = prepared.encode().unwrap();
    assert!(!bytes.is_empty(), "the exclusion below ranges over no element");
    assert!(!bytes.windows(b"private-model-result".len()).any(|value| value == b"private-model-result"));
    assert!(!bytes.is_empty(), "the exclusion below ranges over no element");
    assert!(!bytes.windows(b"private-model-input".len()).any(|value| value == b"private-model-input"));
    assert!(store.admit_effect_recording("messages", &bytes, None, &scope).is_ok());
    for other in [
        EffectScope::new(&EntryKey::new("other-job", &key.step_label, b"private-model-input"), "canonical-body-plan"),
        EffectScope::new(&EntryKey::new("job-owner", "row/8/model", b"private-model-input"), "canonical-body-plan"),
        EffectScope::new(&EntryKey::new("job-owner", "row/7/other", b"private-model-input"), "canonical-body-plan"),
        EffectScope::new(&key, "changed-body-plan"),
    ] { assert!(store.admit_effect_recording("messages", &bytes, None, &other).is_err()); }
    assert!(store.admit_recording("messages", &bytes, None).is_err(), "a body handle cannot enter the ordinary source admission path");
    let source = store.prepare_recording("messages", &batch, None, "source-owner").unwrap().encode().unwrap();
    assert!(store.admit_effect_recording("messages", &source, None, &scope).is_err(), "free valid source bytes cannot acquire body authority");
    assert!(store.admit_recording("messages", &source, None).is_ok(), "ordinary prepared source replay remains compatible");
    let ctx = context();
    assert!(contextful_context::land::stage_recorded_group(&store, &TableDecl::named("messages"), &prepared, &ctx.node, &ctx.injection, 0, &Default::default()).is_err(), "body bytes remain outside ordinary source staging");
    let wrong = EffectScope::new(&EntryKey::new("other-job", &key.step_label, b"private-model-input"), "canonical-body-plan");
    assert!(contextful_context::land::stage_effect_recorded_group(&store, &TableDecl::named("messages"), contextful_context::land::ScopedRecording { prepared:&prepared, scope:&wrong }, &ctx.node, &ctx.injection, 0, &Default::default()).is_err());
    let parts = contextful_context::land::stage_effect_recorded_group(&store, &TableDecl::named("messages"), contextful_context::land::ScopedRecording { prepared:&prepared, scope:&scope }, &ctx.node, &ctx.injection, 0, &Default::default()).unwrap();
    assert_eq!(parts["messages"].rows, 1);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("contextful.toml"), "[project]\nname='research'\n[[pipeline.tables]]\nname='messages'\n").unwrap();
    let plain = Store::open(dir.path(), "research").unwrap();
    assert!(plain.recording_identity("messages", None).unwrap().is_none(), "ordinary unprotected source recording remains verbatim");
    assert!(plain.prepare_recording("messages", &batch, None, "source-owner").is_err(), "body support does not widen the prepared source interface");
    let prepared = plain.prepare_effect_recording("messages", &batch, None, "job-owner", &scope).expect("a declared ruleless root shares canonical body admission in mixed-output plans");
    let bytes = prepared.encode().unwrap();
    assert!(plain.admit_effect_recording("messages", &bytes, None, &scope).is_ok());
    assert!(plain.admit_recording("messages", &bytes, None).is_err());
    assert!(plain.prepare_effect_recording("undeclared", &batch, None, "job-owner", &scope).is_err(), "body recording never admits an undeclared root");
}

fn composed_declarations(ordinary: &str, producer: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("contextful.toml"), format!("[project]\nname = 'research'\n[[pipeline.tables]]\nname = 'feed_messages'\n{ordinary}\n")).unwrap();
    std::fs::create_dir(dir.path().join("pipelines")).unwrap();
    std::fs::write(dir.path().join("pipelines/feed.toml"), format!(
        "id = 'feed'\ntables = ['messages']\njournal = false\nnormalize = {{ mode = 'relational' }}\n{producer}\n[source]\nname = 'http'\nconfig = {{ endpoint = 'http://example.test/v1/{{table}}' }}\n"
    )).unwrap();
    dir
}

#[test]
fn composed_destinations_keep_static_removal_and_add_producer_rules() {
    let static_rule = "redaction = [{ table = 'feed_messages', column = 'body', match = 'whole', operation = 'replace', argument = 'phone' }]";
    let producer_rule = "redaction = [{ table = 'messages', column = 'public', match = 'whole', operation = 'replace', argument = 'email' }]";
    for (ordinary, producer, protected, additional) in [("retain_runs = '0s'", "", false, false), (static_rule, "", true, false), (static_rule, producer_rule, true, true)] {
        let dir = composed_declarations(ordinary, producer);
        let store = Store::open(dir.path(), "research").unwrap();
        let manifest = land_batches(&store, &TableDecl::named("feed_messages"), &[input()], &context(), &Position::default(), &|| Ok(())).unwrap();
        let path = store.table_dir("feed_messages").unwrap().join("data/runs/run-a/ingest-a").join(&manifest.parts[0].name);
        assert_eq!(body(&path), if protected { "[REDACTED:phone]" } else { "415-555-0100" });
        if protected {
            assert!(store.validate_writer_recording("feed_messages", None).is_err());
            assert!(stage_part(&store, &TableDecl::named("undeclared"), &input(), &context().node, &context().injection, 0, 0).is_err());
        }
        if additional {
            let batch = ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(&path).unwrap()).unwrap().build().unwrap().next().unwrap().unwrap();
            assert_eq!(batch.column_by_name("public").unwrap().as_any().downcast_ref::<arrow_array::StringArray>().unwrap().value(0), "[REDACTED:email]");
            let plan = contextful_core::run::plan::Plan::compile(b"pipeline='feed'\ntable='feed_messages'\njournal=false\nredaction=[{table='feed_messages',column='public',match='whole',operation='replace',argument='email'}]\n[connector]\nid='feed'\nversion='1'\ncommand=['unused']\n").unwrap();
            assert!(store.validate_writer_plan(&plan, None).unwrap_err().to_string().contains("complete canonical"));
        }
    }
}

#[test]
fn composed_destinations_refuse_conflicting_types_combined_overflow_and_duplicate_rules() {
    let dir = composed_declarations("columns = { body = 'binary' }", "tables = [{ name = 'messages', columns = { body = 'utf8' } }]");
    // Replace the helper's bare producer list with its typed declaration.
    let path = dir.path().join("pipelines/feed.toml");
    let text = std::fs::read_to_string(&path).unwrap().replacen("tables = ['messages']\n", "", 1);
    std::fs::write(&path, text).unwrap();
    assert!(Store::open(dir.path(), "research").unwrap_err().to_string().contains("conflicting declared type"));
    let rules = (0..256).map(|i| format!("{{ table = 'feed_messages', column = 'c{i}', match = 'whole', operation = 'drop' }}")).collect::<Vec<_>>().join(",");
    let dir = composed_declarations(&format!("redaction = [{rules}]"), "redaction = [{ table = 'messages', column = 'extra', match = 'whole', operation = 'drop' }]");
    let refusal = Store::open(dir.path(), "research").unwrap_err().to_string();
    assert!(refusal.contains("combined removal rule count"), "{refusal}");
    let dir = composed_declarations("redaction = [{ table = 'feed_messages', column = 'body', match = 'whole', operation = 'drop' }]", "redaction = [{ table = 'messages', column = 'body', match = 'whole', operation = 'drop' }]");
    assert!(Store::open(dir.path(), "research").unwrap_err().to_string().contains("duplicate removal rule"));
}

#[test]
fn composed_destinations_refuse_conflicting_class_and_policy_authority() {
    for (ordinary, producer, expected) in [
        ("class = 'phone'", "tables = [{ name = 'messages', class = 'email' }]", "conflicting writer class"),
        ("policy = { columns = { body = { strategy = 'drop' } } }", "tables = [{ name = 'messages', policy = { columns = { body = { strategy = 'hash' } } } }]", "conflicting writer policy"),
    ] {
        let dir = composed_declarations(ordinary, producer);
        let path = dir.path().join("pipelines/feed.toml");
        let text = std::fs::read_to_string(&path).unwrap().replacen("tables = ['messages']\n", "", 1);
        std::fs::write(&path, text).unwrap();
        let refusal = Store::open(dir.path(), "research").unwrap_err().to_string();
        assert!(refusal.contains(expected), "{refusal}");
        assert!(!dir.path().join(".contextful/context/research/tables").exists());
    }
}

#[test]
fn a_producer_removal_cannot_leave_a_static_index_over_its_source_column() {
    let dir = composed_declarations("primary_key = ['id']\nindexes = [{ kind = 'fulltext', column = 'body' }]", "redaction = [{ table = 'messages', column = 'body', match = 'whole', operation = 'drop' }]");
    let refusal = Store::open(dir.path(), "research").unwrap_err().to_string();
    assert!(refusal.contains("StoreIndexOverRedactedColumn"), "{refusal}");
    assert!(!dir.path().join(".contextful/context/research/tables").exists());
}

fn input() -> Batch {
    Batch { rows: vec![json!({"body": "415-555-0100", "public": "unchanged"}).as_object().unwrap().clone()], types: Default::default() }
}

#[test]
fn sparse_removed_cells_retain_nullable_batch_semantics_without_inventing_columns() {
    let (_dir, store) = declared();
    let batch = |missing: bool| Batch {
        rows: vec![
            json!({"body":"415-555-0100", "public":"first"}).as_object().unwrap().clone(),
            if missing { json!({"public":"second"}) } else { json!({"body":null,"public":"second"}) }.as_object().unwrap().clone(),
        ],
        types: Default::default(),
    };
    let encode = |batch: &Batch| {
        let part = stage_part(&store, &TableDecl::named("messages"), batch, &context().node, &context().injection, 0, 0).unwrap();
        let path = store.table_dir("messages").unwrap().join("data/runs/run-a/ingest-a/stage.staging").join(part.name);
        let reader = ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(&path).unwrap()).unwrap().build().unwrap();
        let data = reader.map(|batch| batch.unwrap()).collect::<Vec<_>>();
        let values = data[0].column_by_name("body").unwrap();
        assert!(values.is_null(1));
        assert_eq!(values.as_any().downcast_ref::<arrow_array::StringArray>().unwrap().value(0), "[REDACTED:phone]");
        std::fs::read(path).unwrap()
    };
    let explicit = encode(&batch(false));
    assert_eq!(encode(&batch(true)), explicit, "missing and explicit null cells share the same encoded nullable column");
    let unknown = Batch { rows: vec![json!({"public":"only"}).as_object().unwrap().clone()], types: Default::default() };
    let refusal = stage_part(&store, &TableDecl::named("messages"), &unknown, &context().node, &context().injection, 1, 2).unwrap_err().to_string();
    assert!(refusal.contains("removal column `body` is absent"), "{refusal}");
}

#[test]
fn canonical_prepared_recording_admits_only_its_rewritten_payload_and_current_authority() {
    let (_dir, store) = declared();
    let prepared = store.prepare_recording("messages", &input(), None, "execution-a").unwrap();
    let encoded = prepared.encode().unwrap();
    assert!(!encoded.is_empty(), "the exclusion below ranges over no element");
    assert!(!encoded.windows(b"415-555-0100".len()).any(|bytes| bytes == b"415-555-0100"));
    assert!(String::from_utf8_lossy(&encoded).contains("[REDACTED:phone]"));
    let admitted = store.admit_recording("messages", &encoded, None).unwrap();
    let part = contextful_context::land::stage_recorded_part(&store, &TableDecl::named("messages"), &admitted, &context().node, &context().injection, 0, 0).unwrap();
    let path = store.table_dir("messages").unwrap().join("data/runs/run-a/ingest-a/stage.staging").join(part.name);
    assert_eq!(body(&path), "[REDACTED:phone]");
    let mut forged: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    forged["tables"]["messages"]["rows"][0]["body"] = json!("415-555-0100");
    assert!(store.admit_recording("messages", &serde_json::to_vec(&forged).unwrap(), None).is_err());
    let (other_dir, _) = declared();
    let path = other_dir.path().join("contextful.toml");
    let changed = std::fs::read_to_string(&path).unwrap().replace("argument = \"phone\"", "argument = \"email\"");
    std::fs::write(&path, changed).unwrap();
    let changed = Store::open(other_dir.path(), "research").unwrap();
    assert!(changed.admit_recording("messages", &encoded, None).is_err());
    assert!(store.admit_recording("other", &encoded, None).is_err());
    for ty in ["binary", "not-a-column-type"] {
        let mut forged: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        forged["tables"]["messages"]["types"]["body"] = json!(ty);
        assert!(store.admit_recording("messages", &serde_json::to_vec(&forged).unwrap(), None).is_err());
    }
}

#[test]
fn relational_prepared_recording_keeps_typed_root_columns_and_concrete_child_identities() {
    let (_dir, store) = declared();
    let mut batch = input();
    batch.rows[0].insert("public".into(), json!("YWJj"));
    batch.rows[0].insert("items".into(), json!([{ "score":1 }]));
    batch.types.insert("public".into(), contextful_core::store::reconcile::ColumnType::Binary);
    let normalize = contextful_core::pipeline::normalize::Normalize { mode:contextful_core::pipeline::normalize::Mode::Relational, depth:5 };
    let prepared = store.prepare_recording("messages", &batch, Some(normalize), "execution-a").unwrap();
    let parts = contextful_context::land::stage_recorded_group(&store, &TableDecl::named("messages"), &prepared, &context().node, &context().injection, 0, &Default::default()).unwrap();
    let root = &parts["messages"];
    let path = store.table_dir("messages").unwrap().join("data/runs/run-a/ingest-a/stage.staging").join(&root.name);
    let loaded = ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(path).unwrap()).unwrap().build().unwrap().next().unwrap().unwrap();
    let values = loaded.column_by_name("public").unwrap().as_any().downcast_ref::<arrow_array::BinaryArray>().expect("the root keeps the pull's Binary declaration");
    assert_eq!(values.value(0), b"abc");
    assert!(parts.contains_key("messages_items"));
    let encoded = prepared.encode().unwrap();
    let resumed = store.admit_recording("messages", &encoded, Some(normalize)).unwrap();
    assert_eq!(resumed.encode().unwrap(), encoded, "replay admits exactly the rewritten group identities");
    assert!(store.admit_recording("messages", &encoded, None).is_err());
    assert!(store.admit_recording("messages", &encoded, Some(contextful_core::pipeline::normalize::Normalize { depth:4, ..normalize })).is_err());
}

#[test]
fn a_protected_clock_refuses_before_recording_while_an_independent_clock_remains_admitted() {
    let (_dir, store) = declared();
    let plan = |table: &str, field: &str| contextful_core::run::plan::Plan::compile(format!(
        "pipeline='feed'\ntable='{table}'\njournal=true\n[connector]\nid='feed'\nversion='1'\ncommand=['unused']\n[cursor]\nkind='monotonic'\nfield='{field}'\n"
    ).as_bytes()).unwrap();
    assert!(store.validate_source_plan(&plan("messages", "body"), None).unwrap_err().to_string().contains("safe typed progress lineage"));
    store.validate_source_plan(&plan("messages", "clock"), None).unwrap();
    store.validate_source_plan(&plan("independent", "body"), None).unwrap();
}

#[test]
fn protected_continuation_accepts_no_bytes_or_exact_terminal_form_and_refuses_hidden_values() {
    let (_dir, store) = declared();
    let pull = |cursor: serde_json::Value, more: bool| contextful_core::run::ports::Pull::decode(&serde_json::to_vec(&json!({"rows":[],"cursor":cursor,"more":more})).unwrap()).unwrap();
    for more in [false, true] { store.validate_recorded_control("messages", &pull(json!(null), more)).unwrap(); }
    store.validate_recorded_control("messages", &pull(json!({"next":null}), false)).unwrap();
    for (cursor, more) in [(json!({"next":null}), true), (json!({"next":null,"private":"415-555-0100"}), false), (json!({"next":"415-555-0100"}), false), (json!({"415-555-0100":null}), false), (json!("415-555-0100"), false)] {
        assert!(store.validate_recorded_control("messages", &pull(cursor, more)).is_err());
    }
}

#[test]
fn prepared_relational_replay_retains_no_removed_child_value_or_pre_rewrite_identity() {
    let mut prepared = Vec::new();
    for secret in ["415-555-0100", "415-555-0999"] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("contextful.toml"), "[[pipeline.tables]]\nname='messages'\n[[pipeline.tables]]\nname='messages_body'\nredaction=[{table='messages_body',column='text',match='whole',operation='replace',argument='phone'}]\n").unwrap();
        let store = Store::open(dir.path(), "research").unwrap();
        let batch = Batch { rows:vec![json!({"body":[{"text":secret}],"public":"keep"}).as_object().unwrap().clone()], types:Default::default() };
        let normalize = contextful_core::pipeline::normalize::Normalize { mode:contextful_core::pipeline::normalize::Mode::Relational, depth:5 };
        let encoded = store.prepare_recording("messages", &batch, Some(normalize), "same-execution").unwrap().encode().unwrap();
        assert!(!encoded.is_empty(), "the exclusion below ranges over no element");
        assert!(!encoded.windows(secret.len()).any(|bytes| bytes == secret.as_bytes()));
        let admitted = store.admit_recording("messages", &encoded, Some(normalize)).unwrap();
        let parts = contextful_context::land::stage_recorded_group(&store, &TableDecl::named("messages"), &admitted, &context().node, &context().injection, 0, &Default::default()).unwrap();
        let bytes = parts.into_iter().map(|(table, part)| {
            let path = store.table_dir(&table).unwrap().join("data/runs/run-a/ingest-a/stage.staging").join(part.name);
            (table, std::fs::read(path).unwrap())
        }).collect::<std::collections::BTreeMap<_, _>>();
        prepared.push((encoded, bytes));
    }
    assert_eq!(prepared[0], prepared[1], "only rewritten root, parent and child identities enter prepared recording and replay");
}

#[test]
fn canonical_recording_admission_binds_bare_plans_and_preserves_independent_destinations() {
    let (dir, store) = declared();
    let plan = |table: &str, journal: bool| {
        contextful_core::run::plan::Plan::compile(format!(
            "pipeline = 'feed'\ntable = '{table}'\njournal = {journal}\n[connector]\nid = 'feed'\nversion = '1'\ncommand = ['unused']\n"
        ).as_bytes()).unwrap()
    };
    let refusal = store.validate_writer_plan(&plan("messages", true), None).unwrap_err().to_string();
    assert!(refusal.contains("JournalRedactionConflict"), "{refusal}");
    store.validate_writer_plan(&plan("messages", false), None).unwrap();
    store.validate_writer_plan(&plan("independent", true), None).unwrap();
    assert!(store.validate_writer_recording("messages", None).is_err());
    store.validate_writer_recording("independent", None).unwrap();
    assert!(!dir.path().join("machine.sqlite").exists());
    assert!(!store.table_dir("messages").unwrap().join("data").exists());
}

fn context() -> RunContext {
    RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: "run-a".into(), site_id: "site-a".into(), batch_seq: None, authored_by: None, taint: None },
        committed_at: crate::support::at("2030-01-01T00:01:00Z"),
    }
}

fn body(path: &std::path::Path) -> String {
    let mut reader = ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(path).unwrap()).unwrap().build().unwrap();
    let batch = reader.next().unwrap().unwrap();
    assert_eq!(batch.num_rows(), 1, "the canary row exists before asserting removal");
    let column = batch.column_by_name("body").unwrap();
    assert!(!column.is_null(0));
    column.as_any().downcast_ref::<arrow_array::StringArray>().unwrap().value(0).into()
}

#[test]
fn a_bare_caller_declaration_cannot_omit_the_durable_writer_rule() {
    let (_dir, store) = declared();
    let table = TableDecl::named("messages");
    let ctx = context();
    let position = Position { pipeline_id: Some("feed".into()), ..Position::default() };
    let manifest = land_batches(&store, &table, &[input()], &ctx, &position, &|| Ok(())).unwrap();
    assert_eq!(manifest.parts.len(), 1);
    let path = store.table_dir("messages").unwrap().join("data/runs/run-a/ingest-a").join(&manifest.parts[0].name);
    assert_eq!(body(&path), "[REDACTED:phone]");
}

#[test]
fn a_staged_part_contains_the_same_removed_form_before_commit() {
    let (_direct_dir, direct) = declared();
    let (_staged_dir, staged) = declared();
    let table = TableDecl::named("messages");
    let ctx = context();
    let position = Position { pipeline_id: Some("feed".into()), ..Position::default() };
    let part = stage_part(&staged, &table, &input(), &ctx.node, &ctx.injection, 0, 0).unwrap();
    let run_dir = staged.table_dir("messages").unwrap().join("data/runs/run-a/ingest-a");
    assert_eq!(body(&run_dir.join("stage.staging").join(&part.name)), "[REDACTED:phone]");
    let staged_manifest = commit_parts(&staged, &table, &[part.name], &ctx, &position, &|| Ok(()), &|_| Ok(())).unwrap();
    let direct_manifest = land_batches(&direct, &table, &[input()], &ctx, &position, &|| Ok(())).unwrap();
    let staged_path = run_dir.join(&staged_manifest.parts[0].name);
    let direct_path = direct.table_dir("messages").unwrap().join("data/runs/run-a/ingest-a").join(&direct_manifest.parts[0].name);
    assert_eq!(body(&staged_path), body(&direct_path));
    assert_eq!(std::fs::read(staged_path).unwrap(), std::fs::read(direct_path).unwrap());
}

#[test]
fn replay_compares_the_removed_form_and_refuses_changed_public_data() {
    let (_dir, store) = declared();
    let table = TableDecl::named("messages");
    let ctx = context();
    let position = Position { pipeline_id: Some("feed".into()), ..Position::default() };
    let first = land_batches(&store, &table, &[input()], &ctx, &position, &|| Ok(())).unwrap();
    let mut equivalent = input();
    equivalent.rows[0]["body"] = json!("415-555-0999");
    let replay = land_batches(&store, &table, &[equivalent.clone()], &ctx, &position, &|| Ok(())).unwrap();
    assert_eq!(first.parts, replay.parts);
    equivalent.rows[0]["public"] = json!("changed");
    assert!(land_batches(&store, &table, &[equivalent], &ctx, &position, &|| Ok(())).is_err());
}

#[test]
fn relational_staging_removes_selected_child_cells_and_raw_derived_ids() {
    use contextful_context::land::{stage_normalized_group, NormalizedStage};
    use contextful_core::pipeline::normalize::NormalizedGroup;
    let (_dir, store) = declared();
    // A whole-column rule applies to the exact descendants carried by normalization.
    let row = json!({"body":[{"text":"415-555-0100"}],"public":"unchanged"});
    let group = NormalizedGroup::new(vec![row.as_object().unwrap().clone()], "messages", "run-a", 5);
    let ctx = context();
    let parts = stage_normalized_group(
        &store,
        &TableDecl::named("messages"),
        NormalizedStage { group, ordinal: 0, offsets: Default::default() },
        &ctx.node,
        &ctx.injection,
    )
    .unwrap();
    assert_eq!(parts.len(), 2);
    let child = store.table_dir("messages_body").unwrap().join("data/runs/run-a/ingest-a/stage.staging").join(&parts["messages_body"].name);
    let mut reader = ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(child).unwrap()).unwrap().build().unwrap();
    let batch = reader.next().unwrap().unwrap();
    assert_eq!(batch.column_by_name("text").unwrap().as_any().downcast_ref::<arrow_array::StringArray>().unwrap().value(0), "[REDACTED:phone]");
}

#[test]
fn protected_relational_authority_refuses_unbound_stages_but_admits_declared_independent_tables() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("contextful.toml"), "[[pipeline.tables]]\nname = \"independent\"\n").unwrap();
    std::fs::create_dir(dir.path().join("pipelines")).unwrap();
    std::fs::write(
        dir.path().join("pipelines/feed.toml"),
        r#"
id = "feed"
tables = ["messages"]
normalize = "relational"
journal = false
redaction = [{ table = "messages", column = "body", match = "whole", operation = "replace", argument = "phone" }]
[source]
name = "http"
"#,
    )
    .unwrap();
    let store = Store::open(dir.path(), "research").unwrap();
    let ctx = context();
    let unknown = TableDecl::named("undeclared");
    assert!(stage_part(&store, &unknown, &input(), &ctx.node, &ctx.injection, 0, 0).is_err());
    assert!(!store.table_dir("undeclared").unwrap().exists(), "refusal precedes any staged artifact");
    let independent = TableDecl::named("independent");
    let part = stage_part(&store, &independent, &input(), &ctx.node, &ctx.injection, 0, 0).unwrap();
    let path = store.table_dir("independent").unwrap().join("data/runs/run-a/ingest-a/stage.staging").join(part.name);
    assert_eq!(body(&path), "415-555-0100", "explicit independent authority retains its declared policy");
}

#[test]
fn an_empty_relational_rule_set_does_not_add_writer_authority() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("pipelines")).unwrap();
    std::fs::write(
        dir.path().join("pipelines/feed.toml"),
        r#"
id = "feed"
tables = ["messages"]
normalize = "relational"
journal = false
redaction = []
[source]
name = "http"
"#,
    )
    .unwrap();
    let store = Store::open(dir.path(), "research").unwrap();
    let ctx = context();
    let table = TableDecl::named("independent");
    let part = stage_part(&store, &table, &input(), &ctx.node, &ctx.injection, 0, 0).unwrap();
    let path = store.table_dir("independent").unwrap().join("data/runs/run-a/ingest-a/stage.staging").join(part.name);
    assert_eq!(body(&path), "415-555-0100");
}

#[test]
fn direct_relational_landing_uses_the_same_canonical_group_writer() {
    use contextful_context::land::land_normalized_batches;
    let (_dir, store) = declared();
    let row = json!({"body":[{"text":"415-555-0100"}],"public":"unchanged"});
    let batch = Batch { rows: vec![row.as_object().unwrap().clone()], types: Default::default() };
    let ctx = context();
    let position = Position { pipeline_id: Some("feed".into()), ..Position::default() };
    let root = land_normalized_batches(&store, &TableDecl::named("messages"), &[batch], 5, &ctx, &position, &|| Ok(())).unwrap();
    assert_eq!(root.parts.len(), 1);
    let runs = store.committed_runs("messages_body").unwrap();
    assert_eq!(runs.len(), 1, "the published group includes its child");
    let path = store.table_dir("messages_body").unwrap().join("data/runs/run-a/ingest-a").join(&runs[0].parts[0].name);
    let mut reader = ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(path).unwrap()).unwrap().build().unwrap();
    let batch = reader.next().unwrap().unwrap();
    assert_eq!(batch.column_by_name("text").unwrap().as_any().downcast_ref::<arrow_array::StringArray>().unwrap().value(0), "[REDACTED:phone]");
}

#[test]
fn alternate_declaration_adds_writer_authority_without_replacing_canonical_rules() {
    let (dir, _store) = declared();
    let extra = dir.path().join("extra.toml");
    std::fs::write(
        &extra,
        r#"
[[pipeline.tables]]
name = "other"
redaction = [{ table = "other", column = "body", match = "whole", operation = "replace", argument = "email" }]
"#,
    )
    .unwrap();
    let store = Store::open_declared(dir.path(), "research", &extra).unwrap();
    let ctx = context();
    for (table, marker) in [("messages", "[REDACTED:phone]"), ("other", "[REDACTED:email]")] {
        let part = stage_part(&store, &TableDecl::named(table), &input(), &ctx.node, &ctx.injection, 0, 0).unwrap();
        let path = store.table_dir(table).unwrap().join("data/runs/run-a/ingest-a/stage.staging").join(part.name);
        assert_eq!(body(&path), marker);
    }
    std::fs::write(&extra, "[[pipeline.tables]]\nname = \"messages\"\n").unwrap();
    let store = Store::open_declared(dir.path(), "research", &extra).unwrap();
    let part = stage_part(&store, &TableDecl::named("messages"), &input(), &ctx.node, &ctx.injection, 1, 0).unwrap();
    let path = store.table_dir("messages").unwrap().join("data/runs/run-a/ingest-a/stage.staging").join(part.name);
    assert_eq!(body(&path), "[REDACTED:phone]", "a ruleless additional binding preserves canonical removal");
}

#[test]
fn writer_digests_use_the_declared_row_selector_and_strict_unknown_fallback() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("contextful.toml"),
        r#"
[[pipeline.tables]]
name = "messages"
redaction = [{ table = "messages", column = "body", match = "whole", operation = "hash" }]
[pipeline.tables.policy.columns]
body = { class = { from_column = "kind" }, strategies = { completion = "hash" }, fallback = "drop" }
"#,
    )
    .unwrap();
    let store = Store::open(dir.path(), "research").unwrap();
    let ctx = context();
    let batch = Batch { rows: vec![json!({"body":"private", "kind":"unregistered"}).as_object().unwrap().clone()], types: Default::default() };
    let part = stage_part(&store, &TableDecl::named("messages"), &batch, &ctx.node, &ctx.injection, 0, 0).unwrap();
    let path = store.table_dir("messages").unwrap().join("data/runs/run-a/ingest-a/stage.staging").join(part.name);
    assert_eq!(body(&path), "", "an unknown row class reaches drop, never an undeclared digest branch");
}

#[test]
fn canonical_removed_index_refuses_even_when_the_caller_supplies_no_indexes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("contextful.toml"),
        r#"
[[pipeline.tables]]
name = "messages"
primary_key = ["id"]
redaction = [{ table = "messages", column = "body", operation = "drop" }]
[[pipeline.tables.indexes]]
kind = "fulltext"
column = "body"
"#,
    )
    .unwrap();
    match Store::open(dir.path(), "research") {
        Err(error) => assert!(error.to_string().contains("StoreIndexOverRedactedColumn"), "{error}"),
        Ok(_) => panic!("canonical index authority cannot be omitted by a later bare declaration"),
    }
}

#[test]
fn whole_binary_hash_uses_decoded_bytes_and_persists_the_shared_text_digest() {
    use contextful_core::store::reconcile::ColumnType;
    use contextful_policy::enforce::mask::Pepper;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("contextful.toml"),
        r#"
[[pipeline.tables]]
name = "messages"
redaction = [{ table = "messages", column = "body", operation = "hash" }]
"#,
    )
    .unwrap();
    let store = Store::open(dir.path(), "research").unwrap();
    let ctx = context();
    let batch = Batch { rows: vec![json!({"body":"YWJj"}).as_object().unwrap().clone()], types: [("body".into(), ColumnType::Binary)].into() };
    let part = stage_part(&store, &TableDecl::named("messages"), &batch, &ctx.node, &ctx.injection, 0, 0).unwrap();
    let path = store.table_dir("messages").unwrap().join("data/runs/run-a/ingest-a/stage.staging").join(part.name);
    let pepper = Pepper::resolve(|key| std::env::var(key).ok());
    assert_eq!(body(&path), pepper.digest_bytes(b"abc"));
}

#[test]
fn canonical_binary_type_controls_a_bare_callers_hash_input() {
    use contextful_policy::enforce::mask::Pepper;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("contextful.toml"),
        r#"
[[pipeline.tables]]
name = "messages"
columns = { body = "binary" }
redaction = [{ table = "messages", column = "body", operation = "hash" }]
"#,
    )
    .unwrap();
    let store = Store::open(dir.path(), "research").unwrap();
    let ctx = context();
    let batch = Batch { rows: vec![json!({"body":"YWJj"}).as_object().unwrap().clone()], types: Default::default() };
    let part = stage_part(&store, &TableDecl::named("messages"), &batch, &ctx.node, &ctx.injection, 0, 0).unwrap();
    let path = store.table_dir("messages").unwrap().join("data/runs/run-a/ingest-a/stage.staging").join(part.name);
    assert_eq!(body(&path), Pepper::resolve(|key| std::env::var(key).ok()).digest_bytes(b"abc"));
}

#[test]
fn declared_json_cell_lands_only_addressed_rewrites_as_canonical_json() {
    use contextful_core::store::reconcile::ColumnType;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("contextful.toml"), r#"
[[pipeline.tables]]
name = "messages"
columns = { body = "json" }
redaction = [{ table = "messages", column = "body", match = { pattern = "[0-9]{3}-[0-9]{3}-[0-9]{4}" }, json_path = "$.parts[*].text", operation = "replace", argument = "phone" }]
"#).unwrap();
    let store = Store::open(dir.path(), "research").unwrap();
    let ctx = context();
    let batch = Batch {
        rows: vec![json!({"body":{"z":"public 415-555-0100","parts":[{"text":"☎ call 415-555-0100 now", "kind":"prompt"}]}}).as_object().unwrap().clone()],
        types: [("body".into(), ColumnType::Json)].into(),
    };
    let part = stage_part(&store, &TableDecl::named("messages"), &batch, &ctx.node, &ctx.injection, 0, 0).unwrap();
    let path = store.table_dir("messages").unwrap().join("data/runs/run-a/ingest-a/stage.staging").join(part.name);
    assert_eq!(body(&path), "{\"parts\":[{\"kind\":\"prompt\",\"text\":\"☎ call [REDACTED:phone] now\"}],\"z\":\"public 415-555-0100\"}");
}

#[test]
fn independently_declared_child_rules_precede_all_persisted_group_identities() {
    use contextful_context::land::{stage_normalized_group, NormalizedStage};
    use contextful_core::pipeline::normalize::NormalizedGroup;
    let mut outputs = Vec::new();
    for secret in ["415-555-0100", "415-555-0999"] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("contextful.toml"),
            r#"
[[pipeline.tables]]
name = "messages"
[[pipeline.tables]]
name = "messages_body"
redaction = [{ table = "messages_body", column = "text", operation = "replace", argument = "phone" }]
"#,
        )
        .unwrap();
        let store = Store::open(dir.path(), "research").unwrap();
        let ctx = context();
        let row = json!({"body":[{"text":secret}],"public":"unchanged"});
        let group = NormalizedGroup::new(vec![row.as_object().unwrap().clone()], "messages", "run-a", 5);
        let parts = stage_normalized_group(
            &store,
            &TableDecl::named("messages"),
            NormalizedStage { group, ordinal: 0, offsets: Default::default() },
            &ctx.node,
            &ctx.injection,
        )
        .unwrap();
        outputs.push(
            parts
                .into_iter()
                .map(|(table, part)| {
                    let path = store.table_dir(&table).unwrap().join("data/runs/run-a/ingest-a/stage.staging").join(part.name);
                    (table, std::fs::read(path).unwrap())
                })
                .collect::<std::collections::BTreeMap<_, _>>(),
        );
    }
    assert_eq!(outputs[0], outputs[1], "a removed child value contributes no root, parent or child identity");
}

#[test]
fn a_child_row_selector_without_typed_sibling_lineage_refuses_before_files() {
    use contextful_context::land::{stage_normalized_group, NormalizedStage};
    use contextful_core::pipeline::normalize::NormalizedGroup;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("contextful.toml"),
        r#"
[[pipeline.tables]]
name = "messages"
[[pipeline.tables]]
name = "messages_body"
redaction = [{ table = "messages_body", column = "text", operation = "hash" }]
[pipeline.tables.policy.columns]
text = { class = { from_column = "kind" }, strategies = { completion = "hash" }, fallback = "drop" }
"#,
    )
    .unwrap();
    let store = Store::open(dir.path(), "research").unwrap();
    let ctx = context();
    let row = json!({"body":[{"text":"private","kind":"completion"}]});
    let group = NormalizedGroup::new(vec![row.as_object().unwrap().clone()], "messages", "run-a", 5);
    assert!(stage_normalized_group(
        &store,
        &TableDecl::named("messages"),
        NormalizedStage { group, ordinal: 0, offsets: Default::default() },
        &ctx.node,
        &ctx.injection
    )
    .is_err());
    assert!(!store.table_dir("messages").unwrap().exists());
    assert!(!store.table_dir("messages_body").unwrap().exists());
}

#[test]
fn child_hash_uses_its_canonical_binary_source_type_before_identity_derivation() {
    use contextful_context::land::{stage_normalized_group, NormalizedStage};
    use contextful_core::pipeline::normalize::NormalizedGroup;
    use contextful_policy::enforce::mask::Pepper;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("contextful.toml"),
        r#"
[[pipeline.tables]]
name = "messages"
[[pipeline.tables]]
name = "messages_parts"
columns = { body = "binary" }
redaction = [{ table = "messages_parts", column = "body", operation = "hash" }]
"#,
    )
    .unwrap();
    let store = Store::open(dir.path(), "research").unwrap();
    let ctx = context();
    let row = json!({"parts":[{"body":"YWJj"}]});
    let group = NormalizedGroup::new(vec![row.as_object().unwrap().clone()], "messages", "run-a", 5);
    let parts = stage_normalized_group(
        &store,
        &TableDecl::named("messages"),
        NormalizedStage { group, ordinal: 0, offsets: Default::default() },
        &ctx.node,
        &ctx.injection,
    )
    .unwrap();
    let path = store.table_dir("messages_parts").unwrap().join("data/runs/run-a/ingest-a/stage.staging").join(&parts["messages_parts"].name);
    assert_eq!(body(&path), Pepper::resolve(|key| std::env::var(key).ok()).digest_bytes(b"abc"));
}

#[test]
fn a_parent_text_pattern_refuses_a_canonical_binary_descendant_before_files() {
    use contextful_context::land::{stage_normalized_group, NormalizedStage};
    use contextful_core::pipeline::normalize::NormalizedGroup;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("contextful.toml"),
        r#"
[[pipeline.tables]]
name = "messages"
redaction = [{ table = "messages", column = "parts", match = { pattern = "[0-9]{3}-[0-9]{3}-[0-9]{4}" }, operation = "replace", argument = "phone" }]
[[pipeline.tables]]
name = "messages_parts"
columns = { body = "binary" }
"#,
    )
    .unwrap();
    let store = Store::open(dir.path(), "research").unwrap();
    let ctx = context();
    let row = json!({"parts":[{"body":"NDE1LTU1NS0wMTAw"}]});
    let group = NormalizedGroup::new(vec![row.as_object().unwrap().clone()], "messages", "run-a", 5);
    assert!(stage_normalized_group(
        &store,
        &TableDecl::named("messages"),
        NormalizedStage { group, ordinal: 0, offsets: Default::default() },
        &ctx.node,
        &ctx.injection
    )
    .is_err());
    assert!(!store.table_dir("messages").unwrap().exists());
    assert!(!store.table_dir("messages_parts").unwrap().exists());
}

#[test]
fn a_binary_hash_refuses_an_invalid_source_encoding_before_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("contextful.toml"),
        r#"
[[pipeline.tables]]
name = "messages"
columns = { body = "binary" }
redaction = [{ table = "messages", column = "body", operation = "hash" }]
"#,
    )
    .unwrap();
    let store = Store::open(dir.path(), "research").unwrap();
    let ctx = context();
    let batch = Batch { rows: vec![json!({"body":"!"}).as_object().unwrap().clone()], types: Default::default() };
    assert!(stage_part(&store, &TableDecl::named("messages"), &batch, &ctx.node, &ctx.injection, 0, 0).is_err());
    assert!(!store.table_dir("messages").unwrap().exists());
}

#[test]
fn a_row_selected_binary_drop_preserves_the_shared_null_semantics() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("contextful.toml"),
        r#"
[[pipeline.tables]]
name = "messages"
columns = { body = "binary", kind = "utf8" }
redaction = [{ table = "messages", column = "body", operation = "hash" }]
[pipeline.tables.policy.columns]
body = { class = { from_column = "kind" }, strategies = { completion = "hash" }, fallback = "drop" }
"#,
    )
    .unwrap();
    let store = Store::open(dir.path(), "research").unwrap();
    let ctx = context();
    let batch = Batch { rows: vec![json!({"body":"YWJj","kind":"unknown"}).as_object().unwrap().clone()], types: Default::default() };
    let part = stage_part(&store, &TableDecl::named("messages"), &batch, &ctx.node, &ctx.injection, 0, 0).unwrap();
    let path = store.table_dir("messages").unwrap().join("data/runs/run-a/ingest-a/stage.staging").join(part.name);
    let mut reader = ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(path).unwrap()).unwrap().build().unwrap();
    let batch = reader.next().unwrap().unwrap();
    assert!(batch.column_by_name("body").unwrap().is_null(0), "strict drop nulls a non-text cell");
}
#[test]
fn prepared_clock_admission_checks_derived_json_path_and_relational_removal_authority() {
    let (dir, _) = declared();
    let path = dir.path().join("contextful.toml");
    std::fs::write(&path, "[[pipeline.tables]]\nname='messages'\nredaction=[{table='messages',column='body',json_path='$.private',match='whole',operation='drop'}]\n").unwrap();
    let store = Store::open(dir.path(), "research").unwrap();
    let fields = |name: &str| std::collections::BTreeSet::from([name.to_string()]);
    assert!(store.validate_recording_clock("messages", &fields("body"), None).is_err());
    store.validate_recording_clock("messages", &fields("public_clock"), None).unwrap();
    let normalize = contextful_core::pipeline::normalize::Normalize { mode:contextful_core::pipeline::normalize::Mode::Relational, depth:5 };
    assert!(store.validate_recording_clock("messages", &fields("public_clock"), Some(normalize)).is_err());
    std::fs::write(&path, "[[pipeline.tables]]\nname='messages'\n").unwrap();
    Store::open(dir.path(), "research").unwrap().validate_recording_clock("messages", &fields("public_clock"), Some(normalize)).unwrap();
}
