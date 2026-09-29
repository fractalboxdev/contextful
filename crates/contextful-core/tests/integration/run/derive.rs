//! The derive tier's domain: configuration, binding, selection, rows and the cue grammar.

use contextful_core::run::derive::config::{
    bind, bindings, check_env_name, check_output_table, DeriveConfig, Task, ATTEMPTS_PER_UNIT, DERIVE_PRIMARY_KEY, LINK_SECONDS_PER_RUN, ROWS_PER_RUN,
};
use contextful_core::run::derive::cues::{parse, passages, Cue, PASSAGES_PER_DOCUMENT, PASSAGE_BYTES, PASSAGE_CHARS, PASSAGE_SPAN_MS};
use contextful_core::run::derive::emit::{
    document_status, marker_row, passage_rows, revived, select, superseded, Derivation, Unit, UnitStatus, DERIVATION_KEY, EMPTY_ATTEMPTS,
};
use contextful_core::run::derive::task::{check_content_table, check_host_tables, host_key, select_host, DeriveTask, Derived, HostUnit, Tasks};
use contextful_core::run::derive::exec::{engine_id, excerpt, CAPTURED_OUTPUT_BYTES, CHAIN_DEADLINE_SECS, STEP_ERROR_EXCERPT_BYTES};
use contextful_core::run::ports::Row;
use contextful_core::run::RunError;
use contextful_core::store::declare::TableDecl;
use serde_json::{json, Value};

/// A registry holding `split`, a host task landing `words` and its markers in `units`.
fn registered() -> Tasks {
    struct Split;
    impl DeriveTask for Split {
        fn version(&self) -> &str {
            "1"
        }
        fn columns(&self) -> Vec<String> {
            vec!["body".into()]
        }
        fn marker_table(&self) -> String {
            "units".into()
        }
        fn content_tables(&self) -> Vec<String> {
            vec!["words".into()]
        }
        fn derive(&self, _unit: &HostUnit) -> Result<Derived, RunError> {
            Ok(Derived::new())
        }
    }
    let mut t = Tasks::default();
    t.register("split", std::sync::Arc::new(Split)).unwrap();
    t
}

fn cfg(v: Value) -> Result<DeriveConfig, RunError> {
    DeriveConfig::parse("doc-text", &v)
}

fn base() -> Value {
    json!({"engine": "reader", "source_table": "documents", "media_column": "path", "parent_id_column": "doc_id"})
}

fn with(k: &str, v: Value) -> Value {
    let mut b = base();
    b[k] = v;
    b
}

fn rows(v: Value) -> Vec<Row> {
    v.as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect()
}

/// A derivation under engine `engine`, with no binding parameters and no declared columns.
fn under(engine: &str) -> Derivation {
    Derivation { engine_id: engine.into(), binding: json!({}), output_schema: json!({}) }
}

fn derivation() -> Derivation {
    under("exec:reader@000000000000")
}

/// The key `derivation()` gives a parent row of `id` and `media`.
fn k(id: &str, media: &str) -> String {
    derivation().key(id, media, None)
}

fn unit(key: &str, media: &str, prior_attempts: i64) -> Unit {
    Unit { key: key.into(), media: media.into(), prior_attempts, derivation_key: k(key, media) }
}

/// An absent or blank `source_table` or `parent_id_column`, or an absent or blank `engine` or `media_column`
/// behind a built-in task, raises `DeriveConfigKeyMissing`, naming the key and the pipeline.
// spec: run.select.required-key@abb2e19c
#[test]
fn a_missing_or_blank_required_key_refuses() {
    // A host task binds no engine and reads no media column; it still names its source.
    let host = registered();
    let host_cfg = json!({"task": "split", "source_table": "documents", "parent_id_column": "doc_id"});
    assert!(DeriveConfig::parse_with("doc-text", &host_cfg, &host).is_ok());
    for key in ["source_table", "parent_id_column"] {
        let mut v = host_cfg.clone();
        v[key] = json!(" ");
        assert!(matches!(DeriveConfig::parse_with("doc-text", &v, &host), Err(RunError::DeriveConfigKeyMissing(m)) if m.contains(key)), "{key}");
    }
    for key in ["engine", "source_table", "media_column", "parent_id_column"] {
        for blank in [Value::Null, json!(""), json!("   ")] {
            let mut v = base();
            if blank.is_null() {
                v.as_object_mut().unwrap().remove(key);
            } else {
                v[key] = blank;
            }
            match cfg(v) {
                Err(RunError::DeriveConfigKeyMissing(m)) => assert!(m.contains(key) && m.contains("doc-text"), "{m}"),
                other => panic!("{key}: {other:?}"),
            }
        }
    }
}

/// The anti-join's pipeline id comes from the build alone; a config key naming another pipeline's output raises
/// `DeriveForeignOutputTable`.
// spec: run.select.foreign-output-table@1548b5f0
#[test]
fn a_key_naming_another_pipelines_output_refuses() {
    for k in ["output_table", "pipeline_id"] {
        assert!(matches!(cfg(with(k, json!("other_passages"))), Err(RunError::DeriveForeignOutputTable(_))), "{k}");
    }
}

