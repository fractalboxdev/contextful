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

/// The pepper every released project resolves; the ledger keys units under it.
const PEPPER: &str = "release-test-pepper";

fn cli(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_contextful")).current_dir(dir).env_remove("CONTEXTFUL_NODE_ID").env("CONTEXTFUL_PEPPER", PEPPER).args(args).output().unwrap()
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

/// A project's budget ledger is `.contextful/disclosure/<project>/budget.sqlite`, outside the synced store root, and records each contributing unit under the keyed digest of its key under the project's pepper.
// spec: disclosure.release.budget-ledger@425f6b34
#[test]
fn release_keeps_the_ledger_outside_the_store_root_under_unit_digests() {
    let dir = benchmark(&format!("{RELEASED}{CAP}"));
    assert!(release(dir.path(), &[]).status.success());
    let path = dir.path().join(".contextful/disclosure/benchmark/budget.sqlite");
    assert!(path.is_file(), "the ledger is beside the store root");
    let ledger = contextful_sqlite::SqliteBudgetLedger::open(&path).unwrap();
    for tenant in ["t1", "t2", "t3", "t4", "t5", "t6"] {
        assert!(ledger.budget(tenant).unwrap().is_none(), "{tenant} is recorded in plaintext");
        assert!(ledger.budget(&sha2_hex(tenant)).unwrap().is_none(), "{tenant} is recorded under an unkeyed digest");
        let digest = contextful_policy::enforce::mask::Pepper::resolve(|_| Some(PEPPER.to_string())).digest(tenant);
        assert_eq!(ledger.budget(&digest).unwrap().map(|b| b.spent), Some(1_000_000_000), "{tenant}");
    }
    assert!(!dir.path().join(".contextful/context/benchmark/budget.sqlite").exists());
}

fn sha2_hex(text: &str) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(text.as_bytes()))
}

/// Two governed models over the same tenants, released at one instant, each reserve from
/// every unit: the second release under a cap covering one raises `DisclosureUnitBudgetExhausted`.
#[test]
fn releases_of_two_models_at_one_instant_each_charge_the_shared_units() {
    let second = RELEASED.split("[[model]]").nth(1).unwrap().replace("id = \"revenue_by_industry\"", "id = \"revenue_by_industry_b\"");
    let dir = benchmark(&format!("{RELEASED}\n[[model]]{second}{CAP}"));
    let now = ["--now", "2030-01-01T00:00:00Z"];
    let out = release(dir.path(), &now);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = cli(dir.path(), &["disclosure", "release", "revenue_by_industry_b", "--project", "benchmark", "--now", "2030-01-01T00:00:00Z"]);
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("DisclosureUnitBudgetExhausted:"), "{}", String::from_utf8_lossy(&out.stderr));
}

/// A governed model named as a publication group's job records the group's completion on release.
#[test]
fn a_release_of_a_publication_job_records_the_group_completion() {
    let group = "\n[[export]]\nname='industry-copy'\ntable='revenue_by_industry'\nendpoint='http://127.0.0.1:1/events'\nformat='changes-v1'\nkey=['industry']\nschedule='every 30s'\n[[publication]]\nname='industry-projection'\njob='revenue_by_industry'\nmembers=['industry-copy']\n";
    let dir = benchmark(&format!("{RELEASED}{CAP}{group}"));
    let out = release(dir.path(), &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let ledger = contextful_sqlite::PublicationLedger::open(&dir.path().join(".contextful/context/benchmark/machine.sqlite")).unwrap();
    let source = ledger.source("industry-projection").unwrap().expect("the release records the group's completion");
    assert_eq!(source.frontiers.len(), 1);
}

/// A governed model's build outside `disclosure release` refuses with `error`, names the
/// release verb, and leaves the model's table unpublished.
fn assert_refused_unpublished(dir: &std::path::Path, out: &std::process::Output, error: &str) {
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.starts_with(&format!("{error}:")), "{stderr}");
    assert!(stderr.contains("contextful disclosure release revenue_by_industry --project benchmark"), "{stderr}");
    assert!(!stderr.contains("t4"), "{stderr}");
    assert!(published(dir).is_none(), "nothing was published");
}

