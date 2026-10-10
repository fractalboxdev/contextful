//! The local disclosure diagnostic through the built command.
#![cfg(feature = "data-plane")]

use std::process::Command;
use contextful_acceptance::http::{Response, Server};

fn check(manifest: &str) -> std::process::Output {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("contextful.toml"), manifest).unwrap();
    Command::new(env!("CARGO_BIN_EXE_contextful"))
        .current_dir(dir.path())
        .args(["disclosure", "check"])
        .output()
        .unwrap()
}

#[test]
fn check_reports_empty_policy_by_model_name() {
    let out = check("[[model]]\nid = \"revenue_by_industry\"\nsql = \"SELECT industry, count(*) FROM revenue GROUP BY industry\"\n[model.disclosure]\ngrouping_allowlist = [\"industry\"]\ncontributor_key = \"tenant_id\"\n");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("revenue_by_industry"), "{stderr}");
    assert!(stderr.contains("DisclosurePolicySuppressesNothing"), "{stderr}");
}

// spec: disclosure.set-mode.check-verb@7e5f7922
#[test]
fn check_reports_each_published_model_refusal_by_name() {
    let out = check("[[model]]\nid = \"first\"\nsql = \"SELECT count(*) FROM revenue\"\n[[model]]\nid = \"second\"\nsql = \"SELECT industry, count(*) FROM revenue GROUP BY industry\"\n[model.disclosure]\ngrouping_allowlist = [\"industry\"]\ncontributor_key = \"tenant_id\"\n");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("first: DisclosurePolicyAbsent"), "{stderr}");
    assert!(stderr.contains("second: DisclosurePolicySuppressesNothing"), "{stderr}");
}

// spec: disclosure.suppress.grouping-allowlist@d24f4655
#[test]
fn check_rejects_empty_grouping_allowlist() {
    let out = check("[[model]]\nid = \"revenue_by_industry\"\nsql = \"SELECT industry, count(*) FROM revenue GROUP BY industry\"\n[model.disclosure]\ngrouping_allowlist = []\ncontributor_key = \"tenant_id\"\nmin_group_size = 3\n");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("DisclosureGroupingAllowlistEmpty"));
}

#[test]
fn check_rejects_non_column_grouping_name() {
    let out = check("[[model]]\nid = \"revenue_by_industry\"\nsql = \"SELECT industry, count(*) FROM revenue GROUP BY industry\"\n[model.disclosure]\ngrouping_allowlist = [\"industry; DROP TABLE revenue\"]\ncontributor_key = \"tenant_id\"\nmin_group_size = 3\n");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("DisclosureGroupingAllowlistEmpty"));
}

#[test]
fn check_accepts_valid_local_policy() {
    let out = check("[[model]]\nid = \"revenue_by_industry\"\nsql = \"SELECT industry, count(*) FROM revenue GROUP BY industry\"\n[model.disclosure]\ngrouping_allowlist = [\"industry\"]\ncontributor_key = \"tenant_id\"\nmin_group_size = 3\n");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn check_rejects_a_non_select_model_statement() {
    let out = check("[[model]]\nid = \"revenue_by_industry\"\nsql = \"DELETE FROM revenue\"\n[model.disclosure]\ngrouping_allowlist = [\"industry\"]\ncontributor_key = \"tenant_id\"\nmin_group_size = 3\n");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("StatementNotReadOnly"), "{stderr}");
}

// spec: disclosure.set-mode.policy-absent@39f72fd3
#[test]
fn check_requires_policy_or_recorded_opt_out_for_aggregate_sql() {
    let aggregate = check("[[model]]\nid = \"revenue_by_industry\"\nsql = \"SELECT industry, count(*) FROM revenue GROUP BY industry\"\n");
    let stderr = String::from_utf8_lossy(&aggregate.stderr);
    assert!(!aggregate.status.success());
    assert!(stderr.contains("revenue_by_industry: DisclosurePolicyAbsent"), "{stderr}");

    let scalar = check("[[model]]\nid = \"revenue_rows\"\nsql = \"SELECT industry FROM revenue\"\n");
    assert!(scalar.status.success(), "{}", String::from_utf8_lossy(&scalar.stderr));

    let opted_out = check("[[model]]\nid = \"revenue_by_industry\"\nsql = \"SELECT industry, count(*) FROM revenue GROUP BY industry\"\ndisclosure_opt_out = \"internal aggregate\"\n");
    assert!(opted_out.status.success(), "{}", String::from_utf8_lossy(&opted_out.stderr));
}

// spec: disclosure.set-mode.model-unreadable@27a309cf
#[test]
fn check_reads_sql_file_relative_to_its_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path().join("configs");
    std::fs::create_dir(&config_dir).unwrap();
    std::fs::write(config_dir.join("contextful.toml"), "[[model]]\nid = \"revenue_by_industry\"\nsql_file = \"revenue.sql\"\n[model.disclosure]\ngrouping_allowlist = [\"industry\"]\ncontributor_key = \"tenant_id\"\nmin_group_size = 3\n").unwrap();
    let run = || Command::new(env!("CARGO_BIN_EXE_contextful")).current_dir(dir.path()).args(["disclosure", "check", "--config", "configs/contextful.toml"]).output().unwrap();
    let missing = run();
    assert!(!missing.status.success());
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(stderr.contains("revenue_by_industry: DisclosureModelUnreadable"), "{stderr}");
    std::fs::write(config_dir.join("revenue.sql"), "SELECT industry, count(*) FROM revenue GROUP BY industry").unwrap();
    let present = run();
    assert!(present.status.success(), "{}", String::from_utf8_lossy(&present.stderr));
}

// spec: disclosure.set-mode.offline-diagnostic@6b621ee2
#[test]
fn check_issues_no_object_store_request() {
    let bucket = Server::start(|_| Response::json(200, "{}"));
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(".contextful/context/research");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), format!("[sync]\nendpoint = \"{}\"\nbucket = \"records\"\nprefix = \"team\"\naccess_key_id = \"env://CONTEXTFUL_TEST_SYNC_KEY\"\nsecret_access_key = \"env://CONTEXTFUL_TEST_SYNC_SECRET\"\n", bucket.url(""))).unwrap();
    std::fs::write(dir.path().join("contextful.toml"), "[[model]]\nid = \"revenue_by_industry\"\nsql_file = \"revenue.sql\"\n[model.disclosure]\ngrouping_allowlist = [\"industry\"]\ncontributor_key = \"tenant_id\"\nmin_group_size = 3\n").unwrap();
    std::fs::write(dir.path().join("revenue.sql"), "SELECT industry, count(*) FROM revenue GROUP BY industry").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .current_dir(dir.path())
        .env("CONTEXTFUL_TEST_SYNC_KEY", "key")
        .env("CONTEXTFUL_TEST_SYNC_SECRET", "secret")
        .args(["disclosure", "check"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(bucket.requests.lock().unwrap().is_empty());
}