/// `max_rows_per_run` truncates the outstanding list after the anti-join, at 25 rows by default; zero or below
/// resolves to that default.
// spec: run.select.rows-per-run@c63ee582
#[test]
fn a_run_takes_25_units_by_default_after_the_anti_join() {
    assert_eq!(ROWS_PER_RUN, 25);
    assert_eq!(cfg(base()).unwrap().max_rows_per_run, 25);
    assert_eq!(cfg(with("max_rows_per_run", json!(0))).unwrap().max_rows_per_run, 25);
    assert_eq!(cfg(with("max_rows_per_run", json!(-4))).unwrap().max_rows_per_run, 25);
    let c = cfg(with("max_rows_per_run", json!(2))).unwrap();
    let parents = rows(json!([{"doc_id": "a", "path": "a"}, {"doc_id": "b", "path": "b"}, {"doc_id": "c", "path": "c"}, {"doc_id": "d", "path": "d"}]));
    // `a` already holds passages: truncation counts the outstanding units, not the parents.
    let derived = rows(json!([{"unit_ref": "a", "cue_seq": 0, "kind": "passage", "derivation_key": k("a", "a")}]));
    let keys: Vec<String> = select(&parents, &derived, &c, &derivation()).outstanding.into_iter().map(|u| u.key).collect();
    assert_eq!(keys, ["b", "c"]);
}

/// `max_attempts` allows 3 attempts per unit by default, and a non-positive spelling means the same figure.
// spec: run.select.attempts-per-unit@2f2b9bb1
#[test]
fn a_unit_receives_3_attempts_by_default() {
    assert_eq!(ATTEMPTS_PER_UNIT, 3);
    assert_eq!(cfg(base()).unwrap().max_attempts, 3);
    assert_eq!(cfg(with("max_attempts", json!(-1))).unwrap().max_attempts, 3);
    let c = cfg(base()).unwrap();
    let parents = rows(json!([{"doc_id": "a", "path": "a"}, {"doc_id": "b", "path": "b"}]));
    let derived = rows(json!([
        {"unit_ref": "a", "cue_seq": -1, "kind": "marker", "unit_status": "failed", "attempts": 2, "retryable": true, "derivation_key": k("a", "a")},
        {"unit_ref": "b", "cue_seq": -1, "kind": "marker", "unit_status": "failed", "attempts": 3, "retryable": true, "derivation_key": k("b", "b")}
    ]));
    let out = select(&parents, &derived, &c, &derivation()).outstanding;
    assert_eq!(out, [unit("a", "a", 2)], "the third failure settles `b`");
}

/// `max_seconds_per_run` bounds the serial unit loop at 300 s by default for `link_preview` and carries no
/// default for `transcribe`.
// spec: run.select.seconds-per-run@19394281
#[test]
fn link_preview_holds_300_s_and_transcribe_carries_no_default() {
    assert_eq!(LINK_SECONDS_PER_RUN, 300);
    assert_eq!(cfg(base()).unwrap().max_seconds_per_run, None);
    let link = cfg(with("task", json!("link_preview"))).unwrap();
    assert_eq!((link.task, link.max_seconds_per_run), (Task::LinkPreview, Some(300)));
    assert_eq!(cfg(with("max_seconds_per_run", json!(40))).unwrap().max_seconds_per_run, Some(40));
}

/// A derive pipeline configured to journal its pulls raises `DeriveJournaledPull`.
// spec: run.select.journaled-pull@a6dba9d0
#[test]
fn a_derive_pipeline_journaling_its_pulls_refuses() {
    assert!(matches!(cfg(with("journal", json!(true))), Err(RunError::DeriveJournaledPull(_))));
    assert!(cfg(with("journal", json!(false))).is_ok());
}

/// Each tick recomputes the outstanding set: every parent row holding neither a passage nor a settled marker under
/// its current {{run.emit.derivation-key}} in the pipeline's own output table, or, for a host task, its marker table.
// spec: run.select.anti-join@bb0e90e2
#[test]
fn the_outstanding_set_is_every_parent_without_passages_or_a_settled_marker() {
    let c = cfg(base()).unwrap();
    let parents = rows(json!([
        {"doc_id": "done", "path": "p"}, {"doc_id": "empty", "path": "p"}, {"doc_id": "retry", "path": "p"},
        {"doc_id": "permanent", "path": "p"}, {"doc_id": "new", "path": "p"}, {"doc_id": "new", "path": "p"}, {"doc_id": " ", "path": "p"}
    ]));
    let derived = rows(json!([
        {"unit_ref": "done", "cue_seq": 0, "kind": "passage", "derivation_key": k("done", "p")},
        {"unit_ref": "empty", "cue_seq": -1, "kind": "marker", "unit_status": "empty", "attempts": 1, "derivation_key": k("empty", "p")},
        {"unit_ref": "retry", "cue_seq": -1, "kind": "marker", "unit_status": "unavailable", "attempts": 1, "derivation_key": k("retry", "p")},
        {"unit_ref": "permanent", "cue_seq": -1, "kind": "marker", "unit_status": "failed", "attempts": 1, "retryable": false, "derivation_key": k("permanent", "p")},
        {"unit_ref": "unkeyed", "cue_seq": 0, "kind": "passage"},
        {"unit_ref": "other-key", "cue_seq": 0, "kind": "passage", "derivation_key": "0".repeat(64)}
    ]));
    let parents = [parents, rows(json!([{"doc_id": "unkeyed", "path": "p"}, {"doc_id": "other-key", "path": "p"}]))].concat();
    let sel = select(&parents, &derived, &c, &derivation());
    assert_eq!(sel.outstanding.iter().map(|u| u.key.as_str()).collect::<Vec<_>>(), ["retry", "new", "unkeyed", "other-key"]);
    assert!(sel.outstanding.iter().all(|u| u.derivation_key == k(&u.key, "p")), "each unit carries its current key");
    assert!(matches!(&sel.incomplete[..], [RunError::DeriveUnitIncomplete(_)]));

    // A host task anti-joins against its marker table: an `ok` or `empty` marker settles a unit.
    let host = registered();
    let task = host.get("split").unwrap();
    let hc = DeriveConfig::parse_with("doc-text", &json!({"task": "split", "source_table": "documents", "parent_id_column": "doc_id"}), &host).unwrap();
    let parents = rows(json!([{"doc_id": "done", "body": "a"}, {"doc_id": "empty", "body": ""}, {"doc_id": "retry", "body": "b"}, {"doc_id": "new", "body": "c"}, {"body": "x"}]));
    let hk = |id: &str| host_key("split", "1", id, parents.iter().find(|r| r["doc_id"] == json!(id)).unwrap(), &["body".into()]);
    let markers = rows(json!([
        {"unit_ref": "done", "kind": "marker", "unit_status": "ok", "attempts": 1, "derivation_key": hk("done")},
        {"unit_ref": "empty", "kind": "marker", "unit_status": "empty", "attempts": 1, "derivation_key": hk("empty")},
        {"unit_ref": "retry", "kind": "marker", "unit_status": "failed", "attempts": 1, "retryable": true, "derivation_key": hk("retry")}
    ]));
    let sel = select_host(&parents, &markers, &hc, task.as_ref());
    assert_eq!(sel.outstanding.iter().map(|u| (u.key.as_str(), u.prior_attempts)).collect::<Vec<_>>(), [("retry", 1), ("new", 0)]);
    assert_eq!(sel.outstanding[1].row["body"], json!("c"), "a unit carries its parent row");
    assert!(matches!(&sel.incomplete[..], [RunError::DeriveUnitIncomplete(_)]));
}

