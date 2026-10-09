use contextful_eval::case::load;
use contextful_eval::trace::{admit_export, hosted, staged, RunRecord, TraceStore, Verdict};
use contextful_eval::EvalError;
use serde_json::json;

#[test]
fn an_endpoint_is_hosted_unless_its_host_sits_inside_the_perimeter() {
    for inside in [
        "http://127.0.0.1:4318/v1/traces",
        "http://localhost:4318",
        "http://[::1]:4318",
        "http://10.2.0.7:4318",
        "https://192.168.1.20",
        "http://collector:4318",
        "https://otel.corp.internal/v1/traces",
        "http://user@collector.lan:4318",
    ] {
        assert_eq!(hosted(inside), Ok(false), "{inside}");
    }
    for outside in ["https://api.tracing.example.com/v1/traces", "http://8.8.8.8:4318", "https://[2001:db8::1]/"] {
        assert_eq!(hosted(outside), Ok(true), "{outside}");
    }
    assert!(hosted("udp://collector:4318").is_err());
    assert!(hosted("http:///v1").is_err());
}

#[test]
fn a_hosted_endpoint_admits_fixtures_alone() {
    let deployed = vec!["/srv/ops/.contextful/context/ops".to_string()];
    assert!(admit_export(None, &deployed).is_ok(), "no endpoint exports nothing");
    assert!(admit_export(Some("http://127.0.0.1:4318"), &deployed).is_ok(), "a self-hosted collector serves any run");
    assert!(admit_export(Some("https://api.tracing.example.com"), &[]).is_ok(), "a hosted collector serves fixtures");
    for endpoint in ["https://api.tracing.example.com", "not a url"] {
        match admit_export(Some(endpoint), &deployed) {
            Err(e @ EvalError::TraceExportOutOfPerimeter { .. }) => {
                assert_eq!(e.code(), "TraceExportOutOfPerimeter");
                assert!(e.to_string().contains("/srv/ops/.contextful/context/ops"), "{e}");
            }
            other => panic!("{endpoint}: {other:?}"),
        }
    }
}

#[test]
fn a_store_appends_run_history_and_staged_cases() {
    let lines = [
        json!({"id": "hit", "corpus": "c", "question": "q", "expected": {"artifacts": ["t#a"]}}),
        json!({"id": "miss", "corpus": "c", "question": "q", "expected": {"artifacts": ["t#b"]}}),
        json!({"id": "absent", "corpus": "c", "question": "q", "expected": {"must_abstain": true}}),
    ];
    let cases = load(&lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")).unwrap();
    let report = json!({
        "run": { "k": 5, "tier": "deterministic", "model": null, "samples": 1 },
        "seed": 7,
        "n_cases": 3,
        "retrieval": { "hybrid": { "recall_at_k": { "n": 2, "mean": 0.5, "min": 0.0, "max": 1.0 } } },
        "cases": [
            { "id": "hit", "legs": { "hybrid": ["t#a"] } },
            { "id": "miss", "legs": { "hybrid": ["t#c"] } },
            { "id": "absent", "legs": { "hybrid": ["t#d"] } },
        ],
    });
    let staged = staged(&cases, &report);
    assert_eq!(staged.iter().map(|s| s.case.as_str()).collect::<Vec<_>>(), ["miss", "absent"]);
    assert_eq!(staged[0].returned, ["t#c"]);
    assert!(staged[1].reason.contains("must-abstain"));

    let dir = tempfile::tempdir().unwrap();
    let store = TraceStore::open(&dir.path().join("traces")).unwrap();
    let record = RunRecord::of(&report, Verdict::Held, None).unwrap();
    assert_eq!(record.figures["retrieval.hybrid.recall_at_k"], 0.5);
    store.record(&record, &staged).unwrap();
    store.record(&RunRecord { floors: Verdict::Red, ..record.clone() }, &[]).unwrap();
    let history = store.history().unwrap();
    assert_eq!(history.iter().map(|r| r.floors).collect::<Vec<_>>(), [Verdict::Held, Verdict::Red]);
    assert_eq!(store.staging().unwrap(), staged);
}
