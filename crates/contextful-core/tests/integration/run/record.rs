//! `run.record`: statuses, the owner lease, the site id, history windows and the error cap.

use super::{at, row};
use contextful_core::run::project::{Snapshot, Version};
use contextful_core::run::record::{
    cap_error, check_site_id, describe_ceiling, export_ceiling, parse_bound, resolve_site_id, select_history, Owner, RunStatus,
    SiteIdSources, Window, DESCRIBE_CEILING_ROWS, DESCRIBE_DEFAULT_ROWS, ERROR_CAP_BYTES, ERROR_ELLIPSIS, EXPORT_CEILING_ROWS,
    EXPORT_DEFAULT_ROWS, OWNER_LEASE_RENEWAL_SECS, OWNER_LEASE_TTL_SECS, SITE_ID_MAX_LEN, SITE_ID_MIN_LEN,
};
use contextful_core::run::RunError;

/// A run status is `pending`, `running`, `waiting`, `success`, `partial_failure`, `failed` or `canceled`, spelled
/// identically on the record and the wire snapshot; every surface classifying a run covers all seven.
// spec: run.record.status-set@0d803c11
#[test]
fn seven_statuses_spelled_once_for_record_and_wire() {
    let names: Vec<&str> = RunStatus::ALL.iter().map(|s| s.name()).collect();
    assert_eq!(names, ["pending", "running", "waiting", "success", "partial_failure", "failed", "canceled"]);
    for s in RunStatus::ALL {
        let mut r = row("run-1", "2030-01-01T00:00:00Z");
        r.status = s;
        let record = serde_json::to_value(&r).unwrap();
        let mut snap = Snapshot::new("run-1", "feed", Version { epoch: at("2030-01-01T00:00:00Z"), counter: 1 });
        snap.status = s;
        let wire: serde_json::Value = serde_json::from_str(&snap.to_wire()).unwrap();
        assert_eq!(record["status"], s.name());
        assert_eq!(wire["status"], s.name(), "the wire snapshot spells {s} as the record does");
        assert_eq!(RunStatus::parse(s.name()), Some(s));
        // Every classifier answers for every status.
        assert_ne!(s.is_terminal(), s.is_in_flight());
    }
    let terminal: Vec<_> = RunStatus::ALL.into_iter().filter(|s| s.is_terminal()).collect();
    assert_eq!(terminal, [RunStatus::Success, RunStatus::PartialFailure, RunStatus::Failed, RunStatus::Canceled]);
}

/// `canceled` is a terminal status apart from `failed`, and upstream health observations skip it.
// spec: run.cancel.distinct-terminal-status@a325974f
#[test]
fn canceled_is_terminal_distinct_and_unobserved_by_health() {
    assert!(RunStatus::Canceled.is_terminal());
    assert_ne!(RunStatus::Canceled, RunStatus::Failed);
    assert_ne!(RunStatus::Canceled.name(), RunStatus::Failed.name());
    let observed: Vec<_> = RunStatus::ALL.into_iter().filter(|s| s.observed_by_health()).collect();
    assert!(observed.contains(&RunStatus::Failed));
    assert!(!observed.contains(&RunStatus::Canceled));
    assert_eq!(observed.len(), 6);
}

/// A `running` or `waiting` row carries an owner of process id, boot id and a lease expiry, renewed every 10 s
/// with a 30 s time-to-live.
// spec: run.record.owner-lease@de4a8fe1
#[test]
fn an_owner_lease_lives_30_s_and_renews_every_10_s() {
    assert_eq!((OWNER_LEASE_RENEWAL_SECS, OWNER_LEASE_TTL_SECS), (10, 30));
    let t0 = at("2030-01-01T00:00:00Z");
    let o = Owner::leased(4242, "boot-a", t0);
    assert_eq!((o.pid, o.boot_id.as_str()), (4242, "boot-a"));
    assert_eq!(o.lease_expires_at, t0.plus_secs(30));
    assert!(!o.expired(t0.plus_secs(29)));
    assert!(o.expired(t0.plus_secs(30)));
    assert!(!Owner::renewal_due(t0, t0.plus_secs(9)));
    assert!(Owner::renewal_due(t0, t0.plus_secs(10)));
    let renewed = o.renewed(t0.plus_secs(10));
    assert_eq!(renewed.lease_expires_at, t0.plus_secs(40));
    assert!(!renewed.expired(t0.plus_secs(35)));
}