/// A unit's standing under a key is its latest marker under that key by `_ingested_at`, `_run_id` and `_row_seq`,
/// never its highest attempt count.
// spec: run.select.latest-marker@08e93879
#[test]
fn a_units_latest_marker_decides_its_standing() {
    let c = cfg(base()).unwrap();
    let key = |u: &str| k(u, "p");
    let parents = rows(json!([{"doc_id": "settled", "path": "p"}, {"doc_id": "retrying", "path": "p"}, {"doc_id": "same-instant", "path": "p"}]));
    let derived = rows(json!([
        {"unit_ref": "settled", "kind": "marker", "unit_status": "failed", "attempts": 2, "retryable": true, "derivation_key": key("settled"),
         "_ingested_at": "2026-01-01T00:00:00.000000000Z", "_run_id": "r1", "_row_seq": 0},
        {"unit_ref": "settled", "kind": "marker", "unit_status": "empty", "attempts": 1, "retryable": false, "derivation_key": key("settled"),
         "_ingested_at": "2026-01-02T00:00:00.000000000Z", "_run_id": "r2", "_row_seq": 0},
        {"unit_ref": "retrying", "kind": "marker", "unit_status": "unavailable", "attempts": 2, "retryable": true, "derivation_key": key("retrying"),
         "_ingested_at": "2026-01-03T00:00:00.000000000Z", "_run_id": "r3", "_row_seq": 0},
        {"unit_ref": "retrying", "kind": "marker", "unit_status": "unavailable", "attempts": 1, "retryable": true, "derivation_key": key("retrying"),
         "_ingested_at": "2026-01-02T00:00:00.000000000Z", "_run_id": "r2", "_row_seq": 1},
        {"unit_ref": "same-instant", "kind": "marker", "unit_status": "empty", "attempts": 1, "retryable": false, "derivation_key": key("same-instant"),
         "_ingested_at": "2026-01-02T00:00:00.000000000Z", "_run_id": "r2", "_row_seq": 3},
        {"unit_ref": "same-instant", "kind": "marker", "unit_status": "failed", "attempts": 2, "retryable": true, "derivation_key": key("same-instant"),
         "_ingested_at": "2026-01-02T00:00:00.000000000Z", "_run_id": "r2", "_row_seq": 2}
    ]));
    let sel = select(&parents, &derived, &c, &derivation());
    assert_eq!(sel.outstanding, [unit("retrying", "p", 2)]);
}

/// An engine with no `[derive.<name>]` block raises `DeriveEngineUnbound`, naming the pipeline, the engine, the
/// file to edit and every bound engine.
// spec: run.bind.unbound-engine@a35410ef
#[test]
fn an_engine_with_no_block_refuses_naming_every_bound_engine() {
    let b = bindings("[derive.ocr]\ndriver = \"exec\"\n[derive.asr]\ndriver = \"exec\"\n").unwrap();
    match bind("doc-text", &cfg(base()).unwrap(), &b, "contextful.toml") {
        Err(RunError::DeriveEngineUnbound(m)) => {
            for part in ["doc-text", "`reader`", "contextful.toml", "asr", "ocr"] {
                assert!(m.contains(part), "{part}: {m}");
            }
        }
        other => panic!("{other:?}"),
    }
}

/// `command`, `preprocess`, `env` or `allow_hosts` inside `[pipeline.source.config]` raises
/// `DeriveCommandInManifest`.
// spec: run.bind.command-in-manifest@a1e200c3
#[test]
fn an_executable_key_in_the_manifest_refuses() {
    for k in ["command", "preprocess", "env", "allow_hosts"] {
        assert!(matches!(cfg(with(k, json!(["x"]))), Err(RunError::DeriveCommandInManifest(m)) if m.contains(k)), "{k}");
    }
}

