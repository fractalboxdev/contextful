//! Host derive tasks: registration, the key a host unit derives under, the rows it lands
//! across its marker and content tables, and supersession within a retained version.

use contextful_core::run::RunError;
use contextful_core::run::derive::config::DeriveConfig;
use contextful_core::run::derive::emit::{UnitStatus, superseded, superseded_within_version};
use contextful_core::run::derive::task::{
    DeriveTask, Derived, HostUnit, TASK_VERSION, Tasks, check_host_tables, host_key, host_rows,
    landing_order, select_host,
};
use contextful_core::run::ports::Row;
use contextful_core::store::declare::TableDecl;
use serde_json::{Value, json};
use std::sync::Arc;

struct Split {
    version: &'static str,
}

impl DeriveTask for Split {
    fn version(&self) -> &str {
        self.version
    }
    fn columns(&self) -> Vec<String> {
        vec!["body".into()]
    }
    fn marker_table(&self) -> String {
        "units".into()
    }
    fn content_tables(&self) -> Vec<String> {
        vec!["words".into(), "stats".into()]
    }
    fn derive(&self, unit: &HostUnit) -> Result<Derived, RunError> {
        let body = unit
            .row
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut out = Derived::new();
        for (i, w) in body.split_whitespace().enumerate() {
            out.entry("words".into())
                .or_default()
                .push(row(json!({"word_seq": i, "word": w})));
        }
        Ok(out)
    }
}

fn row(v: Value) -> Row {
    v.as_object().unwrap().clone()
}

fn rows(v: Value) -> Vec<Row> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_object().unwrap().clone())
        .collect()
}

fn tasks(version: &'static str) -> Tasks {
    let mut t = Tasks::default();
    t.register("split", Arc::new(Split { version })).unwrap();
    t
}

fn config(t: &Tasks) -> DeriveConfig {
    DeriveConfig::parse_with(
        "docs",
        &json!({"task": "split", "source_table": "documents", "parent_id_column": "doc_id"}),
        t,
    )
    .unwrap()
}

fn unit(key: &str, body: &str, version: &str) -> HostUnit {
    let r = row(json!({"doc_id": key, "body": body}));
    HostUnit {
        key: key.into(),
        derivation_key: host_key("split", version, key, &r, &["body".into()]),
        row: r,
        prior_attempts: 0,
    }
}

fn decl(name: &str, pk: &[&str]) -> TableDecl {
    TableDecl {
        primary_key: Some(pk.iter().map(|s| s.to_string()).collect()),
        ..TableDecl::named(name)
    }
}

/// An embedding binary registers each compiled derive task under a name before build, and configuration names that symbol, never a command; a name repeating a built-in or registered task raises `DeriveTaskNameTaken`.
// spec: run.bind.host-task@7cdec6b7
#[test]
fn a_registered_task_resolves_by_name_and_a_taken_name_refuses() {
    let mut t = tasks("1");
    assert_eq!(t.names(), ["split"]);
    assert_eq!(config(&t).task.name(), "split");
    assert!(config(&t).task.is_host());
    for taken in ["transcribe", "link_preview", "split", ""] {
        match t.register(taken, Arc::new(Split { version: "1" })) {
            Err(RunError::DeriveTaskNameTaken(m)) => {
                assert!(m.contains("transcribe") && m.contains("split"), "{m}")
            }
            other => panic!("{taken}: {other:?}"),
        }
    }
    // Configuration names a symbol; a command key still refuses.
    let with_command = json!({"task": "split", "source_table": "documents", "parent_id_column": "doc_id", "command": ["sh"]});
    assert!(matches!(
        DeriveConfig::parse_with("docs", &with_command, &t),
        Err(RunError::DeriveCommandInManifest(_))
    ));
}

/// A host task's `derivation_key` hashes its registered name, its `task_version` and the parent row's id, declared columns and `derivation_key`, so raising the version re-selects every settled unit.
// spec: run.emit.task-version@192be1dd
#[test]
fn raising_the_task_version_re_selects_every_settled_unit() {
    let r = row(json!({"doc_id": "d1", "body": "a b", "other": 1}));
    let cols = ["body".to_string()];
    let base = host_key("split", "1", "d1", &r, &cols);
    assert_eq!(base.len(), 64);
    assert_ne!(
        base,
        host_key("split", "2", "d1", &r, &cols),
        "the version is hashed"
    );
    assert_ne!(
        base,
        host_key("other", "1", "d1", &r, &cols),
        "the task name is hashed"
    );
    assert_ne!(
        base,
        host_key("split", "1", "d1", &row(json!({"body": "a c"})), &cols),
        "a declared column is hashed"
    );
    assert_eq!(
        base,
        host_key(
            "split",
            "1",
            "d1",
            &row(json!({"body": "a b", "other": 2})),
            &cols
        ),
        "an undeclared column is not"
    );
    assert_ne!(
        base,
        host_key(
            "split",
            "1",
            "d1",
            &row(json!({"body": "a b", "derivation_key": "p"})),
            &cols
        ),
        "the parent's key is hashed"
    );

    let v1 = tasks("1");
    let parents = rows(json!([{"doc_id": "d1", "body": "a b"}, {"doc_id": "d2", "body": ""}]));
    let settled: Vec<Row> = ["d1", "d2"]
        .iter()
        .map(|d| row(json!({"unit_ref": d, "kind": "marker", "unit_status": "ok", "attempts": 1, "derivation_key": unit(d, if *d == "d1" { "a b" } else { "" }, "1").derivation_key})))
        .collect();
    let split1 = Split { version: "1" };
    assert!(
        select_host(&parents, &settled, &config(&v1), &split1)
            .outstanding
            .is_empty(),
        "every unit settled under version 1"
    );
    let split2 = Split { version: "2" };
    let again = select_host(&parents, &settled, &config(&tasks("2")), &split2);
    assert_eq!(
        again
            .outstanding
            .iter()
            .map(|u| u.key.as_str())
            .collect::<Vec<_>>(),
        ["d1", "d2"]
    );
    assert_eq!(
        again.outstanding[0].derivation_key,
        unit("d1", "a b", "2").derivation_key
    );
}

