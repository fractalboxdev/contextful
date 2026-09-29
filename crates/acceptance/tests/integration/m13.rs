//! Milestone 13 — disclosure.
//!
//! Reach: two parties compare against a benchmark neither can invert.

use contextful_acceptance::{bin, GitRepo};
use serde_json::json;
use std::process::Output;

const POLICY: &str = "authoring_posture = \"per_request\"\n[pipeline.models.revenue_by_industry]\n\
statement = \"SELECT industry, tenant_id, SUM(revenue) AS revenue FROM revenue GROUP BY industry, tenant_id\"\n\
\n\
[pipeline.models.revenue_by_industry.disclosure]\n\
grouping_allowlist    = [\"industry\", \"region\", \"quarter\"]\n\
contributor_key       = \"tenant_id\"\n\
min_group_size        = 3\n\
max_contributor_share = 0.4\n\
emit_sentinel         = true\n\
forbidden_columns     = [\"tenant_id\"]\n";

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
#[ignore = "milestone 13 is open: no surface releases a derived table"]
fn m13_disclosure() {
    let cf = bin("contextful");
    let r = GitRepo::init();
    r.write("contextful.toml", POLICY);

    // A policy setting neither threshold refuses before any release; its allowlist is
    // valid, so the empty-policy refusal is the only one that applies.
    r.write(
        "empty.toml",
        "[pipeline.models.revenue_by_industry.disclosure]\ngrouping_allowlist = [\"industry\"]\ncontributor_key = \"tenant_id\"\n",
    );
    let empty = r.run(&cf, &["disclosure", "check", "--config", "empty.toml"]);
    assert!(!empty.status.success());
    assert!(String::from_utf8_lossy(&empty.stderr).contains("DisclosurePolicySuppressesNothing"));

    // Two parties land their figures into the shared benchmark. In `energy` three tenants
    // hold balanced shares; in `retail` one tenant holds most of the mass.
    let party = |site: &str, rows: &[(&str, &str, f64)]| {
        let file = format!("{site}.jsonl");
        let text: Vec<String> = rows.iter().map(|(t, i, v)| json!({"tenant_id": t, "industry": i, "revenue": v}).to_string()).collect();
        r.write(&file, &text.join("\n"));
        ok(&r.run(&cf, &["context", "land", "revenue", "--project", "benchmark", "--rows", &file, "--run-id", &format!("run-{site}"), "--site-id", site]));
    };
    party("party-a", &[("t1", "energy", 100.0), ("t2", "energy", 110.0), ("t4", "retail", 9_000.0)]);
    party("party-b", &[("t3", "energy", 95.0), ("t5", "retail", 40.0), ("t6", "retail", 35.0)]);
    r.commit("the benchmark model and both parties' figures");

    // The balanced group publishes, the dominated one collapses into one sentinel, and no
    // contributor key leaves.
    let published = ok(&r.run(&cf, &["disclosure", "release", "revenue_by_industry", "--project", "benchmark"]));
    assert!(published.contains("energy"), "{published}");
    assert!(!published.contains("retail"), "{published}");
    assert_eq!(published.matches("__suppressed__").count(), 1, "{published}");
    assert!(!published.contains("tenant_id") && !published.contains("t4"), "{published}");
}