/// `task` names `transcribe`, the default, `link_preview`, or a task the embedding binary registered; any other
/// value raises `DeriveUnknownTask`, listing the built-in and registered names.
// spec: run.bind.unknown-task@d7129e2a
#[test]
fn a_task_outside_the_pair_refuses_printing_both() {
    match cfg(with("task", json!("summarize"))) {
        Err(RunError::DeriveUnknownTask(m)) => assert!(m.contains("transcribe") && m.contains("link_preview") && m.contains("summarize"), "{m}"),
        other => panic!("{other:?}"),
    }
    let host = registered();
    match DeriveConfig::parse_with("doc-text", &with("task", json!("summarize")), &host) {
        Err(RunError::DeriveUnknownTask(m)) => assert!(m.contains("transcribe, link_preview") && m.contains("split"), "{m}"),
        other => panic!("{other:?}"),
    }
    let named = json!({"task": "split", "source_table": "documents", "parent_id_column": "doc_id"});
    assert_eq!(DeriveConfig::parse_with("doc-text", &named, &host).unwrap().task, Task::Host("split".into()));
    assert_eq!(cfg(base()).unwrap().task, Task::Transcribe, "transcribe is the default");
}

/// `driver` is `exec` or `"none"` behind `transcribe` and `fetch` behind `link_preview`, and a host task names no
/// `engine`; any other pairing raises `DeriveDriverMismatch`.
// spec: run.bind.driver-mismatch@d38ed4fd
#[test]
fn a_driver_the_task_does_not_serve_refuses() {
    let host = json!({"task": "split", "engine": "reader", "source_table": "documents", "parent_id_column": "doc_id"});
    assert!(matches!(DeriveConfig::parse_with("doc-text", &host, &registered()), Err(RunError::DeriveDriverMismatch(_))));
    let b = bindings("[derive.reader]\ndriver = \"fetch\"\n").unwrap();
    assert!(matches!(bind("doc-text", &cfg(base()).unwrap(), &b, "m"), Err(RunError::DeriveDriverMismatch(_))));
    let link = DeriveConfig { task: Task::LinkPreview, ..cfg(base()).unwrap() };
    let exec = bindings("[derive.reader]\ndriver = \"exec\"\n").unwrap();
    assert!(matches!(bind("doc-text", &link, &exec, "m"), Err(RunError::DeriveDriverMismatch(_))));
    for driver in ["exec", "none"] {
        let ok = bindings(&format!("[derive.reader]\ndriver = \"{driver}\"\n")).unwrap();
        assert!(bind("doc-text", &cfg(base()).unwrap(), &ok, "m").is_ok());
    }
    assert!(bind("doc-text", &link, &b, "m").is_ok());
}

/// `command` is an argument array run with no shell; a `command` given as one string raises `DeriveShellCommand`.
// spec: run.exec.shell-command@6891fe11
#[test]
fn a_command_given_as_one_string_refuses() {
    let b = bindings("[derive.reader.engine]\ncommand = \"cat {input} | tr a-z A-Z\"\n").unwrap();
    assert!(matches!(b["reader"].engine.as_ref().unwrap().argv("reader"), Err(RunError::DeriveShellCommand(_))));
    let ok = bindings("[derive.reader.engine]\ncommand = [\"cat\", \"{input}\"]\n").unwrap();
    assert_eq!(ok["reader"].engine.as_ref().unwrap().argv("reader").unwrap(), ["cat", "{input}"]);
}

/// An allowlist name that is not ASCII alphanumeric or underscore, or that leads with a digit, raises
/// `DeriveEnvNameInvalid`.
// spec: run.exec.env-name@c55db49a
#[test]
fn an_allowlist_name_outside_the_variable_grammar_refuses() {
    for ok in ["PATH", "SPEECH_API_KEY", "_x1"] {
        assert!(check_env_name(ok).is_ok(), "{ok}");
    }
    for bad in ["1KEY", "API-KEY", "A B", "", "KÉY"] {
        assert!(matches!(check_env_name(bad), Err(RunError::DeriveEnvNameInvalid(_))), "{bad:?}");
    }
}

