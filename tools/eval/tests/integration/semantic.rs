use contextful_eval::case;
use contextful_eval::metrics::{Returned, RowRef, LEGS};
use contextful_eval::report::CaseRun;
use contextful_eval::semantic::{self, Benchmark};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Copy every pinned input of the `v1` cache under `dir`; returns its manifest for mutation.
fn copy_fixture(dir: &Path) -> serde_json::Value {
    let fixture = root().join("evals/semantic/v1");
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture.join("manifest.json")).unwrap()).unwrap();
    for name in manifest["files"].as_object().unwrap().keys() {
        let target = dir.join(name);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(fixture.join(name), target).unwrap();
    }
    manifest
}

/// Open the cache under `dir` at the pinned k and fusion.
fn open(dir: &Path) -> Result<Benchmark, String> {
    Benchmark::open(
        &dir.join("manifest.json"),
        &dir.join("heldout.jsonl"),
        10,
        "reciprocal-rank",
    )
    .map_err(|e| e.to_string())
}

/// Replace the first canonical document's first element with `value`, rehash and write the manifest.
fn mutate_first_element(dir: &Path, manifest: &mut serde_json::Value, value: f64) {
    let name = "corpus/rows/kb/semantic.jsonl";
    let lines = std::fs::read_to_string(dir.join(name)).unwrap();
    let mut rows: Vec<serde_json::Value> = lines
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    rows[0]["embedding"][0] = json!(value);
    let text = rows
        .iter()
        .map(|row| format!("{row}\n"))
        .collect::<String>();
    std::fs::write(dir.join(name), &text).unwrap();
    manifest["files"][name] = json!(digest(text.as_bytes()));
    std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
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
    .unwrap_err()
    .to_string();
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
    .unwrap_err()
    .to_string();
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
    for mutation in ["missing", "dimension", "hash", "extra", "model"] {
        let temp = tempfile::tempdir().unwrap();
        let mut manifest = copy_fixture(temp.path());
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
        let error = open(temp.path()).unwrap_err();
        assert!(error.contains("semantic"), "{mutation}: {error}");
    }
}

/// A second table under `corpus/rows/` is refused by name, declared and hashed or not, with
/// or without a vector: the cache holds one table, and no row reaches a store unverified.
#[test]
fn a_second_table_is_refused_whether_pinned_declared_or_carrying_a_vector() {
    for embedding in [None, Some(json!([1.0, 0.0])), Some(json!(vec![1.0; 384]))] {
        let temp = tempfile::tempdir().unwrap();
        let mut manifest = copy_fixture(temp.path());
        let mut row =
            json!({"doc_id": "extra", "text": "A second table reaches the read arms unverified."});
        if let Some(vector) = embedding {
            row["embedding"] = vector;
        }
        let table = "corpus/rows/kb/semantic-extra.jsonl";
        let text = format!("{row}\n");
        std::fs::write(temp.path().join(table), &text).unwrap();
        manifest["files"][table] = json!(digest(text.as_bytes()));
        let declaration = temp.path().join("corpus/contextful.toml");
        let toml = std::fs::read_to_string(&declaration).unwrap()
            + "\n[[pipeline.tables]]\nname = \"kb/semantic-extra\"\nprimary_key = [\"doc_id\"]\n";
        std::fs::write(&declaration, &toml).unwrap();
        manifest["files"]["corpus/contextful.toml"] = json!(digest(toml.as_bytes()));
        std::fs::write(temp.path().join("manifest.json"), manifest.to_string()).unwrap();
        let error = open(temp.path()).unwrap_err();
        assert!(
            error.contains("EvalSemanticCacheUnverified")
                && error.contains("other than kb/semantic")
                && error.contains("kb/semantic-extra"),
            "{error}"
        );
    }
}

/// Elements are checked in the single precision the store keeps: a finite f64 whose f32 is
/// infinite, and a vector whose squared norm overflows f32, are both refused.
#[test]
fn vectors_outside_single_precision_are_refused() {
    for (name, value) in [("element", 1e39), ("norm", 3e19)] {
        let temp = tempfile::tempdir().unwrap();
        let mut manifest = copy_fixture(temp.path());
        mutate_first_element(temp.path(), &mut manifest, value);
        let error = open(temp.path()).unwrap_err();
        assert!(
            error.contains("EvalSemanticInputRejected") && error.contains("semantic document"),
            "{name}: {error}"
        );
    }
    let temp = tempfile::tempdir().unwrap();
    let mut manifest = copy_fixture(temp.path());
    mutate_first_element(temp.path(), &mut manifest, 0.5);
    assert!(open(temp.path()).is_ok());
}

/// A table's manifest key joins its path components with `/` under any host separator.
#[test]
fn a_table_key_joins_components_with_slashes_on_any_host() {
    let root: PathBuf = ["cache", "v1"].iter().collect();
    let path: PathBuf = ["cache", "v1", "corpus", "rows", "kb", "semantic.jsonl"]
        .iter()
        .collect();
    assert_eq!(
        semantic::manifest_key(&root, &path).unwrap(),
        "corpus/rows/kb/semantic.jsonl"
    );
    assert!(semantic::manifest_key(Path::new("elsewhere"), &path).is_err());
}

/// A symbolic link in place of the manifest, a pinned input or the held-out argument is
/// refused before any input is read through it: hashes and the held-out comparison follow
/// links, so a link would admit bytes from outside the verified cache.
#[cfg(unix)]
#[test]
fn symbolic_links_to_the_manifest_inputs_or_held_out_file_are_refused() {
    use std::os::unix::fs::symlink;
    for link in ["heldout.jsonl", "config.json", "manifest.json", "goldens"] {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        copy_fixture(&cache);
        std::fs::copy(
            root().join("evals/semantic/v1/manifest.json"),
            cache.join("manifest.json"),
        )
        .unwrap();
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let mut goldens = cache.join("heldout.jsonl");
        if link == "goldens" {
            goldens = outside.join("heldout.jsonl");
            symlink(cache.join("heldout.jsonl"), &goldens).unwrap();
        } else {
            std::fs::rename(cache.join(link), outside.join(link)).unwrap();
            symlink(outside.join(link), cache.join(link)).unwrap();
        }
        let error = Benchmark::open(
            &cache.join("manifest.json"),
            &goldens,
            10,
            "reciprocal-rank",
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
        .unwrap_err();
        assert!(
            error.contains("EvalSemanticCacheUnverified") && error.contains("symbolic link"),
            "{link}: {error}"
        );
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