/// Startup marks `partial_failure` only a non-terminal row whose owner lease has expired; a row held by a live
/// process is left alone.
// spec: run.record.orphan-reap@065824ad
#[test]
fn only_an_in_flight_row_with_a_lapsed_lease_is_reaped() {
    let t0 = at("2030-01-01T00:00:00Z");
    let mut r = row("run-1", "2030-01-01T00:00:00Z");
    r.owner = Some(Owner::leased(1, "boot", t0));
    let (mut live_reaped, mut rows) = (0u64, 0u64);
    for s in [RunStatus::Running, RunStatus::Waiting, RunStatus::Pending] {
        r.status = s;
        rows += 1;
        live_reaped += u64::from(r.reaped(t0.plus_secs(29)).is_some());
        assert_eq!(r.reaped(t0.plus_secs(29)), None, "{s}: a live lease is left alone");
        assert_eq!(r.reaped(t0.plus_secs(30)), Some(RunStatus::PartialFailure), "{s}");
    }
    for s in [RunStatus::Success, RunStatus::Failed, RunStatus::Canceled, RunStatus::PartialFailure] {
        r.status = s;
        rows += 1;
        live_reaped += u64::from(r.reaped(t0.plus_secs(300)).is_some());
        assert_eq!(r.reaped(t0.plus_secs(300)), None, "{s} is terminal");
    }
    crate::emit("orphan-reap-spares-live", live_reaped as f64, rows, 0);
}

/// A site id matches letters, digits, dot, underscore and hyphen, from 1 chars to 64 chars.
// spec: run.record.site-id-length@e4aed2ca
#[test]
fn a_site_id_is_1_to_64_path_safe_chars() {
    assert_eq!((SITE_ID_MIN_LEN, SITE_ID_MAX_LEN), (1, 64));
    for ok in ["a", "site-a", "eu_west.2", &"x".repeat(64)] {
        assert!(check_site_id(ok).is_ok(), "{ok}");
    }
    for bad in ["", &"x".repeat(65), "site/a", "site a", "sité", "a:b"] {
        assert!(matches!(check_site_id(bad), Err(RunError::Invalid(_))), "{bad:?}");
    }
}

/// A site id comes from the manifest's `site_id`, or the variable its `site_id_env` names, and a run's
/// `--site-id` or `--site-id-env` replaces that declaration; no declaration, both keys in one place, or an unset
/// variable raises `SiteIdUnresolved` at startup.
// spec: run.record.site-id-unresolved@4e549cda
#[test]
fn a_site_id_resolves_from_exactly_one_bound_source() {
    let manifest = |id: &str| SiteIdSources { manifest: Some(id.into()), env: None };
    let env = |value: Option<&str>| SiteIdSources { manifest: None, env: Some(("CONTEXTFUL_SITE".into(), value.map(str::to_string))) };
    assert_eq!(resolve_site_id(&manifest("site-a")).unwrap(), "site-a");
    assert_eq!(resolve_site_id(&env(Some("site-b"))).unwrap(), "site-b");
    for (sources, needle) in [
        (env(None), "CONTEXTFUL_SITE"),
        (SiteIdSources { manifest: Some("site-a".into()), env: Some(("CONTEXTFUL_SITE".into(), Some("site-b".into()))) }, "declare one"),
        (SiteIdSources::default(), "no site id"),
    ] {
        match resolve_site_id(&sources) {
            Err(RunError::SiteIdUnresolved(m)) => assert!(m.contains(needle), "{m}"),
            other => panic!("{sources:?}: {other:?}"),
        }
    }
}