/// An `exec` engine id reads `exec:<name>@<prefix>`, the prefix being 12 chars of lowercase hex over every step's
/// binary digest and arguments.
// spec: run.exec.engine-id@e32e953c
#[test]
fn an_exec_engine_id_hashes_every_steps_digest_and_arguments() {
    let steps = vec![("d1".to_string(), vec!["-i".to_string(), "{input}".to_string()]), ("d2".to_string(), vec!["{input}".to_string()])];
    let id = engine_id("local-asr", &steps);
    let (head, prefix) = id.split_once('@').unwrap();
    assert_eq!(head, "exec:local-asr");
    assert_eq!(prefix.len(), 12);
    assert!(prefix.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    assert_ne!(id, engine_id("local-asr", &[steps[0].clone(), ("d3".into(), steps[1].1.clone())]), "a binary change moves the id");
    assert_ne!(id, engine_id("local-asr", &[steps[0].clone(), ("d2".into(), vec!["-x".into()])]), "an argument change moves the id");
}

/// A failing step's error text carries at most 4 KiB of its standard error.
// spec: run.exec.step-error-excerpt@2da14cf2
#[test]
fn a_failing_steps_error_carries_4_kib_of_its_stderr() {
    assert_eq!(STEP_ERROR_EXCERPT_BYTES, 4096);
    let long = "é".repeat(4000);
    let cut = excerpt(long.as_bytes());
    assert!(cut.len() <= 4096 && cut.len() > 4000, "{}", cut.len());
    assert_eq!(excerpt(b"  short failure\n"), "short failure");
    assert_eq!((CHAIN_DEADLINE_SECS, CAPTURED_OUTPUT_BYTES), (1800, 8 * 1024 * 1024));
}

/// `unit_status` is `ok`, `empty` where the engine established nothing to derive, `unavailable` where it returned
/// nothing and stated no reason, or `failed` on a typed error; any other value raises `DeriveUnitStatusUnknown`.
// spec: run.emit.unit-status@ed506426
#[test]
fn a_unit_status_is_one_of_four() {
    for s in ["ok", "empty", "unavailable", "failed"] {
        assert_eq!(UnitStatus::parse(s).unwrap().name(), s);
    }
    assert!(matches!(UnitStatus::parse("skipped"), Err(RunError::DeriveUnitStatusUnknown(_))));
}

/// `attempts` is the prior count under the row's key plus one; `unavailable` and `failed` stop at
/// {{run.select.attempts-per-unit}}, and `empty` receives 1 attempts in total.
// spec: run.emit.attempts@b3309258
#[test]
fn attempts_count_up_from_the_prior_and_an_empty_unit_takes_one() {
    assert_eq!(EMPTY_ATTEMPTS, 1);
    let unit = unit("a", "a", 2);
    assert_eq!(marker_row(&unit, UnitStatus::Failed, Some("x"), true, "e")["attempts"], 3);
    assert_eq!(marker_row(&unit, UnitStatus::Unavailable, None, true, "e")["attempts"], 3);
    assert_eq!(marker_row(&unit, UnitStatus::Empty, None, false, "e")["attempts"], 1);
    // Attempts spent under another key count for nothing under the current one.
    let parents = rows(json!([{"doc_id": "a", "path": "a"}]));
    let spent = rows(json!([{"unit_ref": "a", "cue_seq": -1, "kind": "marker", "unit_status": "failed", "attempts": 2, "retryable": true,
        "derivation_key": under("exec:reader@111111111111").key("a", "a", None)}]));
    assert_eq!(select(&parents, &spent, &cfg(base()).unwrap(), &derivation()).outstanding[0].prior_attempts, 0);
}

/// A unit yielding no passage lands one marker row, `cue_seq` -1 and `kind` `marker`, carrying its status,
/// attempts, last error and whether it retries.
// spec: run.emit.marker-row@f1869530
#[test]
fn a_unit_without_passages_lands_one_marker_row() {
    let unit = unit("d3", "docs/lost.txt", 0);
    let m = marker_row(&unit, UnitStatus::Failed, Some("fetching https://user:pw@host.example/a?token=1 failed"), true, "exec:r@abc");
    assert_eq!(m["cue_seq"], -1);
    assert_eq!(m["kind"], "marker");
    assert_eq!(m["unit_status"], "failed");
    assert_eq!(m["retryable"], true);
    assert_eq!(m["last_error"], "fetching https://host.example/a failed", "the address is redacted");
}

/// Only a WebVTT document whose blocks are its header, notes and styles establishes nothing to derive; empty
/// output, or blocks none of which parse, lands `unavailable`.
// spec: run.emit.empty-document@03c908ae
#[test]
fn only_a_well_formed_webvtt_without_cues_is_empty() {
    let status = |doc: &str| document_status(doc, &parse(doc));
    assert_eq!(status("WEBVTT\n\nNOTE nothing was spoken\n\nSTYLE\n::cue { color: red }\n"), UnitStatus::Empty);
    assert_eq!(status("WEBVTT - silence\n"), UnitStatus::Empty);
    for silent in ["", "  \n\n", "garbled output\n\nmore of it\n", "WEBVTT\n\n00:01.000 --> nonsense\ntext\n", "1\n00:00:01,000 --> 00:00:02,000\n\n"] {
        assert_eq!(status(silent), UnitStatus::Unavailable, "{silent:?}");
    }
    assert_eq!(status("1\n00:00:01,000 --> 00:00:02,000\nhello\n"), UnitStatus::Ok);
}

/// A derive output table or host marker table without `primary_key` `["unit_ref", "derivation_key", "cue_seq"]`, or
/// a host content table whose key does not open with `unit_ref` and `derivation_key`, raises `DerivePrimaryKeyMissing`.
// spec: run.emit.primary-key@1069896e
#[test]
fn a_derive_table_keys_on_unit_derivation_and_sequence() {
    let keyed = |pk: &[&str]| TableDecl { primary_key: Some(pk.iter().map(|s| s.to_string()).collect()), ..TableDecl::named("doc_text_passages") };
    assert_eq!(DERIVE_PRIMARY_KEY, ["unit_ref", "derivation_key", "cue_seq"]);
    assert!(check_output_table(&keyed(&["unit_ref", "derivation_key", "cue_seq"])).is_ok());
    for pk in [&["unit_ref", "cue_seq"][..], &["unit_ref"], &["derivation_key", "unit_ref", "cue_seq"], &[]] {
        assert!(matches!(check_output_table(&keyed(pk)), Err(RunError::DerivePrimaryKeyMissing(_))), "{pk:?}");
    }
    // A host content table's key opens with the unit and its derivation key.
    for pk in [&["unit_ref", "derivation_key"][..], &["unit_ref", "derivation_key", "word_seq"]] {
        assert!(check_content_table(&keyed(pk)).is_ok(), "{pk:?}");
    }
    for pk in [&["unit_ref", "word_seq"][..], &["derivation_key", "unit_ref"], &["unit_ref"], &[]] {
        assert!(matches!(check_content_table(&keyed(pk)), Err(RunError::DerivePrimaryKeyMissing(_))), "{pk:?}");
    }
    // The marker table holds the output table's key.
    let task = registered().get("split").unwrap();
    let named = |name: &str, pk: &[&str]| TableDecl { name: name.into(), ..keyed(pk) };
    let words = named("words", &["unit_ref", "derivation_key", "word_seq"]);
    assert!(check_host_tables("doc-text", "split", task.as_ref(), &[words.clone(), named("units", &DERIVE_PRIMARY_KEY)]).is_ok());
    let loose = [words, named("units", &["unit_ref", "cue_seq"])];
    assert!(matches!(check_host_tables("doc-text", "split", task.as_ref(), &loose), Err(RunError::DerivePrimaryKeyMissing(_))));
}

/// A derive table declaring a column named `kind` raises `DeriveReservedDiscriminator`.
// spec: run.emit.reserved-discriminator@6d178a29
#[test]
fn a_derive_table_naming_kind_refuses() {
    let t = TableDecl { primary_key: Some(DERIVE_PRIMARY_KEY.map(String::from).to_vec()), cluster_by: Some(vec!["kind".into()]), ..TableDecl::named("t") };
    assert!(matches!(check_output_table(&t), Err(RunError::DeriveReservedDiscriminator(_))));
    // A host content table refuses `kind` wherever it declares a column, as an output table does.
    let content = |pk: &[&str]| TableDecl { primary_key: Some(pk.iter().map(|s| s.to_string()).collect()), ..TableDecl::named("words") };
    let kinded = [
        content(&["unit_ref", "derivation_key", "kind"]),
        TableDecl { cluster_by: Some(vec!["kind".into()]), ..content(&["unit_ref", "derivation_key"]) },
        TableDecl { order_by: Some("kind".into()), ..content(&["unit_ref", "derivation_key"]) },
        TableDecl { partition_by: Some(vec!["kind".into()]), ..content(&["unit_ref", "derivation_key"]) },
    ];
    for c in &kinded {
        assert!(matches!(check_output_table(c), Err(RunError::DeriveReservedDiscriminator(_))), "{c:?}");
        assert!(matches!(check_content_table(c), Err(RunError::DeriveReservedDiscriminator(_))), "{c:?}");
        let task = registered().get("split").unwrap();
        let tables = [c.clone(), TableDecl { name: "units".into(), ..t.clone() }];
        assert!(matches!(check_host_tables("doc-text", "split", task.as_ref(), &tables), Err(RunError::DeriveReservedDiscriminator(_))), "{c:?}");
    }
}

/// SubRip and WebVTT read through one grammar: a block's timing line, then its text lines joined by a space.
// spec: run.parse-cues.grammar@74a2d589
#[test]
fn subrip_and_webvtt_read_through_one_grammar() {
    let srt = "1\n00:00:01,000 --> 00:00:02,500\nHello\nthere\n\n2\n00:00:03,000 --> 00:00:04,000\nagain\n";
    let vtt = "WEBVTT\n\n00:01.000 --> 00:02.500 align:start\nHello\nthere\n\nnote-id\n00:00:03.000 --> 00:00:04.000\nagain\n";
    let (a, b) = (parse(srt), parse(vtt));
    assert_eq!(a.cues, b.cues);
    assert_eq!(a.cues[0], Cue { start_ms: 1000, end_ms: 2500, text: "Hello there".into() });
}

/// A block starting before the last accepted block raises `DeriveCueOutOfOrder` and is dropped.
// spec: run.parse-cues.backward-cue@b045bd85
#[test]
fn a_block_starting_before_the_last_accepted_one_is_dropped() {
    let doc = "1\n00:00:05,000 --> 00:00:06,000\nlater\n\n2\n00:00:01,000 --> 00:00:02,000\nearlier\n\n3\n00:00:07,000 --> 00:00:08,000\nlast\n";
    let p = parse(doc);
    assert_eq!(p.cues.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), ["later", "last"]);
    assert!(matches!(&p.defects[..], [RunError::DeriveCueOutOfOrder(_)]));
}

