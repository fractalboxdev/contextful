//! Versioned semantic-quality evidence over the caller's actual ranked read results.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::case::{self, Case};
use crate::judge::{bootstrap_interval, BOOTSTRAP_RESAMPLES, INTERVAL_LEVEL};
use crate::metrics::{self, Aggregate, RowRef, LEGS};
use crate::report::CaseRun;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: String,
    model: String,
    revision: String,
    license: String,
    dimension: usize,
    heldout: String,
    files: BTreeMap<String, String>,
    model_artifacts: BTreeMap<String, String>,
}

/// Verified cache identity and its frozen retrieval configuration.
#[derive(Debug)]
pub struct Benchmark {
    manifest: Manifest,
    identity: String,
    config: Value,
    cases: Vec<Case>,
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

/// The one table a semantic corpus holds, as its manifest key.
const CORPUS_TABLE: &str = "corpus/rows/kb/semantic.jsonl";

/// `path`'s key in the manifest: its components under `root`, joined with `/` on every host.
pub fn manifest_key(root: &Path, path: &Path) -> Result<String, String> {
    Ok(path
        .strip_prefix(root)
        .map_err(|e| e.to_string())?
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/"))
}

/// `dimension` numbers, each finite in the single precision the store keeps, whose squared
/// norm in that precision is finite and above zero.
fn vector(value: &Value, dimension: usize) -> bool {
    let Some(v) = value.as_array().filter(|v| v.len() == dimension) else {
        return false;
    };
    let mut norm = 0.0f32;
    for x in v {
        let Some(x) = x.as_f64().map(|x| x as f32).filter(|x| x.is_finite()) else {
            return false;
        };
        norm += x * x;
    }
    norm.is_finite() && norm > 0.0
}

impl Benchmark {
    /// Verify every input before a scratch store opens; a cache never falls back to stubs.
    pub fn open(path: &Path, goldens: &Path, k: usize, fusion: &str) -> Result<Self, String> {
        let read = |p: &Path| {
            std::fs::read(p).map_err(|e| format!("semantic benchmark `{}`: {e}", p.display()))
        };
        let raw = read(path)?;
        let manifest: Manifest = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
        let root = path.parent().unwrap_or(Path::new("."));
        if manifest.version.is_empty()
            || manifest.model.is_empty()
            || manifest.license.is_empty()
            || manifest.revision.len() != 40
            || !manifest.revision.bytes().all(|b| b.is_ascii_hexdigit())
            || manifest.dimension == 0
            || manifest.model_artifacts.is_empty()
        {
            return Err("semantic benchmark requires a pinned model revision, license, dimension and artifact hashes".into());
        }
        for (name, digest) in &manifest.model_artifacts {
            if !safe_path(name)
                || digest.len() != 64
                || !digest.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err("semantic benchmark model artifact digest is malformed".into());
            }
        }
        for (name, digest) in &manifest.files {
            if !safe_path(name) || hash(&read(&root.join(name))?) != *digest {
                return Err(format!("semantic benchmark input hash differs: {name}"));
            }
        }
        for required in [
            manifest.heldout.as_str(),
            "config.json",
            "development.jsonl",
            "corpus/contextful.toml",
            CORPUS_TABLE,
        ] {
            if !manifest.files.contains_key(required) {
                return Err(format!("semantic benchmark input has no hash: {required}"));
            }
        }
        let mut stack = vec![root.join("corpus/rows")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                let path = entry.path();
                let kind = entry.file_type().map_err(|e| e.to_string())?;
                if kind.is_symlink() {
                    return Err("semantic corpus contains a symbolic link".into());
                }
                if kind.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "jsonl") {
                    let relative = manifest_key(root, &path)?;
                    if relative != CORPUS_TABLE {
                        return Err(format!(
                            "semantic corpus holds a table other than kb/semantic: {relative}"
                        ));
                    }
                }
            }
        }
        if goldens.canonicalize().map_err(|e| e.to_string())?
            != root
                .join(&manifest.heldout)
                .canonicalize()
                .map_err(|e| e.to_string())?
        {
            return Err(
                "semantic benchmark held-out case file differs from the pinned input".into(),
            );
        }
        let config: Value =
            serde_json::from_slice(&read(&root.join("config.json"))?).map_err(|e| e.to_string())?;
        if config["k"].as_u64() != Some(k as u64)
            || config["fusion"].as_str() != Some(fusion)
            || config["seed"].as_u64().is_none()
            || k == 0
            || config["regression_tolerance"]
                .as_f64()
                .is_none_or(|v| !v.is_finite() || v < 0.0)
        {
            return Err(
                "semantic benchmark retrieval configuration differs from the pinned configuration"
                    .into(),
            );
        }
        let text = String::from_utf8(read(goldens)?).map_err(|e| e.to_string())?;
        let cases = case::load(&text).map_err(|e| e.to_string())?;
        let rows = String::from_utf8(read(&root.join(CORPUS_TABLE))?).map_err(|e| e.to_string())?;
        let mut documents = BTreeSet::new();
        for line in rows.lines() {
            let row: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
            let id = row["doc_id"]
                .as_str()
                .ok_or("semantic document has no id")?;
            if !documents.insert(format!("kb/semantic#{id}"))
                || !vector(&row["embedding"], manifest.dimension)
            {
                return Err(format!(
                    "semantic document `{id}` has a duplicate id or a missing, wrong-dimension, zero-norm or non-finite single-precision embedding"
                ));
            }
        }
        let development =
            String::from_utf8(read(&root.join("development.jsonl"))?).map_err(|e| e.to_string())?;
        let development: Vec<Value> = development
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        for case in &cases {
            let query = serde_json::to_value(&case.query_embedding).map_err(|e| e.to_string())?;
            if case.corpus != "corpus"
                || case.prefix.as_deref() != Some("kb/semantic")
                || !vector(&query, manifest.dimension)
                || case.expected.artifacts.is_empty()
                || case
                    .expected
                    .artifacts
                    .iter()
                    .any(|a| !documents.contains(a))
                || development.iter().any(|d| {
                    d["id"].as_str() == Some(&case.id)
                        || d["question"].as_str() == Some(&case.question)
                })
            {
                return Err(format!(
                    "semantic held-out case `{}` has invalid truth, split or cached embedding",
                    case.id
                ));
            }
        }
        for tag in [
            "heldout",
            "paraphrase",
            "hard_negative",
            "alias",
            "en",
            "ja",
            "cross_language",
        ] {
            if !cases.iter().any(|c| c.tags.iter().any(|t| t == tag)) {
                return Err(format!("semantic benchmark has no `{tag}` slice"));
            }
        }
        Ok(Self {
            manifest,
            identity: hash(&raw),
            config,
            cases,
        })
    }

    pub fn dimension(&self) -> usize {
        self.manifest.dimension
    }

    pub fn seed(&self) -> u64 {
        self.config["seed"]
            .as_u64()
            .expect("verified configuration seed")
    }

    /// Semantic metrics contain no model-judged answer or deterministic safety verdict.
    pub fn report(&self, runs: &[CaseRun<'_>], seed: u64) -> Result<Value, String> {
        if seed != self.seed() {
            return Err("semantic report seed differs from the pinned configuration".into());
        }
        if runs.len() != self.cases.len() || runs.iter().zip(&self.cases).any(|(r, c)| r.case != c)
        {
            return Err("semantic results differ from the verified held-out cases".into());
        }
        let k = self.config["k"].as_u64().unwrap() as usize;
        let mut groups: BTreeMap<String, Vec<&CaseRun<'_>>> = BTreeMap::new();
        for run in runs {
            groups.entry(String::new()).or_default().push(run);
            for tag in &run.case.tags {
                groups.entry(tag.clone()).or_default().push(run);
            }
        }
        let mut out = json!({
            "run": {"tier": "semantic", "k": k, "seed": seed},
            "benchmark": {"identity": self.identity, "manifest": self.manifest, "config": self.config},
            "n_cases": runs.len(), "slices": {},
            "cases": runs.iter().map(|r| json!({"id": r.case.id, "legs": r.legs})).collect::<Vec<_>>()
        });
        for (tag, group) in groups {
            let mut retrieval = Map::new();
            for leg in LEGS {
                let mut values: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
                for run in &group {
                    let returned = run.legs.get(leg).ok_or_else(|| {
                        format!("semantic case `{}` has no {leg} result", run.case.id)
                    })?;
                    let ranking: Vec<RowRef> = returned.iter().map(|r| r.row.clone()).collect();
                    let scores = metrics::score_ranked(&ranking, &run.case.relevant(), k);
                    for (metric, value) in [
                        ("ndcg_at_k", scores.ndcg_at_k),
                        ("recall_at_k", scores.recall_at_k),
                        ("reciprocal_rank", scores.reciprocal_rank),
                        ("r_precision", scores.r_precision),
                        ("hit_rate_at_k", scores.hit_rate_at_k),
                    ] {
                        values.entry(metric).or_default().push(value);
                    }
                }
                let figures: Map<String, Value> = values.into_iter().map(|(metric, samples)| {
                    let mut figure = serde_json::to_value(samples.iter().copied().collect::<Aggregate>().summary()).unwrap();
                    let (lo, hi) = bootstrap_interval(&samples, seed).unwrap();
                    figure["interval"] = json!({"method": "case_percentile_bootstrap", "level": INTERVAL_LEVEL, "resamples": BOOTSTRAP_RESAMPLES, "lo": lo, "hi": hi});
                    (metric.to_string(), figure)
                }).collect();
                retrieval.insert(leg.to_string(), Value::Object(figures));
            }
            if tag.is_empty() {
                out["retrieval"] = Value::Object(retrieval);
            } else {
                out["slices"][tag] = json!({"n_cases": group.len(), "retrieval": retrieval});
            }
        }
        Ok(out)
    }

    /// Comparable baselines report each overall and slice metric's mean regression.
    pub fn regressions(&self, report: &Value, baseline: &Value) -> Result<Vec<Value>, String> {
        if baseline["benchmark"]["identity"].as_str() != Some(&self.identity)
            || report["benchmark"]["identity"].as_str() != Some(&self.identity)
        {
            return Err("semantic baseline input/model/config identity differs".into());
        }
        let mut paths = vec![(
            "retrieval".to_string(),
            &report["retrieval"],
            &baseline["retrieval"],
        )];
        for (tag, slice) in report["slices"]
            .as_object()
            .ok_or("semantic report has no slices")?
        {
            paths.push((
                format!("slices.{tag}.retrieval"),
                &slice["retrieval"],
                &baseline["slices"][tag]["retrieval"],
            ));
        }
        let tolerance = self.config["regression_tolerance"].as_f64().unwrap();
        let mut regressions = Vec::new();
        for (prefix, current, previous) in paths {
            for leg in LEGS {
                for metric in ["ndcg_at_k", "recall_at_k", "reciprocal_rank"] {
                    let value = current[leg][metric]["mean"]
                        .as_f64()
                        .ok_or("semantic report metric absent")?;
                    let base = previous[leg][metric]["mean"]
                        .as_f64()
                        .ok_or("semantic baseline metric absent")?;
                    if !base.is_finite() || previous[leg][metric]["n"] != current[leg][metric]["n"]
                    {
                        return Err("semantic baseline sample count or value differs".into());
                    }
                    if value < base - tolerance {
                        regressions.push(json!({"path": format!("{prefix}.{leg}.{metric}"), "mean": value, "baseline": base, "delta": value - base, "tolerance": tolerance}));
                    }
                }
            }
        }
        Ok(regressions)
    }
}