/// A build of a model declaring `[model.disclosure]` outside the release verb raises `DisclosureReleaseRequired` and publishes nothing.
// spec: disclosure.release.governed-build@93613c4a
#[test]
fn a_plain_build_of_a_governed_model_raises_release_required() {
    let dir = benchmark(&format!("{RELEASED}{CAP}"));
    let out = cli(dir.path(), &["build", "revenue_by_industry", "--project", "benchmark"]);
    assert_refused_unpublished(dir.path(), &out, "DisclosureReleaseRequired");
    let out = release(dir.path(), &[]);
    assert!(out.status.success(), "the release verb still publishes: {}", String::from_utf8_lossy(&out.stderr));
    assert!(published(dir.path()).is_some());
}

/// A build job naming a governed model refuses as the plain build does.
#[test]
fn a_build_job_naming_a_governed_model_raises_release_required() {
    let job = "\n[[job]]\nname = \"industry-build\"\nkind = \"build\"\ntarget = \"revenue_by_industry\"\nschedule = \"every 1h\"\n";
    let dir = benchmark(&format!("{RELEASED}{CAP}{job}"));
    let out = cli(dir.path(), &["job", "fire", "industry-build", "--project", "benchmark"]);
    assert_refused_unpublished(dir.path(), &out, "DisclosureReleaseRequired");
    // The refusal is deterministic, so the job's execution records it as permanent.
    let history = cli(dir.path(), &["run", "history", "--project", "benchmark", "--export"]);
    let rows: Vec<serde_json::Value> = String::from_utf8_lossy(&history.stdout).lines().skip(1).map(|l| serde_json::from_str(l).unwrap()).collect();
    let fired: Vec<_> = rows.iter().filter(|r| r["host_scope"] == "job:industry-build").collect();
    assert_eq!(fired.len(), 1, "{rows:?}");
    assert_eq!((&fired[0]["status"], &fired[0]["error_kind"]), (&serde_json::json!("failed"), &serde_json::json!("Permanent")), "{rows:?}");
}

/// `RELEASED` with `keys` added to its `[model.disclosure]` block.
fn governed(keys: &str) -> String {
    RELEASED.replace("per_run_spend = 1000\n", &format!("per_run_spend = 1000\n{keys}\n"))
}

/// The diagnostic reports a singleton cohort and exact per-person figures by model name,
/// and admits exact figures under cohort governance.
// spec: disclosure.bound-cohort.singleton-cohort@4a844bb2
// spec: disclosure.bound-cohort.exact-figures@0c5b5fc3
#[test]
fn check_reports_singleton_cohorts_and_exact_per_person_figures_by_model() {
    let model = |id: &str, keys: &str| {
        format!("[[model]]\nid = \"{id}\"\nsql = \"SELECT industry, tenant_id, SUM(revenue) AS revenue FROM revenue GROUP BY industry, tenant_id\"\nunique_key = [\"industry\", \"tenant_id\"]\n[model.disclosure]\ncontributor_key = \"tenant_id\"\nmin_group_size = 3\n{keys}\n")
    };
    let out = check(&format!(
        "{}{}{}",
        model("singleton", "grouping_allowlist = [\"industry\", \"tenant_id\"]\ngovernance = \"cohort\""),
        model("exact", "grouping_allowlist = [\"industry\"]\nfigures = \"exact\""),
        model("cohort_exact", "grouping_allowlist = [\"industry\"]\nfigures = \"exact\"\ngovernance = \"cohort\""),
    ));
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("singleton: DisclosureSingletonCohort"), "{stderr}");
    assert!(stderr.contains("exact: DisclosureExactPerPerson"), "{stderr}");
    assert!(!stderr.contains("cohort_exact"), "{stderr}");
}