fn cue(start_s: u64, text: &str) -> Cue {
    Cue { start_ms: start_s * 1000, end_ms: start_s * 1000 + 500, text: text.into() }
}

/// A passage stays open while it holds fewer than 600 chars.
// spec: run.parse-cues.passage-chars@ca693ea0
#[test]
fn a_passage_closes_at_600_chars() {
    assert_eq!(PASSAGE_CHARS, 600);
    let word = "x".repeat(299);
    let ps = passages(&[cue(0, &word), cue(1, &word), cue(2, "next"), cue(3, "after")]);
    assert_eq!(ps.len(), 2, "599 chars stay open; the third cue takes it past 600 and closes it");
    assert!(ps[0].text.ends_with("next") && ps[1].text == "after");
    let ps = passages(&[cue(0, &"y".repeat(600)), cue(1, "next")]);
    assert_eq!(ps.len(), 2);
}

/// A passage stays open while it spans less than 60 s.
// spec: run.parse-cues.passage-span@9ace94f1
#[test]
fn a_passage_closes_at_60_seconds() {
    assert_eq!(PASSAGE_SPAN_MS, 60_000);
    let ps = passages(&[cue(0, "a"), cue(30, "b"), cue(59, "c"), cue(61, "d")]);
    assert_eq!(ps.iter().map(|p| p.text.as_str()).collect::<Vec<_>>(), ["a b c d"], "59.5 s spans less than 60");
    let ps = passages(&[cue(0, "a"), cue(60, "b"), cue(61, "c")]);
    assert_eq!(ps.iter().map(|p| p.text.as_str()).collect::<Vec<_>>(), ["a b", "c"]);
}

/// One passage's text holds at most 8 KiB.
// spec: run.parse-cues.passage-bytes@deaa9ec4
#[test]
fn a_passage_holds_at_most_8_kib() {
    assert_eq!(PASSAGE_BYTES, 8192);
    let ps = passages(&[cue(0, &"é".repeat(5000))]);
    assert!(ps[0].text.len() <= 8192 && ps[0].text.len() > 8190);
}

/// One document yields at most 2000 rows of passages.
// spec: run.parse-cues.passages-per-document@7a486c85
#[test]
fn a_document_yields_at_most_2000_passages() {
    assert_eq!(PASSAGES_PER_DOCUMENT, 2000);
    let cues: Vec<Cue> = (0..2100).map(|i| Cue { start_ms: i * 61_000, end_ms: i * 61_000 + 60_000, text: format!("c{i}") }).collect();
    assert_eq!(passages(&cues).len(), 2000);
}

