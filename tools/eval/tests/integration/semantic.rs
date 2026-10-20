use contextful_eval::metrics::{Returned, RowRef, LEGS};
use contextful_eval::report::CaseRun;
use contextful_eval::{case, semantic::Benchmark};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

fn root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn the_versioned_cache_refuses_changed_inputs_and_missing_real_vectors() {
    let root = root().join("evals/semantic/v1");
    let benchmark = Benchmark::open(
        &root.join("manifest.json"),
        &root.join("heldout.jsonl"),
        10,
        "reciprocal-rank",
    )
    .unwrap();
    assert_eq!(benchmark.dimension(), 384);
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("heldout.jsonl"), "{}\n").unwrap();
    let error = Benchmark::open(
        &root.join("manifest.json"),
        &temp.path().join("heldout.jsonl"),
        10,
        "reciprocal-rank",
    )
    .unwrap_err();
    assert!(error.contains("held-out"), "{error}");
    std::fs::copy(
        root.join("heldout.jsonl"),
        temp.path().join("heldout.jsonl"),
    )
    .unwrap();
    let error = Benchmark::open(
        &root.join("manifest.json"),
        &temp.path().join("heldout.jsonl"),
        10,
        "reciprocal-rank",
    )
    .unwrap_err();
    assert!(
        error.contains("held-out"),
        "an identical copy cannot redirect the verified corpus: {error}"
    );
    assert!(Benchmark::open(
        &root.join("manifest.json"),
        &root.join("heldout.jsonl"),
        5,
        "reciprocal-rank"
    )
    .is_err());
}

#[test]
fn rehashed_missing_vectors_wrong_dimensions_and_unpinned_tables_are_rejected() {
    let fixture = root().join("evals/semantic/v1");
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture.join("manifest.json")).unwrap()).unwrap();
    for mutation in ["missing", "dimension", "hash", "extra", "model"] {
        let temp = tempfile::tempdir().unwrap();
        let mut manifest = manifest.clone();
        for name in manifest["files"].as_object().unwrap().keys() {
            let target = temp.path().join(name);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::copy(fixture.join(name), target).unwrap();
        }
        let name = "corpus/rows/kb/semantic.jsonl";
        if mutation == "extra" {
            std::fs::write(temp.path().join("corpus/rows/extra.jsonl"), "{}\n").unwrap();
        } else if mutation == "model" {
            manifest["model_artifacts"]["model.safetensors"] = json!("bad-hash");
        } else {
            let lines = std::fs::read_to_string(temp.path().join(name)).unwrap();
            let mut rows: Vec<serde_json::Value> = lines
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            if mutation == "dimension" {
                rows[0]["embedding"] = json!([1.0]);
            } else {
                rows[0].as_object_mut().unwrap().remove("embedding");
            }
            let text = rows
                .iter()
                .map(|row| format!("{row}\n"))
                .collect::<String>();
            std::fs::write(temp.path().join(name), &text).unwrap();
            if mutation != "hash" {
                manifest["files"][name] = json!(format!("{:x}", Sha256::digest(text.as_bytes())));
            }
        }
        std::fs::write(temp.path().join("manifest.json"), manifest.to_string()).unwrap();
        let error = Benchmark::open(
            &temp.path().join("manifest.json"),
            &temp.path().join("heldout.jsonl"),
            10,
            "reciprocal-rank",
        )
        .unwrap_err();
        assert!(error.contains("semantic"), "{mutation}: {error}");
    }
}

#[test]
fn quality_reports_counts_intervals_and_slice_regressions_apart_from_judged_answers() {
    let root = root().join("evals/semantic/v1");
    let benchmark = Benchmark::open(
        &root.join("manifest.json"),
        &root.join("heldout.jsonl"),
        10,
        "reciprocal-rank",
    )
    .unwrap();
    let cases = case::load(&std::fs::read_to_string(root.join("heldout.jsonl")).unwrap()).unwrap();
    assert!(cases.len() >= 24);
    let runs: Vec<_> = cases
        .iter()
        .map(|case| {
            let relevant = case.relevant().into_iter().next().unwrap();
            CaseRun::new(
                case,
                LEGS.into_iter()
                    .map(|leg| {
                        (
                            leg.to_string(),
                            vec![Returned {
                                row: relevant.clone(),
                                in_window: true,
                            }],
                        )
                    })
                    .collect::<BTreeMap<_, _>>(),
            )
        })
        .collect();
    let baseline = benchmark.report(&runs, 683).unwrap();
    assert_eq!(baseline["run"]["tier"], "semantic");
    assert!(baseline.get("judge").is_none());
    assert_eq!(baseline["retrieval"]["vector"]["ndcg_at_k"]["mean"], 1.0);
    assert_eq!(
        baseline["retrieval"]["vector"]["ndcg_at_k"]["n"],
        cases.len()
    );
    assert_eq!(
        baseline["retrieval"]["vector"]["ndcg_at_k"]["interval"]["lo"],
        1.0
    );
    let mut misses = runs.clone();
    for run in &mut misses {
        run.legs.get_mut("vector").unwrap()[0].row = RowRef::new("kb/semantic", "absent");
    }
    let report = benchmark.report(&misses, 683).unwrap();
    let regressions = benchmark.regressions(&report, &baseline).unwrap();
    assert!(!regressions.is_empty());
    assert!(regressions.iter().any(|r| {
        r["path"]
            .as_str()
            .unwrap()
            .contains("slices.cross_language")
    }));
    let mut wrong = baseline.clone();
    wrong["benchmark"]["identity"] = json!("wrong");
    assert!(benchmark.regressions(&report, &wrong).is_err());
}