/// The manifest declares a site id through its top-level `site_id` or `site_id_env` key, and a command-line
/// declaration replaces the manifest's whole.
#[test]
fn the_manifest_declares_a_site_id_and_the_command_line_replaces_it() {
    let var = |name: &str| (name == "CONTEXTFUL_SITE").then(|| "site-env".to_string());
    let read = |text: &str| SiteIdSources::from_manifest(text, var);
    let tables = "[[pipeline.tables]]\nname = \"filings\"\n";
    assert_eq!(read(&format!("site_id = \"site-m\"\n{tables}")).unwrap(), SiteIdSources { manifest: Some("site-m".into()), env: None });
    assert_eq!(
        read("site_id_env = \"CONTEXTFUL_SITE\"\n").unwrap(),
        SiteIdSources { manifest: None, env: Some(("CONTEXTFUL_SITE".into(), Some("site-env".into()))) }
    );
    assert_eq!(read("site_id_env = \"UNSET_SITE\"\n").unwrap().env, Some(("UNSET_SITE".into(), None)));
    assert_eq!(read(tables).unwrap(), SiteIdSources::default());
    match read("site_id = 7\n") {
        Err(RunError::SiteIdUnresolved(m)) => assert!(m.contains("`site_id`"), "{m}"),
        other => panic!("{other:?}"),
    }

    let manifest = read("site_id = \"site-m\"\n").unwrap();
    let flag = SiteIdSources::declared(Some("site-f".into()), None, var);
    assert_eq!(resolve_site_id(&flag.over(manifest.clone())).unwrap(), "site-f");
    let flag_env = SiteIdSources::declared(None, Some("CONTEXTFUL_SITE".into()), var);
    assert_eq!(resolve_site_id(&flag_env.over(manifest.clone())).unwrap(), "site-env");
    assert_eq!(resolve_site_id(&SiteIdSources::declared(None, None, var).over(manifest)).unwrap(), "site-m");
    // Both keys in one place stay a refusal after the merge.
    let both = read("site_id = \"site-m\"\nsite_id_env = \"CONTEXTFUL_SITE\"\n").unwrap();
    assert!(matches!(resolve_site_id(&SiteIdSources::default().over(both)), Err(RunError::SiteIdUnresolved(_))));
}

/// The describe surface answers with 5 rows by default and clamps a caller's ceiling at 500 rows.
// spec: run.record.describe-window@e4c47c33
#[test]
fn describe_answers_5_rows_by_default_and_at_most_500() {
    assert_eq!((DESCRIBE_DEFAULT_ROWS, DESCRIBE_CEILING_ROWS), (5, 500));
    assert_eq!(describe_ceiling(None), 5);
    assert_eq!(describe_ceiling(Some(20)), 20);
    assert_eq!(describe_ceiling(Some(501)), 500);
    let rows: Vec<_> = (0..9).map(|i| row(&format!("run-{i}"), &format!("2030-01-01T00:00:0{i}Z"))).collect();
    let page = select_history(rows, &Window { since: None, ceiling: describe_ceiling(None) });
    assert_eq!(page.runs.len(), 5);
}

/// The export's ceiling: 500 rows unasked, 5000 at most.
#[test]
fn export_answers_500_rows_by_default_and_at_most_5000() {
    assert_eq!((EXPORT_DEFAULT_ROWS, EXPORT_CEILING_ROWS), (500, 5000));
    assert_eq!(export_ceiling(None), 500);
    assert_eq!(export_ceiling(Some(9999)), 5000);
}

