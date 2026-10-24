#![cfg(feature = "data-plane")]

use contextful_eval::entity::METRICS;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn run(dir: &Path, args: &[&str], token: Option<&str>) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_contextful"));
    c.current_dir(dir).args(args).env_remove("CONTEXTFUL_TOKEN");
    if let Some(t) = token {
        c.env("CONTEXTFUL_TOKEN", t);
    }
    c.output().unwrap()
}

fn text(out: Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[test]
fn native_entity_evidence_runs_both_live_arms_and_emits_versioned_measurements() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".contextful")).unwrap();
    std::fs::write(
        dir.path().join(".contextful/issuance.toml"),
        "default_audience = \"contextful://entity-eval\"\nmax_lifetime_secs = 3600\n",
    )
    .unwrap();
    let public = text(run(
        dir.path(),
        &["token", "keygen", "--out", ".contextful/issuer.seed"],
        None,
    ));
    let token = text(run(
        dir.path(),
        &[
            "token",
            "mint",
            "--issuer-key",
            ".contextful/issuer.seed",
            "--agent",
            "agent://entity-eval",
            "--zone",
            "on-prem:hq",
            "--ttl",
            "600",
            "--action",
            "read",
            "--table",
            "memory/*",
            "--table",
            "evidence/*",
        ],
        None,
    ));
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest = root.join("evals/entity-evidence/v1.json");
    let baseline = root.join("evals/entity-evidence/v1.baseline.json");
    let report = dir.path().join("report.json");
    let output = run(
        dir.path(),
        &[
            "eval",
            "entity-evidence",
            "--manifest",
            manifest.to_str().unwrap(),
            "--baseline",
            baseline.to_str().unwrap(),
            "--report",
            report.to_str().unwrap(),
            "--public-key",
            &public,
            "--audience",
            "contextful://entity-eval",
        ],
        Some(&token),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_str(&std::fs::read_to_string(report).unwrap()).unwrap();
    assert_eq!(report["benchmark"]["version"], "entity-evidence/v1");
    assert!(
        report["benchmark"]["fixture_sha256"]
            .as_str()
            .unwrap()
            .len()
            == 64
    );
    assert_eq!(report["n_cases"], 72);
    if let Some(path) = std::env::var_os("CONTEXTFUL_ENTITY_REPORT") {
        if let Some(parent) = Path::new(&path).parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    assert_eq!(
        report["entity_evidence"]["structured"]["path_correctness"]["mean"],
        json!(1.0)
    );
    let cases = report["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 72);
    for case in cases {
        assert_eq!(
            case["arms"]["ordinary"]["eligible_evidence"],
            case["arms"]["structured"]["eligible_evidence"]
        );
    }
    // The ordinary arm is a configured control: ranked retrieval scores the declared
    // evidence text, so an answerable case resolves its exact entity, claim and path.
    let case = |id: &str| cases.iter().find(|c| c["id"] == id).unwrap().clone();
    let control = case("amber-alias");
    let ordinary = &control["arms"]["ordinary"];
    assert_eq!(ordinary["answer"]["entity"], json!("plant-0"));
    assert_eq!(ordinary["answer"]["claims"].as_array().unwrap().len(), 1);
    assert_eq!(ordinary["answer"]["claims"][0]["id"], json!("0-live"));
    assert_eq!(ordinary["answer"]["reason"], Value::Null);
    for metric in METRICS {
        assert_eq!(ordinary["scores"][metric], json!(1.0), "{metric}");
    }
    // A claim with two citations earns credit from either arm in either citation order.
    let cited = case("harbour-canonical");
    let complete = BTreeSet::from(["evidence/notes#eval-0001:87", "evidence/notes#eval-0001:96"]);
    for arm in ["structured", "ordinary"] {
        let claim = &cited["arms"][arm]["answer"]["claims"][0];
        let citations: BTreeSet<&str> = claim["citations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap())
            .collect();
        assert_eq!(citations, complete, "{arm}");
        assert_eq!(cited["arms"][arm]["scores"]["eligible_claim_recall"], json!(1.0), "{arm}");
        assert_eq!(cited["arms"][arm]["scores"]["path_correctness"], json!(1.0), "{arm}");
    }
    // Both arms reproduce the committed envelope's means and sample counts exactly.
    let envelope: Value =
        serde_json::from_str(&std::fs::read_to_string(&baseline).unwrap()).unwrap();
    for arm in ["structured", "ordinary"] {
        for metric in METRICS {
            let committed = &envelope["summary"][arm][metric];
            let observed = &report["entity_evidence"][arm][metric];
            assert_eq!(observed["mean"].as_f64(), committed["mean"].as_f64(), "{arm}.{metric}");
            assert_eq!(observed["n"].as_u64(), committed["n"].as_u64(), "{arm}.{metric}");
            assert_eq!(observed["mean"], json!(1.0), "{arm}.{metric}");
        }
    }
    for (id, metric) in [
        ("entity-evidence-v1-entity", "entity_accuracy"),
        ("entity-evidence-v1-claims", "eligible_claim_recall"),
        ("entity-evidence-v1-path", "path_correctness"),
        ("entity-evidence-v1-citations", "citation_resolution"),
        ("entity-evidence-v1-abstention", "appropriate_abstention"),
    ] {
        for arm in ["structured", "ordinary"] {
            let node = &report["entity_evidence"][arm][metric];
            if arm == "structured" {
                assert_eq!(node["mean"], json!(1.0));
            }
            let id = if arm == "ordinary" {
                id.replace("v1-", "v1-ordinary-")
            } else {
                id.to_string()
            };
            contextful_eval::record::emit(
                &id,
                node["mean"].as_f64().unwrap(),
                node["n"].as_u64().unwrap(),
                0,
            );
        }
    }
}

#[test]
fn entity_baseline_refuses_a_different_fixture_envelope_before_admission() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let mut baseline: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("evals/entity-evidence/v1.baseline.json")).unwrap(),
    )
    .unwrap();
    baseline["benchmark"]["version"] = json!("other/v1");
    let path = dir.path().join("baseline.json");
    std::fs::write(&path, baseline.to_string()).unwrap();
    let manifest = root.join("evals/entity-evidence/v1.json");
    let output = run(
        dir.path(),
        &[
            "eval",
            "entity-evidence",
            "--manifest",
            manifest.to_str().unwrap(),
            "--baseline",
            path.to_str().unwrap(),
        ],
        None,
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("baseline fixture version/config differs")
    );
}
