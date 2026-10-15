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

const RELEASED: &str = r#"authoring_posture = "per_request"
site_id = "site-a"
[project]
name = "benchmark"

[[model]]
id = "revenue_by_industry"
sql = "SELECT industry, tenant_id, SUM(revenue) AS revenue FROM revenue GROUP BY industry, tenant_id"
unique_key = ["industry", "tenant_id"]
[model.contract]
version = "1.0.0"
columns = [{ name = "industry", type = "utf8", nullable = false }, { name = "tenant_id", type = "utf8", nullable = false }, { name = "revenue", type = "float64", nullable = false }]
[model.disclosure]
grouping_allowlist = ["industry"]
contributor_key = "tenant_id"
min_group_size = 3
max_contributor_share = 0.4
emit_sentinel = true
forbidden_columns = ["tenant_id"]
per_run_spend = 1000
[model.disclosure.metric_bounds]
revenue = { lower = 0, upper = 10000, quantum = 1 }
"#;

const CAP: &str = "\n[disclosure]\nunit_lifetime_cap = 1500\n";

fn cli(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_contextful")).current_dir(dir).env_remove("CONTEXTFUL_NODE_ID").args(args).output().unwrap()
}

/// A project declaring `manifest`, with `energy` balanced across three tenants and `retail`
/// dominated by one.
fn benchmark(manifest: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::write(p.join("contextful.toml"), manifest).unwrap();
    let rows = [("t1", "energy", 100.0), ("t2", "energy", 110.0), ("t3", "energy", 95.0), ("t4", "retail", 9000.0), ("t5", "retail", 40.0), ("t6", "retail", 35.0)];
    let text: Vec<String> = rows.iter().map(|(t, i, v)| serde_json::json!({"tenant_id": t, "industry": i, "revenue": v}).to_string()).collect();
    std::fs::write(p.join("revenue.jsonl"), text.join("\n")).unwrap();
    let out = cli(p, &["context", "land", "revenue", "--project", "benchmark", "--rows", "revenue.jsonl", "--run-id", "r1", "--site-id", "site-a"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    dir
}

fn release(dir: &std::path::Path, extra: &[&str]) -> std::process::Output {
    let mut args = vec!["disclosure", "release", "revenue_by_industry", "--project", "benchmark"];
    args.extend_from_slice(extra);
    cli(dir, &args)
}

fn published(dir: &std::path::Path) -> Option<serde_json::Value> {
    let out = cli(dir, &["query", "--json", "--project", "benchmark", "SELECT * FROM revenue_by_industry ORDER BY industry"]);
    out.status.success().then(|| serde_json::from_slice(&out.stdout).unwrap())
}

/// `contextful disclosure release <model> --project <project>` builds the named model from its declared statement, applies its disclosure policy, and publishes one derived table.
// spec: disclosure.release.release-verb@a750bf01
// spec: disclosure.release.model-policy@cbb3ae01
#[test]
fn release_publishes_one_derived_table_under_the_model_policy() {
    let dir = benchmark(&format!("{RELEASED}{CAP}"));
    let out = release(dir.path(), &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["envelope"]["table"], "revenue_by_industry");
    assert_eq!(report["envelope"]["reserved_spend"], 1_000_000_000u64);
    assert_eq!(report["envelope"]["suppressed_present"], true);
    assert_eq!(report["envelope"]["row_count"], 2);
    assert!(!stdout.contains("tenant_id") && !stdout.contains("t4"), "{stdout}");
    let read = published(dir.path()).expect("the derived table reads");
    let text = read.to_string();
    assert!(text.contains("energy") && text.contains("__suppressed__") && !text.contains("retail"), "{text}");
    assert!(!text.contains("tenant_id") && !text.contains("t4"), "{text}");
}

/// A release requesting an activation handle or lookalike segment, as `--output activation-handle` or `--output lookalike-segment`, raises `DisclosureLookalikeUnsupported` and publishes nothing.
// spec: disclosure.release.lookalike-refusal@a6a3a6ce
#[test]
fn release_refuses_an_activation_handle_or_lookalike_segment() {
    let dir = benchmark(&format!("{RELEASED}{CAP}"));
    for output in ["activation-handle", "lookalike-segment"] {
        let out = release(dir.path(), &["--output", output]);
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).starts_with("DisclosureLookalikeUnsupported:"), "{}", String::from_utf8_lossy(&out.stderr));
    }
    assert!(published(dir.path()).is_none(), "nothing was published");
    assert!(!dir.path().join(".contextful/disclosure/benchmark/budget.sqlite").exists(), "no unit was charged");
}

/// A release whose policy declares no `per_run_spend` raises `DisclosureSpendUndeclared` and publishes nothing.
#[test]
fn release_refuses_a_policy_declaring_no_spend() {
    let dir = benchmark(&format!("{}{CAP}", RELEASED.replace("per_run_spend = 1000\n", "")));
    let out = release(dir.path(), &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("DisclosureSpendUndeclared:"), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(published(dir.path()).is_none());
}

/// A project manifest's top-level `[disclosure]` block declares `unit_lifetime_cap`, the epsilon each contributing unit spends across every release; a release grants it to each unit it reserves from, and an undeclared cap is zero.
// spec: disclosure.release.lifetime-cap@7e178cbe
#[test]
fn release_spends_against_the_declared_lifetime_cap() {
    // No cap declared: every unit holds zero, so the first release refuses.
    let uncapped = benchmark(RELEASED);
    let out = release(uncapped.path(), &[]);
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("DisclosureUnitBudgetExhausted:"), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(published(uncapped.path()).is_none());

    // A cap of 1500 covers one release of 1000 and not a second.
    let dir = benchmark(&format!("{RELEASED}{CAP}"));
    assert!(release(dir.path(), &[]).status.success());
    let first = published(dir.path()).unwrap();
    let out = release(dir.path(), &[]);
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("DisclosureUnitBudgetExhausted:"), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(published(dir.path()).unwrap(), first, "the refused release left the first one serving");

    // Raising the declared cap is the grant edit that admits the next release.
    std::fs::write(dir.path().join("contextful.toml"), format!("{RELEASED}\n[disclosure]\nunit_lifetime_cap = 2000\n")).unwrap();
    assert!(release(dir.path(), &[]).status.success(), "the raised cap covers a second release");
}

/// A project's budget ledger is `.contextful/disclosure/<project>/budget.sqlite`, outside the synced store root, and records each contributing unit under the SHA-256 digest of its key.
// spec: disclosure.release.budget-ledger@775f89b7
#[test]
fn release_keeps_the_ledger_outside_the_store_root_under_unit_digests() {
    let dir = benchmark(&format!("{RELEASED}{CAP}"));
    assert!(release(dir.path(), &[]).status.success());
    let path = dir.path().join(".contextful/disclosure/benchmark/budget.sqlite");
    assert!(path.is_file(), "the ledger is beside the store root");
    let ledger = contextful_sqlite::SqliteBudgetLedger::open(&path).unwrap();
    for tenant in ["t1", "t2", "t3", "t4", "t5", "t6"] {
        assert!(ledger.budget(tenant).unwrap().is_none(), "{tenant} is recorded in plaintext");
        let digest = sha2_hex(tenant);
        assert_eq!(ledger.budget(&digest).unwrap().map(|b| b.spent), Some(1_000_000_000), "{tenant}");
    }
    assert!(!dir.path().join(".contextful/context/benchmark/budget.sqlite").exists());
}

fn sha2_hex(text: &str) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(text.as_bytes()))
}
