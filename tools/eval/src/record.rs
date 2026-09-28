//! The record one measure writes (`assurance.measure.record`), and the runner stamp it
//! carries (`assurance.measure.runner-stamp`).
//!
//! A test hosting a ledger entry calls [`emit`]; `contextful-ci measure` sets
//! [`MEASURE_DIR_VAR`] for the run and reads each entry's record back with [`read`]. With
//! the variable unset — the workspace stage, a contributor's `cargo test` — [`emit`]
//! writes nothing.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::EvalError;

/// The directory a measure run collects records in; unset outside a measure run.
pub const MEASURE_DIR_VAR: &str = "CONTEXTFUL_MEASURE_DIR";

/// The machine a figure was measured on. A trend figure compares only against a baseline
/// carrying an equal stamp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runner {
    /// The processor model the operating system reports.
    pub processor: String,
    /// Processors available to the measuring process.
    pub nproc: u64,
    /// The memory limit of the process's control group in bytes; `None` when unlimited.
    pub memory_limit: Option<u64>,
}

impl Runner {
    /// The stamp of the machine running this process.
    pub fn detect() -> Runner {
        Runner {
            processor: processor_model(),
            nproc: std::thread::available_parallelism().map(|n| n.get() as u64).unwrap_or(1),
            memory_limit: memory_limit(),
        }
    }
}

fn processor_model() -> String {
    if let Ok(text) = std::fs::read_to_string("/proc/cpuinfo") {
        let model = text
            .lines()
            .find(|l| l.starts_with("model name") || l.starts_with("Model") || l.starts_with("uarch"))
            .and_then(|l| l.split_once(':'))
            .map(|(_, v)| v.trim().to_string());
        if let Some(m) = model.filter(|m| !m.is_empty()) {
            return m;
        }
    }
    Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| std::env::consts::ARCH.to_string())
}

/// The cgroup v2, then v1, memory limit; `max` and the v1 unlimited sentinel read as none.
fn memory_limit() -> Option<u64> {
    ["/sys/fs/cgroup/memory.max", "/sys/fs/cgroup/memory/memory.limit_in_bytes"]
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .find_map(|t| t.trim().parse::<u64>().ok())
        .filter(|b| *b < (1 << 62))
}

/// One measure's figure: its ledger entry, value, sample count, seed and runner stamp.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub id: String,
    pub value: f64,
    /// Samples behind the value: iterations, cases or measured items.
    pub n: u64,
    /// The seed every generated fixture and schedule derived from.
    pub seed: u64,
    pub run: Runner,
}

impl Record {
    pub fn new(id: &str, value: f64, n: u64, seed: u64) -> Record {
        Record { id: id.to_string(), value, n, seed, run: Runner::detect() }
    }
}

/// The file an entry's record lands in under `dir`.
pub fn path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.json"))
}

/// Write `record` under `dir`, replacing an earlier record of the same entry.
pub fn write(dir: &Path, record: &Record) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let p = path(dir, &record.id);
    let text = serde_json::to_string_pretty(record).map_err(std::io::Error::other)?;
    std::fs::write(&p, text + "\n")?;
    Ok(p)
}

/// Record a measure's figure when a measure run collects records; do nothing otherwise.
/// Returns the file written.
///
/// # Panics
///
/// When the collecting directory cannot be written, so a measure run never reads a
/// silently absent record as a pass.
pub fn emit(id: &str, value: f64, n: u64, seed: u64) -> Option<PathBuf> {
    let dir = std::env::var_os(MEASURE_DIR_VAR)?;
    let record = Record::new(id, value, n, seed);
    Some(write(Path::new(&dir), &record).unwrap_or_else(|e| panic!("writing the record of `{id}`: {e}")))
}

/// The record `id` left under `dir`: `MeasureRecordMissing` when none exists, it does not
/// parse, or it names another entry.
pub fn read(dir: &Path, id: &str) -> Result<Record, EvalError> {
    let p = path(dir, id);
    let text = std::fs::read_to_string(&p).map_err(|_| EvalError::record_missing(id, "the method finished and wrote no record"))?;
    let record: Record = serde_json::from_str(&text).map_err(|e| EvalError::record_missing(id, format!("the record does not parse: {e}")))?;
    if record.id != id {
        return Err(EvalError::record_missing(id, format!("the record names entry `{}`", record.id)));
    }
    if !record.value.is_finite() {
        return Err(EvalError::record_missing(id, "the record's value is not finite"));
    }
    Ok(record)
}
