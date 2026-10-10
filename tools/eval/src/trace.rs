//! The trace store (`assurance.baseline.trace-store`) and the perimeter check on trace
//! export (`assurance.baseline.trace-export`).
//!
//! A trace store is a directory holding two JSON Lines files: `runs.jsonl`, one record per
//! run, and `staging.jsonl`, the cases a run stages for curation. It records verdicts the
//! floors and the baseline gate already decided, and nothing reads it back into one.
//!
//! A trace endpoint is hosted unless its host is a loopback or private address or a name
//! inside the operator's network. A hosted endpoint serves runs over fixtures alone: a run
//! touching a deployed store refuses before any row lands.

use std::fs::OpenOptions;
use std::io::{self, Write as _};
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::baseline::RunStamp;
use crate::case::{self, Case};
use crate::error::EvalError;
use crate::metrics::{RANKED_METRICS, RELEVANCE_METRICS};

/// The run history, one [`RunRecord`] per line.
pub const RUNS_FILE: &str = "runs.jsonl";

/// The curation staging, one [`Staged`] case per line.
pub const STAGING_FILE: &str = "staging.jsonl";

/// The leg whose return a run stages for curation: the one a caller's reader is handed.
pub const STAGED_LEG: &str = "hybrid";

/// Host-name suffixes that resolve inside an operator's network.
const INTERNAL_SUFFIXES: [&str; 5] = [".internal", ".local", ".lan", ".home.arpa", ".localhost"];

/// A verdict as the run decided it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Held,
    Red,
}

impl Verdict {
    pub fn of(held: bool) -> Verdict {
        if held {
            Verdict::Held
        } else {
            Verdict::Red
        }
    }
}

/// One run in the history: its configuration, its size, both verdicts it reached, and the
/// hybrid leg's means.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRecord {
    pub run: RunStamp,
    pub seed: u64,
    pub n_cases: u64,
    pub floors: Verdict,
    /// `None` when the run held no baseline.
    pub baseline: Option<Verdict>,
    /// `retrieval.hybrid.<metric>` → its mean, for each metric the report defines.
    pub figures: std::collections::BTreeMap<String, f64>,
}

impl RunRecord {
    /// The record of `report`, under the verdicts the run already reached.
    pub fn of(report: &Value, floors: Verdict, baseline: Option<Verdict>) -> Result<RunRecord, String> {
        let run = serde_json::from_value(report["run"].clone()).map_err(|e| format!("the report's run block: {e}"))?;
        let figures = RANKED_METRICS
            .iter()
            .chain(RELEVANCE_METRICS.iter())
            .filter_map(|m| report["retrieval"][STAGED_LEG][m]["mean"].as_f64().map(|v| (format!("retrieval.{STAGED_LEG}.{m}"), v)))
            .collect();
        Ok(RunRecord {
            run,
            seed: report["seed"].as_u64().unwrap_or_default(),
            n_cases: report["n_cases"].as_u64().unwrap_or_default(),
            floors,
            baseline,
            figures,
        })
    }
}

/// A case staged for a curator: its id, why, and the rows it returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Staged {
    pub case: String,
    pub reason: String,
    pub returned: Vec<String>,
}

/// The cases of `report` a curator reviews: a case whose staged leg returned none of its
/// expected artifacts, and a must-abstain case that returned rows.
pub fn staged(cases: &[Case], report: &Value) -> Vec<Staged> {
    let mut out = Vec::new();
    for (case, line) in cases.iter().zip(report["cases"].as_array().into_iter().flatten()) {
        let returned: Vec<String> =
            line["legs"][STAGED_LEG].as_array().into_iter().flatten().filter_map(|r| r.as_str().map(str::to_string)).collect();
        let relevant = case.relevant();
        let reason = if case.expected.must_abstain && !returned.is_empty() {
            Some("a must-abstain case returned rows".to_string())
        } else if !relevant.is_empty() && !returned.iter().filter_map(|r| case::row_ref(r)).any(|r| relevant.contains(&r)) {
            Some(format!("the {STAGED_LEG} leg returned none of {} expected artifact(s)", relevant.len()))
        } else {
            None
        };
        if let Some(reason) = reason {
            out.push(Staged { case: case.id.clone(), reason, returned });
        }
    }
    out
}

