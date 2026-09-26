//! Milestone 13 — disclosure.
//!
//! Reach: two parties compare against a benchmark neither can invert.

use contextful_acceptance::{bin, GitRepo};

const POLICY: &str = "[pipeline.models.revenue_by_industry.disclosure]\n\
grouping_allowlist    = [\"industry\", \"region\", \"quarter\"]\n\
contributor_key       = \"tenant_id\"\n\
min_group_size        = 3\n\
max_contributor_share = 0.4\n\
emit_sentinel         = true\n\
forbidden_columns     = [\"tenant_id\"]\n";

#[test]
#[ignore = "milestone 13 is open: no surface releases a derived table"]
fn m13_disclosure() {
    let cf = bin("contextful");
    let r = GitRepo::init();
    r.write("contextful.toml", POLICY);

    // A policy setting neither threshold refuses before any release.
    r.write("empty.toml", "[pipeline.models.revenue_by_industry.disclosure]\ncontributor_key = \"tenant_id\"\n");
    let empty = r.run(&cf, &["disclosure", "check", "--config", "empty.toml"]);
    assert!(!empty.status.success());
    assert!(String::from_utf8_lossy(&empty.stderr).contains("DisclosurePolicySuppressesNothing"));

    // A dominated group publishes as the sentinel, and no contributor key leaves.
    let out = r.run(&cf, &["disclosure", "release", "revenue_by_industry", "--project", "benchmark"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let published = String::from_utf8_lossy(&out.stdout);
    assert!(published.contains("__suppressed__"), "{published}");
    assert!(!published.contains("tenant_id"), "{published}");
}