/// A cohort release whose key names the contributor key refuses before any unit is charged
/// or any row is published.
#[test]
fn release_refuses_a_singleton_cohort_before_charging_a_unit() {
    let manifest = governed("governance = \"cohort\"").replace("grouping_allowlist = [\"industry\"]", "grouping_allowlist = [\"industry\", \"tenant_id\"]");
    let dir = benchmark(&format!("{manifest}{CAP}"));
    let out = release(dir.path(), &[]);
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("DisclosureSingletonCohort:"), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(published(dir.path()).is_none(), "nothing was published");
    assert!(!dir.path().join(".contextful/disclosure/benchmark/budget.sqlite").exists(), "no unit was charged");
}

/// One policy declaring exact figures refuses under the default `per-person` governance and
/// publishes under `cohort`.
#[test]
fn a_per_person_and_a_cohort_release_of_exact_figures_differ() {
    let dir = benchmark(&format!("{}{CAP}", governed("figures = \"exact\"")));
    let out = release(dir.path(), &[]);
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("DisclosureExactPerPerson:"), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(published(dir.path()).is_none(), "nothing was published");

    let dir = benchmark(&format!("{}{CAP}", governed("figures = \"exact\"\ngovernance = \"cohort\"")));
    let out = release(dir.path(), &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = published(dir.path()).expect("the cohort release publishes").to_string();
    assert!(text.contains("energy") && !text.contains("t4"), "{text}");
}

const AUDIENCE: &str = "contextful://benchmark";

/// The answers `contextful mcp` gives under `token` to one `context.query` per statement.
fn credentialed_queries(dir: &std::path::Path, public: &str, token: &str, statements: &[&str]) -> Vec<serde_json::Value> {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .current_dir(dir)
        .env_remove("CONTEXTFUL_NODE_ID")
        .env("CONTEXTFUL_PEPPER", PEPPER)
        .env("CONTEXTFUL_TOKEN", token)
        .args(["mcp", "--project", "benchmark", "--public-key", public, "--audience", AUDIENCE])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        for (id, sql) in statements.iter().enumerate() {
            let call = serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": "context.query", "arguments": { "sql": sql } } });
            writeln!(stdin, "{call}").unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

fn refusal_text(answer: &serde_json::Value) -> String {
    assert_eq!(answer["result"]["isError"], serde_json::json!(true), "{answer}");
    answer["result"]["content"][0]["text"].as_str().unwrap().to_string()
}

/// A served face over a released `cohort` model: its source table answers only a credential
/// holding `read-individual` over it, and its published table refuses a grouping coarser
/// than its released key.
// spec: disclosure.bound-cohort.per-individual-row@b3d08f04
// spec: disclosure.bound-cohort.cohort-widening@5979fff9
#[test]
fn a_served_cohort_model_guards_its_source_rows_and_its_released_key() {
    let dir = benchmark(&format!("{}{CAP}", governed("governance = \"cohort\"")));
    let p = dir.path();
    let out = release(p, &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    std::fs::write(p.join(".contextful/issuance.toml"), format!("default_audience = \"{AUDIENCE}\"\nmax_lifetime_secs = 3600\n")).unwrap();
    let text = |out: std::process::Output| {
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let public = text(cli(p, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let mint = |extra: &[&str]| {
        let mut args = vec!["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--ttl", "600"];
        args.extend_from_slice(extra);
        text(cli(p, &args))
    };
    let statements = ["SELECT tenant_id FROM revenue ORDER BY tenant_id", "SELECT industry FROM revenue_by_industry ORDER BY industry", "SELECT sum(revenue) FROM revenue_by_industry"];

    let reader = mint(&["--table", "*"]);
    let answers = credentialed_queries(p, &public, &reader, &statements);
    assert_eq!(answers.len(), 3, "{answers:?}");
    assert!(refusal_text(&answers[0]).contains("DisclosurePerIndividualUngranted"), "{answers:?}");
    assert_eq!(answers[1]["result"]["structuredContent"]["rows"], serde_json::json!([["__suppressed__"], ["energy"]]), "{answers:?}");
    assert!(refusal_text(&answers[2]).contains("DisclosureCohortWidening"), "{answers:?}");

    let individual = mint(&["--action", "read", "--action", "read-individual", "--table", "revenue"]);
    let answers = credentialed_queries(p, &public, &individual, &statements[..1]);
    let rows = &answers[0]["result"]["structuredContent"]["rows"];
    assert_eq!(rows.as_array().map(Vec::len), Some(6), "{answers:?}");
}

/// A derive pipeline over the cohort model's source table `revenue`, reading each row's
/// `industry` as a media file under `media/`.
const DERIVE: &str = r#"
[[pipeline]]
id = "revenue-text"
tables = [{ name = "passages", primary_key = ["unit_ref", "derivation_key", "cue_seq"] }]
[pipeline.source]
name = "derive"
config = { engine = "reader", source_table = "revenue", media_column = "industry", parent_id_column = "tenant_id" }

[derive.reader]
driver = "exec"
media_root = "media"
[derive.reader.engine]
command = ["cat", "{input}"]
output_format = "srt"
"#;

/// A derive run reading a cohort model's source table refuses at run start, landing nothing,
/// unless its credential holds `read-individual` over that table; the output it lands then
/// answers only a reader holding `read-individual` over the output.
// spec: disclosure.bound-cohort.derive-individual@a3d9fb27
// spec: disclosure.bound-cohort.derived-individual@6c87283b
#[test]
fn a_derive_over_a_cohort_source_runs_only_under_read_individual() {
    let dir = benchmark(&format!("{}{CAP}{DERIVE}", governed("governance = \"cohort\"")));
    let p = dir.path();
    std::fs::create_dir_all(p.join("media")).unwrap();
    for industry in ["energy", "retail"] {
        std::fs::write(p.join("media").join(industry), format!("1\n00:00:00,000 --> 00:00:01,000\n{industry} memo.\n\n")).unwrap();
    }
    std::fs::write(p.join(".contextful/issuance.toml"), format!("default_audience = \"{AUDIENCE}\"\nmax_lifetime_secs = 3600\n")).unwrap();
    let text = |out: std::process::Output| {
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let public = text(cli(p, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let mint = |extra: &[&str]| {
        let mut args = vec!["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--ttl", "600"];
        args.extend_from_slice(extra);
        text(cli(p, &args))
    };
    let run = |token: Option<&str>, run_id: &str| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_contextful"));
        c.current_dir(p).env_remove("CONTEXTFUL_NODE_ID").env_remove("CONTEXTFUL_TOKEN").env("CONTEXTFUL_PEPPER", PEPPER);
        c.args(["pipeline", "run", "revenue-text", "--project", "benchmark", "--run-id", run_id, "--site-id", "site-a"]);
        if let Some(token) = token {
            c.env("CONTEXTFUL_TOKEN", token).args(["--public-key", public.as_str(), "--audience", AUDIENCE]);
        }
        c.output().unwrap()
    };
    let output = p.join(".contextful/context/benchmark/tables/revenue_text_passages");
    let refused = |out: std::process::Output| {
        assert!(!out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(stderr.contains("DisclosurePerIndividualUngranted") && stderr.contains("revenue"), "{stderr}");
        assert!(!output.exists(), "a refused derive landed output");
    };

    refused(run(None, "derive-unbound"));
    let reader = mint(&["--action", "read", "--action", "write", "--table", "revenue", "--table", "revenue_text_passages"]);
    refused(run(Some(&reader), "derive-read"));

    let individual = mint(&["--action", "read", "--action", "write", "--action", "read-individual", "--table", "revenue", "--table", "revenue_text_passages"]);
    let out = run(Some(&individual), "derive-individual");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(output.exists(), "the granted derive landed its output");

    let statement = ["SELECT count(*) FROM revenue_text_passages"];
    let any = mint(&["--table", "*"]);
    let answers = credentialed_queries(p, &public, &any, &statement);
    assert!(refusal_text(&answers[0]).contains("DisclosurePerIndividualUngranted"), "{answers:?}");
    let output_reader = mint(&["--action", "read", "--action", "read-individual", "--table", "revenue_text_passages"]);
    let answers = credentialed_queries(p, &public, &output_reader, &statement);
    assert_eq!(answers[0]["result"]["structuredContent"]["rows"], serde_json::json!([["6"]]), "{answers:?}");
}
