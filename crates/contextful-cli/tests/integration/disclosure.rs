//! The local disclosure diagnostic through the built command.

use std::process::Command;

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