/// Every passage and marker row carries `derivation_key`: lowercase hex SHA-256 over the engine id, the binding
/// less its bounds and `zone`, the output table's declared columns, and the parent row's id, media value and
/// `derivation_key`.
// spec: run.emit.derivation-key@09414d30
#[test]
fn a_derivation_key_hashes_engine_binding_columns_and_parent() {
    let block = "[derive.reader]\ntimeout_secs = 5\nmax_output_bytes = 64\nzone = \"local:device\"\n[derive.reader.engine]\ncommand = [\"cat\", \"{input}\"]\noutput_format = \"srt\"\n";
    let params = |toml: &str| bindings(toml).unwrap().remove("reader").unwrap().derivation_params();
    let d = Derivation { engine_id: "exec:reader@abcdefabcdef".into(), binding: params(block), output_schema: json!({"text": "string"}) };
    let key = d.key("a", "a.txt", None);
    assert_eq!(key.len(), 64);
    assert!(key.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "{key}");
    assert_eq!(key, d.key("a", "a.txt", None), "one derivation, one key");
    let moved = [
        Derivation { engine_id: "exec:reader@000000000000".into(), ..d.clone() }.key("a", "a.txt", None),
        Derivation { binding: params(&block.replace("\"srt\"", "\"vtt\"")), ..d.clone() }.key("a", "a.txt", None),
        Derivation { binding: params(&format!("{block}[derive.reader.env]\nLANG = \"C\"\n")), ..d.clone() }.key("a", "a.txt", None),
        Derivation { output_schema: json!({"text": "string", "speaker": "string"}), ..d.clone() }.key("a", "a.txt", None),
        d.key("b", "a.txt", None),
        d.key("a", "b.txt", None),
        d.key("a", "a.txt", Some(&"f".repeat(64))),
    ];
    for (i, m) in moved.iter().enumerate() {
        assert_ne!(*m, key, "input {i} moves the key");
    }
    for unmoved in [block.replace("timeout_secs = 5", "timeout_secs = 9"), block.replace("local:device", "vendor"), block.replace("max_output_bytes = 64\n", "")] {
        assert_eq!(Derivation { binding: params(&unmoved), ..d.clone() }.key("a", "a.txt", None), key, "{unmoved}");
    }
    // The unit a tick selects carries its key onto every row it lands.
    let parents = rows(json!([{"doc_id": "a", "path": "a.txt"}]));
    let u = select(&parents, &[], &cfg(base()).unwrap(), &d).outstanding.remove(0);
    assert_eq!(u.derivation_key, key);
    let cue = Cue { start_ms: 0, end_ms: 500, text: "hello".into() };
    assert_eq!(passage_rows(&u, &[cue], "e")[0][DERIVATION_KEY], key.as_str());
    assert_eq!(marker_row(&u, UnitStatus::Unavailable, None, true, "e")[DERIVATION_KEY], key.as_str());
    // A derive parent's own key reaches its children's.
    let child = rows(json!([{"doc_id": "a", "path": "a.txt", "derivation_key": "f".repeat(64)}]));
    assert_ne!(select(&child, &[], &cfg(base()).unwrap(), &d).outstanding[0].derivation_key, key);
}

/// Stamp rows as one landing: tick `tick`'s instant, run and row sequence.
fn landed(rows: Vec<Row>, tick: u32) -> Vec<Row> {
    rows.into_iter()
        .enumerate()
        .map(|(i, mut r)| {
            r.insert("_ingested_at".into(), json!(format!("2030-01-01T00:{:02}:00.000000000Z", tick)));
            r.insert("_run_id".into(), json!(format!("r{tick:02}")));
            r.insert("_row_seq".into(), json!(i as i64));
            r
        })
        .collect()
}

fn passage(u: &Unit, text: &str) -> Vec<Row> {
    passage_rows(u, &[Cue { start_ms: 0, end_ms: 500, text: text.into() }], "e")
}

/// Rows under the current key landed before the unit's latest `ok` or `empty` landing under another key count for
/// nothing, so a key changed and changed back derives the unit again.
// spec: run.select.key-change@5cea1a4e
#[test]
fn a_changed_key_rederives_and_a_key_changed_back_derives_again() {
    let c = cfg(base()).unwrap();
    let (old, new) = (under("exec:reader@111111111111"), under("exec:reader@222222222222"));
    let parents = rows(json!([{"doc_id": "a", "path": "p"}]));
    let outstanding = |d: &Derivation, table: &[Row]| select(&parents, table, &c, d).outstanding;
    let mut table = Vec::new();
    table.extend(landed(passage(&outstanding(&old, &table)[0], "first"), 1));
    assert!(outstanding(&old, &table).is_empty(), "a settled unit stays settled under its key");
    let u = outstanding(&new, &table).remove(0);
    assert_eq!((u.prior_attempts, u.derivation_key.clone()), (0, new.key("a", "p", None)), "a changed key re-selects the unit");
    table.extend(landed(vec![marker_row(&u, UnitStatus::Unavailable, None, true, "e")], 2));
    assert_eq!(outstanding(&new, &table)[0].prior_attempts, 1, "attempts count under the new key");
    let u = outstanding(&new, &table).remove(0);
    table.extend(landed(passage(&u, "second"), 3));
    assert!(outstanding(&new, &table).is_empty());
    // Changed back: the first key's passages predate the second key's landing and count for nothing.
    let back = outstanding(&old, &table);
    assert_eq!(back.len(), 1);
    assert_eq!((back[0].prior_attempts, back[0].derivation_key.clone()), (0, old.key("a", "p", None)));
    // A new key that settles on failure leaves the unit settled under it.
    let failing = under("exec:reader@333333333333");
    let mut spent = table.clone();
    for (tick, attempts) in [(4, 0), (5, 1), (6, 2)] {
        let u = Unit { prior_attempts: attempts, ..outstanding(&failing, &spent).remove(0) };
        spent.extend(landed(vec![marker_row(&u, UnitStatus::Failed, Some("x"), true, "e")], tick));
    }
    assert!(outstanding(&failing, &spent).is_empty(), "the third failure under the new key settles it");
}