/// A window lower bound is `YYYY-MM-DD` or a UTC RFC3339 instant ending `Z`; any other spelling raises
/// `HistoryBoundSpelling` at every caller-facing surface.
// spec: run.record.bound-spelling@2ee4a78a
#[test]
fn a_bound_is_a_date_or_a_zulu_instant() {
    assert_eq!(parse_bound("2030-01-02").unwrap(), at("2030-01-02T00:00:00Z"));
    assert_eq!(parse_bound("2030-01-02T03:04:05Z").unwrap(), at("2030-01-02T03:04:05Z"));
    for bad in ["2030-01-02T03:04:05+08:00", "2030-01-02T03:04:05", "01/02/2030", "yesterday", "2030-1-2", ""] {
        assert!(matches!(parse_bound(bad), Err(RunError::HistoryBoundSpelling(_))), "{bad:?}");
    }
}

/// A history response echoes its window and flags truncation when the pipeline recorded more runs inside it than
/// the ceiling returned.
// spec: run.record.truncation-flag@7daa39b3
#[test]
fn history_echoes_its_window_and_flags_truncation() {
    let rows: Vec<_> = (0..4).map(|i| row(&format!("run-{i}"), &format!("2030-01-01T00:00:0{i}Z"))).collect();
    let window = Window { since: Some(at("2030-01-01T00:00:01Z")), ceiling: 2 };
    let page = select_history(rows.clone(), &window);
    assert_eq!(page.window, window);
    assert!(page.truncated, "three runs fall inside and two return");
    assert_eq!(page.runs.iter().map(|r| r.run_id.as_str()).collect::<Vec<_>>(), ["run-3", "run-2"], "newest first");
    let page = select_history(rows, &Window { ceiling: 3, ..window });
    assert!(!page.truncated);
    assert_eq!(page.runs.len(), 3, "the lower bound is inclusive");
}

/// An empty window answers `[]` and raises nothing.
// spec: run.record.empty-window@6549694a
#[test]
fn an_empty_window_answers_an_empty_list() {
    let rows = vec![row("run-0", "2030-01-01T00:00:00Z")];
    let page = select_history(rows, &Window { since: Some(at("2031-01-01T00:00:00Z")), ceiling: 5 });
    assert!(page.runs.is_empty());
    assert!(!page.truncated);
    assert_eq!(serde_json::to_value(&page.runs).unwrap(), serde_json::json!([]));
}

/// A recorded error is masked over credential-shaped spans and capped at 2 KiB with an ellipsis marker, on the
/// one projection every surface serves.
// spec: run.record.error-cap@11aa1e6f
#[test]
fn a_recorded_error_is_masked_and_capped_at_2_kib() {
    assert_eq!(ERROR_CAP_BYTES, 2048);
    let leaked = "GET https://svc:hunter2@api.example.com/v1 failed: Authorization: Bearer abc.def.ghi token=s3cr3t-value key sk-live-0123456789abcdef";
    let masked = cap_error(leaked);
    for secret in ["hunter2", "abc.def.ghi", "s3cr3t-value", "sk-live-0123456789abcdef"] {
        assert!(!masked.contains(secret), "{secret} survives: {masked}");
    }
    assert!(masked.contains("api.example.com/v1 failed"), "{masked}");
    // A long message is clipped on a char boundary and marked.
    let long = format!("{}{}", "é".repeat(1500), "tail");
    let capped = cap_error(&long);
    assert!(capped.len() <= ERROR_CAP_BYTES, "{}", capped.len());
    assert!(capped.ends_with(ERROR_ELLIPSIS));
    assert!(!capped.contains("tail"));
    assert_eq!(cap_error("short"), "short");
}

/// A run row carries run id, pipeline id, site id, status, owner, start and end instants, row and byte counts,
/// error kind and message, connector id, version and hash, trace id and phase.
// spec: run.record.columns@facc3543
#[test]
fn a_run_row_carries_every_column() {
    let v = serde_json::to_value(row("run-1", "2030-01-01T00:00:00Z")).unwrap();
    for column in [
        "run_id", "pipeline_id", "site_id", "status", "owner", "started_at", "ended_at", "rows", "bytes", "error_kind", "error_message",
        "connector_id", "connector_version", "connector_hash", "trace_id", "phase",
    ] {
        assert!(v.get(column).is_some(), "no `{column}` in {v}");
    }
}
