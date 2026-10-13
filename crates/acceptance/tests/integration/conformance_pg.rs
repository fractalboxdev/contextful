#![cfg(feature = "pg-conformance")]
//! The remote profile runs against a supplied PG-enabled release artifact and synthetic services.

use contextful_acceptance::conformance::{self, Options};
use std::path::PathBuf;

#[test]
fn the_cloudflare_profile_executes_every_required_scenario_on_remote_state() {
    let selected = PathBuf::from(
        std::env::var_os("CONTEXTFUL_CONFORMANCE_TEST_BINARY")
            .expect("pg-conformance requires a supplied pg-catalog artifact"),
    );
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("contextful");
    std::fs::copy(selected, &binary).unwrap();
    let report = conformance::run(&Options {
        binary,
        storage_profile: "cloudflare-postgres".into(),
        required: vec![],
        timeout_seconds: 60,
    })
    .unwrap();
    assert!(
        report.passed(),
        "{}",
        serde_json::to_string_pretty(&report).unwrap()
    );
    assert_eq!(report.storage_profile, "cloudflare-postgres");
    assert_eq!(report.scenarios.len(), 7);
}