/// A unit that settled under its current key revives; one whose key changed does not.
#[test]
fn only_a_unit_settled_under_its_current_key_is_a_revival() {
    let (old, new) = (under("exec:reader@111111111111"), under("exec:reader@222222222222"));
    let at = |d: &Derivation| Unit { key: "a".into(), media: "p".into(), prior_attempts: 0, derivation_key: d.key("a", "p", None) };
    let table = landed(passage(&at(&old), "first"), 1);
    assert!(matches!(revived(&at(&old), &table, ATTEMPTS_PER_UNIT), Some(RunError::DeriveSettledUnitRevived(m)) if m.contains("`a`")));
    assert!(revived(&at(&new), &table, ATTEMPTS_PER_UNIT).is_none());
    let retrying = landed(vec![marker_row(&at(&new), UnitStatus::Unavailable, None, true, "e")], 2);
    assert!(revived(&at(&new), &[table.clone(), retrying].concat(), ATTEMPTS_PER_UNIT).is_none(), "an unsettled marker is no settlement");
}

/// Rows under another key landed before a unit's latest `ok` or `empty` landing are superseded; a retry marker
/// supersedes nothing.
#[test]
fn rows_under_an_older_key_are_superseded_once_a_newer_key_lands() {
    let (old, new) = (under("exec:reader@111111111111"), under("exec:reader@222222222222"));
    let at = |d: &Derivation, unit: &str| Unit { key: unit.into(), media: "p".into(), prior_attempts: 0, derivation_key: d.key(unit, "p", None) };
    let mut table = landed([passage(&at(&old, "a"), "old a"), passage(&at(&old, "b"), "old b")].concat(), 1);
    table.extend(landed(vec![marker_row(&at(&new, "a"), UnitStatus::Unavailable, None, true, "e")], 2));
    assert_eq!(superseded(&table), [false, false, false], "a retry leaves the old passages answering");
    table.extend(landed(passage(&at(&new, "a"), "new a"), 3));
    table.extend(landed(vec![marker_row(&at(&new, "b"), UnitStatus::Empty, None, false, "e")], 4));
    assert_eq!(superseded(&table), [true, true, false, false, false], "each unit's old rows yield to its newer landing");
}

/// Re-derive every unit under a changed engine a few units a tick, reading the table between ticks.
#[test]
fn a_changed_engine_rederives_every_unit_and_no_read_between_ticks_is_empty() {
    let c = cfg(with("max_rows_per_run", json!(4))).unwrap();
    let parents = rows(json!((0..10).map(|i| json!({"doc_id": format!("doc{i}"), "path": format!("m{i}")})).collect::<Vec<_>>()));
    let (old, new) = (under("exec:reader@111111111111"), under("exec:reader@222222222222"));
    let (mut table, mut tick) = (Vec::<Row>::new(), 0u32);
    let (mut reads, mut empty_reads) = (0u64, 0u64);
    // Each unit's passage texts, superseded rows left out.
    let read = |table: &[Row]| -> Vec<Vec<String>> {
        let live: Vec<&Row> = table.iter().zip(superseded(table)).filter(|(_, s)| !s).map(|(r, _)| r).collect();
        (0..10)
            .map(|i| live.iter().filter(|r| r["unit_ref"] == format!("doc{i}") && r["kind"] == "passage").map(|r| r["text"].as_str().unwrap().to_string()).collect())
            .collect()
    };
    let mut tick_with = |d: &Derivation, fail: bool, table: &mut Vec<Row>| {
        tick += 1;
        let sel = select(&parents, table, &c, d);
        let rows: Vec<Row> = sel
            .outstanding
            .iter()
            .flat_map(|u| if fail { vec![marker_row(u, UnitStatus::Unavailable, None, true, "e")] } else { passage(u, &format!("{} {}", u.derivation_key, u.key)) })
            .collect();
        table.extend(landed(rows, tick));
        sel.outstanding.len()
    };
    while tick_with(&old, false, &mut table) > 0 {}
    assert!(read(&table).iter().all(|p| p.len() == 1));
    // The engine changes: every settled unit is outstanding again.
    let all = DeriveConfig { max_rows_per_run: 100, ..c.clone() };
    assert_eq!(select(&parents, &table, &all, &new).outstanding.len(), 10, "an argument change re-selects every unit");
    tick_with(&new, true, &mut table);
    loop {
        for p in read(&table) {
            reads += 1;
            empty_reads += u64::from(p.is_empty());
        }
        if tick_with(&new, false, &mut table) == 0 {
            break;
        }
    }
    let now = read(&table);
    for (i, p) in now.iter().enumerate() {
        assert_eq!(p, &[format!("{} doc{i}", new.key(&format!("doc{i}"), &format!("m{i}"), None))], "doc{i} answers from the new engine alone");
    }
    // A parent whose media value changes re-selects that unit alone.
    let mut moved = parents.clone();
    moved[3].insert("path".into(), json!("m3-corrected"));
    let again: Vec<String> = select(&moved, &table, &all, &new).outstanding.into_iter().map(|u| u.key).collect();
    assert_eq!(again, ["doc3"]);
    assert_eq!(empty_reads, 0);
    crate::emit("derivation-freshness", empty_reads as f64, reads, 0);
}