/// A host-task pipeline declares exactly its task's marker and content tables, and a unit returns rows for its content tables alone; either breach raises `DeriveOutputTablesMismatch`, the second failing that unit alone.
// spec: run.emit.output-tables@f783e3dd
#[test]
fn tables_outside_the_tasks_set_refuse() {
    let task = Split { version: "1" };
    let marker = decl("units", &["unit_ref", "derivation_key", "cue_seq"]);
    let words = decl("words", &["unit_ref", "derivation_key", "word_seq"]);
    let stats = decl("stats", &["unit_ref", "derivation_key"]);
    assert!(
        check_host_tables(
            "docs",
            "split",
            &task,
            &[words.clone(), stats.clone(), marker.clone()]
        )
        .is_ok()
    );
    for tables in [
        vec![words.clone(), marker.clone()],
        vec![
            words.clone(),
            stats.clone(),
            marker.clone(),
            decl("extra", &["unit_ref", "derivation_key"]),
        ],
        vec![words.clone(), stats.clone(), stats.clone(), marker.clone()],
    ] {
        assert!(matches!(
            check_host_tables("docs", "split", &task, &tables),
            Err(RunError::DeriveOutputTablesMismatch(_))
        ));
    }
    let u = unit("d1", "a", "1");
    let mut stray = Derived::new();
    stray.insert("elsewhere".into(), vec![row(json!({"x": 1}))]);
    let out = host_rows(&u, "split", &task, Ok(stray));
    assert_eq!(
        out.keys().collect::<Vec<_>>(),
        ["units"],
        "the unit lands its marker alone"
    );
    let m = &out["units"][0];
    assert_eq!(m["unit_status"], json!("failed"));
    assert!(
        m["last_error"]
            .as_str()
            .unwrap()
            .contains("DeriveOutputTablesMismatch"),
        "{m:?}"
    );
}

/// A host task's content row carries its unit's `unit_ref`, `derivation_key` and `task_version`, `kind` `passage` and `unit_status` `ok`; every unit lands one marker, `ok` over content rows, `empty` over none.
// spec: run.emit.host-rows@4e0663fe
#[test]
fn a_unit_lands_stamped_content_rows_and_one_marker() {
    let task = Split { version: "3" };
    let u = unit("d1", "alpha beta", "3");
    let out = host_rows(&u, "split", &task, task.derive(&u));
    assert_eq!(out["words"].len(), 2);
    for r in &out["words"] {
        assert_eq!(
            (
                &r["unit_ref"],
                &r["derivation_key"],
                &r[TASK_VERSION],
                &r["kind"],
                &r["unit_status"]
            ),
            (
                &json!("d1"),
                &json!(u.derivation_key),
                &json!("3"),
                &json!("passage"),
                &json!("ok")
            )
        );
    }
    let marker = &out["units"];
    assert_eq!(marker.len(), 1);
    assert_eq!(
        (
            &marker[0]["unit_status"],
            &marker[0]["cue_seq"],
            &marker[0]["kind"],
            &marker[0]["engine_id"]
        ),
        (
            &json!("ok"),
            &json!(-1),
            &json!("marker"),
            &json!("host:split@3")
        )
    );

    let empty = unit("d2", "   ", "3");
    let out = host_rows(&empty, "split", &task, task.derive(&empty));
    assert_eq!(out.keys().collect::<Vec<_>>(), ["units"]);
    assert_eq!(
        (
            &out["units"][0]["unit_status"],
            &out["units"][0]["attempts"]
        ),
        (&json!(UnitStatus::Empty.name()), &json!(1))
    );

    let failed = host_rows(&u, "split", &task, Err(RunError::Invalid("no body".into())));
    assert_eq!(
        (
            &failed["units"][0]["unit_status"],
            &failed["units"][0]["retryable"]
        ),
        (&json!("failed"), &json!(true))
    );
    assert_eq!(
        landing_order(&task),
        ["words", "stats", "units"],
        "the marker table lands last"
    );
}

/// A retaining table supersedes a row only within its own task version.
#[test]
fn a_retaining_table_supersedes_within_a_version_alone() {
    let landed = rows(json!([
        {"unit_ref": "d1", "derivation_key": "k1", "kind": "passage", "task_version": "1", "_ingested_at": "2030-01-01T01:00:00Z", "_run_id": "r1", "_row_seq": 0},
        {"unit_ref": "d1", "derivation_key": "k2", "kind": "passage", "task_version": "2", "_ingested_at": "2030-01-01T02:00:00Z", "_run_id": "r2", "_row_seq": 0},
        {"unit_ref": "d1", "derivation_key": "k3", "kind": "passage", "task_version": "2", "_ingested_at": "2030-01-01T03:00:00Z", "_run_id": "r3", "_row_seq": 0},
    ]));
    assert_eq!(
        superseded(&landed),
        [true, true, false],
        "without retention every older key yields"
    );
    assert_eq!(
        superseded_within_version(&landed),
        [false, true, false],
        "version 1 stays current under its own version"
    );
}