/// A directory recording run history and curation staging.
#[derive(Debug, Clone)]
pub struct TraceStore {
    dir: PathBuf,
}

fn append<T: Serialize>(path: &Path, items: &[T]) -> io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let mut text = String::new();
    for item in items {
        text.push_str(&serde_json::to_string(item).map_err(io::Error::other)?);
        text.push('\n');
    }
    file.write_all(text.as_bytes())?;
    file.sync_data()
}

fn read<T: for<'de> Deserialize<'de>>(path: &Path) -> io::Result<Vec<T>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    text.lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).map_err(io::Error::other)).collect()
}

impl TraceStore {
    /// The store at `dir`, created when absent.
    pub fn open(dir: &Path) -> io::Result<TraceStore> {
        std::fs::create_dir_all(dir)?;
        Ok(TraceStore { dir: dir.to_path_buf() })
    }

    /// Append one run to the history and its staged cases to the curation staging.
    pub fn record(&self, run: &RunRecord, staged: &[Staged]) -> io::Result<()> {
        append(&self.dir.join(RUNS_FILE), std::slice::from_ref(run))?;
        append(&self.dir.join(STAGING_FILE), staged)
    }

    /// Every run recorded, oldest first.
    pub fn history(&self) -> io::Result<Vec<RunRecord>> {
        read(&self.dir.join(RUNS_FILE))
    }

    /// Every case staged, oldest first.
    pub fn staging(&self) -> io::Result<Vec<Staged>> {
        read(&self.dir.join(STAGING_FILE))
    }
}

/// The host of an `http` or `https` endpoint, or why it names none.
fn host(endpoint: &str) -> Result<String, String> {
    let rest = endpoint
        .strip_prefix("http://")
        .or_else(|| endpoint.strip_prefix("https://"))
        .ok_or_else(|| format!("trace endpoint `{endpoint}` is no http(s) URL"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split_once(']').map(|(h, _)| h).ok_or_else(|| format!("trace endpoint `{endpoint}` holds an unclosed IPv6 literal"))?
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    if host.is_empty() {
        return Err(format!("trace endpoint `{endpoint}` names no host"));
    }
    Ok(host.to_ascii_lowercase())
}

/// Whether `endpoint` is a hosted collector, outside the operator's perimeter.
pub fn hosted(endpoint: &str) -> Result<bool, String> {
    let host = host(endpoint)?;
    if let Ok(ip) = host.parse::<IpAddr>() {
        let inside = match ip {
            IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
            IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local(),
        };
        return Ok(!inside);
    }
    let inside = host == "localhost" || !host.contains('.') || INTERNAL_SUFFIXES.iter().any(|s| host.ends_with(s));
    Ok(!inside)
}

/// Admit a run's trace export before any row lands: a run touching a deployed store with a
/// hosted endpoint configured raises `TraceExportOutOfPerimeter`; a run over fixtures alone
/// exports anywhere. An endpoint naming no host reads as hosted.
pub fn admit_export(endpoint: Option<&str>, deployed: &[String]) -> Result<(), EvalError> {
    let Some(endpoint) = endpoint else { return Ok(()) };
    let hosted = hosted(endpoint).unwrap_or(true);
    match deployed.first() {
        Some(store) if hosted => Err(EvalError::TraceExportOutOfPerimeter {
            endpoint: endpoint.to_string(),
            reason: format!("the run touches the deployed store `{store}`, and a hosted collector serves runs over fixtures alone"),
        }),
        _ => Ok(()),
    }
}
