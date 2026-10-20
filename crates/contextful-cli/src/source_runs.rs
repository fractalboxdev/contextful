//! Version-bound source fire evidence and the summaries carried by each node's run state.

use anyhow::Result;
use contextful_core::pipeline::declare::PipelineSpec;
use contextful_core::run::record::{RunRow, RunStatus};
use contextful_sync::{SourceFire, SourceRuns};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;

pub(crate) type Summaries = BTreeMap<(String, u64, String), SourceRuns>;

/// Merge first starts and latest completions under a versioned configuration identity.
pub(crate) fn merge(into: &mut Summaries, summaries: impl IntoIterator<Item = SourceRuns>) {
    for summary in summaries {
        let key = (summary.pipeline_id.clone(), summary.version, summary.config_hash.clone());
        match into.get_mut(&key) {
            None => { into.insert(key, summary); }
            Some(prior) => {
                if (summary.first.started_at, &summary.first.run_id) < (prior.first.started_at, &prior.first.run_id) { prior.first = summary.first; }
                if (summary.latest.started_at, summary.latest.ended_at.unwrap_or(summary.latest.started_at), &summary.latest.run_id) >
                    (prior.latest.started_at, prior.latest.ended_at.unwrap_or(prior.latest.started_at), &prior.latest.run_id) { prior.latest = summary.latest; }
            }
        }
    }
}

/// Record one fire's version and row identities in the existing operational record.
pub(crate) fn record(audit: &crate::project::ReadAudit, version: u64, spec: &PipelineSpec, fire: &str, rows: &[String], failed: bool) -> Result<()> {
    if !rows.is_empty() {
        audit.append(json!({ "contextful.control.version": version, "contextful.pipeline.id": spec.id,
            "contextful.pipeline.hash": spec.content_hash(), "contextful.run.id": fire,
            "contextful.run.rows": rows, "contextful.run.failed": failed }))?;
    }
    Ok(())
}

/// Join local fire attestations to catalog rows and retain first/latest per configuration.
pub(crate) fn local(audit_dir: &Path, rows: &[RunRow]) -> Result<Vec<SourceRuns>> {
    if !audit_dir.exists() { return Ok(Vec::new()); }
    let by_id: BTreeMap<_, _> = rows.iter().map(|row| (row.run_id.as_str(), row)).collect();
    let mut summaries = Summaries::new();
    for entry in contextful_policy::audit::entries(audit_dir)? {
        let attributes = entry.attributes;
        let (Some(pipeline), Some(version), Some(hash), Some(run), Some(ids)) = (
            attributes["contextful.pipeline.id"].as_str(), attributes["contextful.control.version"].as_u64(),
            attributes["contextful.pipeline.hash"].as_str(), attributes["contextful.run.id"].as_str(), attributes["contextful.run.rows"].as_array(),
        ) else { continue; };
        let ids: std::collections::BTreeSet<_> = ids.iter().filter_map(|id| id.as_str()).collect();
        let rows: Vec<_> = ids.into_iter().filter_map(|id| by_id.get(id).copied()).filter(|row| row.pipeline_id == pipeline).collect();
        let Some(first) = rows.iter().min_by_key(|row| row.started_at) else { continue; };
        let failed = attributes["contextful.run.failed"] == true || rows.iter().any(|row| row.status != RunStatus::Success);
        let fire = SourceFire { run_id: run.to_string(), status: if failed { RunStatus::Failed } else { RunStatus::Success },
            rows: rows.iter().fold(0_u64, |sum, row| sum.saturating_add(row.rows)), started_at: first.started_at,
            ended_at: rows.iter().filter_map(|row| row.ended_at).max(), error_kind: rows.iter().find_map(|row| row.error_kind) };
        merge(&mut summaries, [SourceRuns { pipeline_id: pipeline.to_string(), version, config_hash: hash.to_string(), first: fire.clone(), latest: fire }]);
    }
    Ok(summaries.into_values().collect())
}
