//! Host derive tasks: registration, the key a host unit derives under, the rows it lands
//! across its marker and content tables, and supersession within a retained version.

use contextful_core::run::RunError;
use contextful_core::run::derive::config::DeriveConfig;
use contextful_core::run::derive::emit::{UnitStatus, superseded, superseded_within_version};
use contextful_core::run::derive::task::{
    DeriveTask, Derived, HostUnit, Join, TASK_VERSION, Tasks, check_host_tables, host_key,
    host_rows, join_units, landing_order, select_host,
};
use contextful_core::run::ports::Row;
use contextful_core::store::declare::TableDecl;
use serde_json::{Value, json};
use std::collections::BTreeSet;
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
        joined: Default::default(),
        prior_attempts: 0,
    }
}

fn decl(name: &str, pk: &[&str]) -> TableDecl {
    TableDecl {
        primary_key: Some(pk.iter().map(|s| s.to_string()).collect()),
        ..TableDecl::named(name)
    }
}

/// An embedding binary registers each compiled derive task by name before build. Configuration selects that registered
/// name without supplying a command. A duplicate built-in or registered task name raises `DeriveTaskNameTaken`.
// spec: run.bind.host-task@bf2d17d0
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
    assert_eq!(out.keys().collect::<Vec<_>>(), ["stats", "units", "words"]);
    assert!(
        ["stats", "words"]
            .iter()
            .all(|t| out[*t].len() == 1 && out[*t][0]["kind"] == json!("marker")),
        "no content row lands: {out:?}"
    );
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

/// Stamp `rows` as landed by run `run` at hour `hour`, in order.
fn landed_at(rows: &[Row], run: &str, hour: u32) -> Vec<Row> {
    rows.iter()
        .enumerate()
        .map(|(i, r)| {
            let mut r = r.clone();
            r.insert(
                "_ingested_at".into(),
                json!(format!("2030-01-01T{hour:02}:00:00Z")),
            );
            r.insert("_run_id".into(), json!(run));
            r.insert("_row_seq".into(), json!(i));
            r
        })
        .collect()
}

