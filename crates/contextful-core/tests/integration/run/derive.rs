//! The derive tier's domain: configuration, binding, selection, rows and the cue grammar.

use contextful_core::run::derive::config::{bind, bindings, check_env_name, check_output_table, DeriveConfig, Task, ATTEMPTS_PER_UNIT, LINK_SECONDS_PER_RUN, ROWS_PER_RUN};
use contextful_core::run::derive::cues::{parse, passages, Cue, PASSAGES_PER_DOCUMENT, PASSAGE_BYTES, PASSAGE_CHARS, PASSAGE_SPAN_MS};
use contextful_core::run::derive::emit::{marker_row, select, Unit, UnitStatus, EMPTY_ATTEMPTS};
use contextful_core::run::derive::exec::{engine_id, excerpt, CAPTURED_OUTPUT_BYTES, CHAIN_DEADLINE_SECS, STEP_ERROR_EXCERPT_BYTES};
use contextful_core::run::ports::Row;
use contextful_core::run::RunError;
use contextful_core::store::declare::TableDecl;
use serde_json::{json, Value};

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

/// An absent or blank `engine`, `source_table`, `media_column` or `parent_id_column` raises
/// `DeriveConfigKeyMissing`, naming the key and the pipeline.
// spec: run.select.required-key@d3d60868
#[test]
fn a_missing_or_blank_required_key_refuses() {
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
    let derived = rows(json!([{"unit_ref": "a", "cue_seq": 0, "kind": "passage"}]));
    let keys: Vec<String> = select(&parents, &derived, &c).outstanding.into_iter().map(|u| u.key).collect();
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
        {"unit_ref": "a", "cue_seq": -1, "kind": "marker", "unit_status": "failed", "attempts": 2, "retryable": true},
        {"unit_ref": "b", "cue_seq": -1, "kind": "marker", "unit_status": "failed", "attempts": 3, "retryable": true}
    ]));
    let out = select(&parents, &derived, &c).outstanding;
    assert_eq!(out, [Unit { key: "a".into(), media: "a".into(), prior_attempts: 2 }], "the third failure settles `b`");
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

/// Each tick recomputes the outstanding set: every parent row holding neither a passage nor a settled marker in
/// the pipeline's own output table.
// spec: run.select.anti-join@74eedf3f
#[test]
fn the_outstanding_set_is_every_parent_without_passages_or_a_settled_marker() {
    let c = cfg(base()).unwrap();
    let parents = rows(json!([
        {"doc_id": "done", "path": "p"}, {"doc_id": "empty", "path": "p"}, {"doc_id": "retry", "path": "p"},
        {"doc_id": "permanent", "path": "p"}, {"doc_id": "new", "path": "p"}, {"doc_id": "new", "path": "p"}, {"doc_id": " ", "path": "p"}
    ]));
    let derived = rows(json!([
        {"unit_ref": "done", "cue_seq": 0, "kind": "passage"},
        {"unit_ref": "empty", "cue_seq": -1, "kind": "marker", "unit_status": "empty", "attempts": 1},
        {"unit_ref": "retry", "cue_seq": -1, "kind": "marker", "unit_status": "unavailable", "attempts": 1},
        {"unit_ref": "permanent", "cue_seq": -1, "kind": "marker", "unit_status": "failed", "attempts": 1, "retryable": false}
    ]));
    let sel = select(&parents, &derived, &c);
    assert_eq!(sel.outstanding.iter().map(|u| u.key.as_str()).collect::<Vec<_>>(), ["retry", "new"]);
    assert!(matches!(&sel.incomplete[..], [RunError::DeriveUnitIncomplete(_)]));
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

/// `task` is `transcribe`, the default, or `link_preview`; any other value raises `DeriveUnknownTask`, printing
/// both.
// spec: run.bind.unknown-task@ae19fcd9
#[test]
fn a_task_outside_the_pair_refuses_printing_both() {
    match cfg(with("task", json!("summarize"))) {
        Err(RunError::DeriveUnknownTask(m)) => assert!(m.contains("transcribe") && m.contains("link_preview") && m.contains("summarize"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(cfg(base()).unwrap().task, Task::Transcribe, "transcribe is the default");
}

/// `driver` is `exec` or `"none"` behind `transcribe` and `fetch` behind `link_preview`; any other pairing raises
/// `DeriveDriverMismatch`.
// spec: run.bind.driver-mismatch@02fc3d61
#[test]
fn a_driver_the_task_does_not_serve_refuses() {
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

/// `attempts` is the prior count plus one; `unavailable` and `failed` stop at {{run.select.attempts-per-unit}},
/// and `empty` receives 1 attempts in total.
// spec: run.emit.attempts@a9d5a2e1
#[test]
fn attempts_count_up_from_the_prior_and_an_empty_unit_takes_one() {
    assert_eq!(EMPTY_ATTEMPTS, 1);
    let unit = Unit { key: "a".into(), media: "a".into(), prior_attempts: 2 };
    assert_eq!(marker_row(&unit, UnitStatus::Failed, Some("x"), true, "e")["attempts"], 3);
    assert_eq!(marker_row(&unit, UnitStatus::Unavailable, None, true, "e")["attempts"], 3);
    assert_eq!(marker_row(&unit, UnitStatus::Empty, None, false, "e")["attempts"], 1);
}

/// A unit yielding no passage lands one marker row, `cue_seq` -1 and `kind` `marker`, carrying its status,
/// attempts, last error and whether it retries.
// spec: run.emit.marker-row@f1869530
#[test]
fn a_unit_without_passages_lands_one_marker_row() {
    let unit = Unit { key: "d3".into(), media: "docs/lost.txt".into(), prior_attempts: 0 };
    let m = marker_row(&unit, UnitStatus::Failed, Some("fetching https://user:pw@host.example/a?token=1 failed"), true, "exec:r@abc");
    assert_eq!(m["cue_seq"], -1);
    assert_eq!(m["kind"], "marker");
    assert_eq!(m["unit_status"], "failed");
    assert_eq!(m["retryable"], true);
    assert_eq!(m["last_error"], "fetching https://host.example/a failed", "the address is redacted");
}

/// A derive output table without `primary_key` `["unit_ref", "cue_seq"]` raises `DerivePrimaryKeyMissing`.
// spec: run.emit.primary-key@a32ff217
#[test]
fn a_derive_table_keys_on_unit_and_sequence() {
    let keyed = |pk: &[&str]| TableDecl { primary_key: Some(pk.iter().map(|s| s.to_string()).collect()), ..TableDecl::named("doc_text_passages") };
    assert!(check_output_table(&keyed(&["unit_ref", "cue_seq"])).is_ok());
    for pk in [&["unit_ref"][..], &["cue_seq", "unit_ref"], &[]] {
        assert!(matches!(check_output_table(&keyed(pk)), Err(RunError::DerivePrimaryKeyMissing(_))), "{pk:?}");
    }
}

/// A derive table declaring a column named `kind` raises `DeriveReservedDiscriminator`.
// spec: run.emit.reserved-discriminator@6d178a29
#[test]
fn a_derive_table_naming_kind_refuses() {
    let t = TableDecl { primary_key: Some(vec!["unit_ref".into(), "cue_seq".into()]), cluster_by: Some(vec!["kind".into()]), ..TableDecl::named("t") };
    assert!(matches!(check_output_table(&t), Err(RunError::DeriveReservedDiscriminator(_))));
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