/// A host unit landing no row in a content table lands one `kind` `marker`, `unit_status` `empty` row there under its key, so {{run.emit.stale-supersedes}} holds in every content table.
#[test]
fn a_content_table_a_unit_lands_nothing_in_takes_an_empty_marker() {
    struct Partial;
    impl DeriveTask for Partial {
        fn version(&self) -> &str {
            "2"
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
        fn derive(&self, _unit: &HostUnit) -> Result<Derived, RunError> {
            let mut out = Derived::new();
            out.insert(
                "words".into(),
                vec![row(json!({"word_seq": 0, "word": "alpha"}))],
            );
            Ok(out)
        }
    }
    // Version 1 lands d1 in both content tables.
    let v1 = unit("d1", "alpha beta", "1");
    let mut before = Derived::new();
    before.insert(
        "words".into(),
        vec![row(json!({"word_seq": 0, "word": "alpha"}))],
    );
    before.insert("stats".into(), vec![row(json!({"word_count": 2}))]);
    let first = host_rows(&v1, "split", &Split { version: "1" }, Ok(before));

    // A blank body derives nothing: each content table takes one `empty` marker.
    let blank = unit("d1", "  ", "1");
    let task = Split { version: "1" };
    let out = host_rows(&blank, "split", &task, task.derive(&blank));
    for t in ["words", "stats"] {
        assert_eq!(out[t].len(), 1, "{t}");
        let m = &out[t][0];
        assert_eq!(
            (
                &m["unit_ref"],
                &m["derivation_key"],
                &m[TASK_VERSION],
                &m["kind"],
                &m["unit_status"]
            ),
            (
                &json!("d1"),
                &json!(blank.derivation_key),
                &json!("1"),
                &json!("marker"),
                &json!("empty")
            ),
            "{t}"
        );
        let table = [landed_at(&first[t], "r1", 1), landed_at(&out[t], "r2", 2)].concat();
        let flags = superseded(&table);
        assert!(!flags[..first[t].len()].is_empty(), "the exclusion below ranges over no element");
        assert!(
            flags[..first[t].len()].iter().all(|s| *s),
            "{t}: the earlier key yields to the empty landing"
        );
        assert!(!flags[first[t].len()], "{t}: the empty landing stands");
    }
    assert_eq!(out["units"][0]["unit_status"], json!("empty"));

    // A unit returning rows for `words` alone lands the empty marker in `stats`.
    let v2 = unit("d1", "alpha beta", "2");
    let out = host_rows(&v2, "split", &Partial, Partial.derive(&v2));
    assert_eq!(
        (out["words"][0]["kind"].clone(), out["words"].len()),
        (json!("passage"), 1)
    );
    assert_eq!(
        (
            out["stats"][0]["kind"].clone(),
            out["stats"][0]["unit_status"].clone()
        ),
        (json!("marker"), json!("empty"))
    );
    assert_eq!(out["units"][0]["unit_status"], json!("ok"));
    let stats = [
        landed_at(&first["stats"], "r1", 1),
        landed_at(&out["stats"], "r2", 2),
    ]
    .concat();
    assert_eq!(
        superseded(&stats),
        [true, false],
        "the earlier stats row stops answering"
    );
    assert_eq!(
        superseded_within_version(&stats),
        [false, false],
        "a retaining table keeps version 1 under its own version"
    );

    // A failed unit lands its marker alone and supersedes nothing.
    let failed = host_rows(
        &v2,
        "split",
        &Partial,
        Err(RunError::Invalid("no body".into())),
    );
    assert_eq!(failed.keys().collect::<Vec<_>>(), ["units"]);
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

/// A host task's unit carries, per table the task joins, every row whose join column equals the unit's key, each joined table read once after selection and before the first unit derives, so a row landing later is absent.
#[test]
fn a_unit_carries_every_joined_row_sharing_its_key() {
    let mut units = vec![unit("d1", "alpha", "1"), unit("d2", "beta", "1"), unit("7", "gamma", "1")];
    let notes = Join { table: "notes".into(), key_column: "doc_id".into(), columns: vec!["note".into()] };
    let tags = Join { table: "tags".into(), key_column: "doc".into(), columns: vec!["tag".into()] };
    join_units(&mut units, &notes, rows(json!([
        {"doc_id": "d1", "note": "first"},
        {"doc_id": "d2", "note": "second"},
        {"doc_id": "d1", "note": "third"},
        {"doc_id": null, "note": "orphan"},
        {"doc_id": "  ", "note": "blank"},
        {"doc_id": 7, "note": "numeric"},
    ])));
    join_units(&mut units, &tags, rows(json!([{"doc": "d2", "tag": "red"}])));
    let notes_of = |u: &HostUnit| u.joined["notes"].iter().map(|r| r["note"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(notes_of(&units[0]), ["first", "third"]);
    assert_eq!(notes_of(&units[1]), ["second"]);
    assert_eq!(notes_of(&units[2]), ["numeric"], "a numeric join value matches the unit key's text");
    assert!(units[0].joined["tags"].is_empty(), "a unit sharing no joined row holds an empty list");
    assert_eq!(units[1].joined["tags"], rows(json!([{"doc": "d2", "tag": "red"}])));
    assert_eq!(notes.read_columns(), ["doc_id", "note"]);
}

struct Edges;

impl DeriveTask for Edges {
    fn version(&self) -> &str {
        "1"
    }
    fn columns(&self) -> Vec<String> {
        Vec::new()
    }
    fn joins(&self) -> Vec<Join> {
        ["from_id", "to_id"]
            .into_iter()
            .map(|key| Join { table: "edges".into(), key_column: key.into(), columns: vec!["weight".into()] })
            .collect()
    }
    fn marker_table(&self) -> String {
        "units".into()
    }
    fn content_tables(&self) -> Vec<String> {
        vec!["degrees".into()]
    }
    fn derive(&self, _unit: &HostUnit) -> Result<Derived, RunError> {
        Ok(Derived::new())
    }
}

/// A host task whose joins name one table twice raises `DeriveJoinRepeated` at registration, since its unit holds one joined list per table.
// spec: run.bind.join-repeated@e62036c9
#[test]
fn a_task_joining_one_table_twice_refuses_at_registration() {
    let mut t = Tasks::default();
    match t.register("edges", Arc::new(Edges)) {
        Err(RunError::DeriveJoinRepeated(m)) => assert!(m.contains("edges") && m.contains("from_id") && m.contains("to_id"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(t.names().is_empty());
}

fn sampling(rate: Value, budget: i64) -> DeriveConfig {
    DeriveConfig::parse_with(
        "docs",
        &json!({"task": "split", "source_table": "documents", "parent_id_column": "doc_id", "sample_rate": rate, "max_rows_per_run": budget}),
        &tasks("1"),
    )
    .unwrap()
}

fn selected(sel: &contextful_core::run::derive::task::HostSelection) -> BTreeSet<String> {
    sel.outstanding.iter().map(|u| u.key.clone()).collect()
}

/// `sample_rate` r keeps a host unit before the anti-join when the first eight bytes of SHA-256 over its key then task
/// name, read big-endian and divided by 2^64, fall below r; r outside `(0, 1]` refuses as {{run.declare.spec-invalid}}.
// spec: run.select.sample@6ccfb5a4
#[test]
fn a_sample_rate_selects_one_hashed_set_on_every_pass_and_on_resume() {
    use sha2::{Digest, Sha256};
    let parents: Vec<Row> = (0..10_000).map(|i| row(json!({"doc_id": format!("d{i}"), "body": "a"}))).collect();
    let task = Split { version: "1" };
    let first = select_host(&parents, &[], &sampling(json!(0.25), 10_000), &task);
    let set = selected(&first);
    assert!(!set.is_empty(), "the sample holds units");
    assert!((2_450..=2_550).contains(&set.len()), "a quarter of 10,000 within 2%: {}", set.len());
    assert_eq!(first.counts.eligible, set.len() as u64, "a unit the rate drops is no eligible unit");
    let hashed: BTreeSet<String> = (0..10_000)
        .map(|i| format!("d{i}"))
        .filter(|key| {
            let digest = Sha256::digest(format!("{key}split").as_bytes());
            let head = u64::from_be_bytes(digest[..8].try_into().unwrap());
            (head as f64) / 18_446_744_073_709_551_616.0 < 0.25
        })
        .collect();
    assert_eq!(set, hashed, "membership is the hash of unit key then task name");
    assert_eq!(
        selected(&select_host(&parents, &[], &sampling(json!(0.25), 10_000), &task)),
        set,
        "a second pass selects the same set"
    );

    // A resumed pass: 500 units per tick, each tick landing its markers before the next selects.
    let mut markers: Vec<Row> = Vec::new();
    let mut resumed = BTreeSet::new();
    for _ in 0..10 {
        let sel = select_host(&parents, &markers, &sampling(json!(0.25), 500), &task);
        assert!(sel.outstanding.len() <= 500);
        for unit in &sel.outstanding {
            assert!(resumed.insert(unit.key.clone()), "a settled unit is selected once");
            markers.extend(host_rows(unit, "split", &task, Ok(Derived::new()))["units"].clone());
        }
    }
    assert_eq!(resumed, set, "ticks resumed over landed markers select the same set");
    assert!(select_host(&parents, &markers, &sampling(json!(0.25), 500), &task).outstanding.is_empty());

    let whole = select_host(&parents, &[], &sampling(json!(1), 10_000), &task);
    assert_eq!(selected(&whole).len(), 10_000, "a rate of 1 keeps every unit");
}

/// A `sample_rate` outside `(0, 1]` and a `since` that is no RFC 3339 instant refuse as `PipelineSpecInvalid`; a
/// built-in task reads neither key.
// spec: run.select.sample@6ccfb5a4
// spec: run.select.since@3c276278
#[test]
fn a_rate_outside_the_unit_interval_and_an_unreadable_since_refuse() {
    let t = tasks("1");
    let parse = |extra: Value| {
        let mut cfg = json!({"task": "split", "source_table": "documents", "parent_id_column": "doc_id"});
        cfg.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        DeriveConfig::parse_with("docs", &cfg, &t)
    };
    for rate in [json!(0), json!(0.0), json!(-0.25), json!(1.5), json!("0.5"), json!(true)] {
        match parse(json!({"sample_rate": rate})) {
            Err(RunError::PipelineSpecInvalid(m)) => assert!(m.contains("sample_rate") && m.contains("(0, 1]"), "{m}"),
            other => panic!("{rate}: {other:?}"),
        }
    }
    for since in [json!("yesterday"), json!("2030-01-01 00:00:00Z"), json!(1_700_000_000)] {
        match parse(json!({"since": since})) {
            Err(RunError::PipelineSpecInvalid(m)) => assert!(m.contains("since"), "{m}"),
            other => panic!("{since}: {other:?}"),
        }
    }
    assert!(parse(json!({"sample_rate": 1, "since": "2030-01-01T00:00:00Z"})).is_ok());
    let built_in = json!({"engine": "w", "source_table": "documents", "media_column": "path", "parent_id_column": "doc_id", "sample_rate": 0.5});
    assert!(matches!(
        DeriveConfig::parse("docs", &built_in),
        Err(RunError::PipelineUnknownConfigKey(m)) if m.contains("sample_rate")
    ));
}
