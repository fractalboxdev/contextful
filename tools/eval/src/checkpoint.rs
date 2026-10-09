//! The run checkpoint (`assurance.evaluate.checkpoint`): a JSON Lines file a run appends
//! each finished case's result to, so a killed run resumes where it stopped.
//!
//! The first line is the header, the run block and seed the results were measured under;
//! each further line is one case's result, written whole and synced before the next case
//! starts. A trailing line that does not parse is the case a crash interrupted: opening
//! the file truncates it away, and that case alone runs again.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::baseline::RunStamp;
use crate::judge::CaseJudgment;
use crate::metrics::Returned;
use crate::systems::Systems;

/// The configuration a checkpoint's results were measured under.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub run: RunStamp,
    pub seed: u64,
}

/// One finished case: its return on each leg, its edge returns and its systems figures.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseResult {
    pub id: String,
    pub legs: BTreeMap<String, Vec<Returned>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edges: Option<BTreeMap<String, Vec<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub systems: Option<Systems>,
    /// The case's answer and judged verdicts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judged: Option<CaseJudgment>,
}

/// A checkpoint that cannot resume this run.
#[derive(Debug)]
pub enum CheckpointError {
    Io { path: PathBuf, source: io::Error },
    /// The header records another run block or seed than this run's.
    Mismatch { path: PathBuf, recorded: String, run: String },
    /// A line before the last does not parse, or repeats a case: the file was edited, not torn.
    Corrupt { path: PathBuf, line: usize, reason: String },
}

impl fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CheckpointError::Io { path, source } => write!(f, "checkpoint `{}`: {source}", path.display()),
            CheckpointError::Mismatch { path, recorded, run } => {
                write!(f, "checkpoint `{}` was written under {recorded}; this run is {run}", path.display())
            }
            CheckpointError::Corrupt { path, line, reason } => write!(f, "checkpoint `{}` line {line}: {reason}", path.display()),
        }
    }
}

impl std::error::Error for CheckpointError {}

/// An open checkpoint, appending after the last whole result.
#[derive(Debug)]
pub struct Checkpoint {
    path: PathBuf,
    file: File,
}

fn header_text(h: &Header) -> String {
    format!("{} seed={}", h.run, h.seed)
}

impl Checkpoint {
    /// Open the checkpoint at `path` for a run under `header`, creating it when absent, and
    /// return the results it already holds by case id. A torn trailing line is truncated.
    pub fn open(path: &Path, header: &Header) -> Result<(Checkpoint, BTreeMap<String, CaseResult>), CheckpointError> {
        let io = |source| CheckpointError::Io { path: path.to_path_buf(), source };
        let text = match std::fs::read(path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(io(e)),
        };
        let mut done = BTreeMap::new();
        let mut keep = 0usize;
        let mut header_seen = false;
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        for (i, raw) in lines.iter().enumerate() {
            let last = i + 1 == lines.len();
            // A line the crash cut short carries no newline, or does not parse.
            let whole = raw.ends_with('\n');
            let line = raw.trim_end_matches('\n');
            let corrupt = |reason: String| CheckpointError::Corrupt { path: path.to_path_buf(), line: i + 1, reason };
            if !header_seen {
                match serde_json::from_str::<Header>(line) {
                    Ok(h) if whole => {
                        if h != *header {
                            return Err(CheckpointError::Mismatch { path: path.to_path_buf(), recorded: header_text(&h), run: header_text(header) });
                        }
                        header_seen = true;
                    }
                    _ if last => break,
                    Ok(_) => unreachable!("a line before the last ends in a newline"),
                    Err(e) => return Err(corrupt(format!("the header does not parse: {e}"))),
                }
            } else {
                match serde_json::from_str::<CaseResult>(line) {
                    Ok(r) if whole => {
                        if done.contains_key(&r.id) {
                            return Err(corrupt(format!("case `{}` repeats an earlier result", r.id)));
                        }
                        done.insert(r.id.clone(), r);
                    }
                    _ if last => break,
                    Ok(_) => unreachable!("a line before the last ends in a newline"),
                    Err(e) => return Err(corrupt(e.to_string())),
                }
            }
            keep += raw.len();
        }
        let file = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path).map_err(io)?;
        file.set_len(keep as u64).map_err(io)?;
        let mut cp = Checkpoint { path: path.to_path_buf(), file };
        if !header_seen {
            let line = serde_json::to_string(header).expect("a header serializes");
            cp.append_line(&line).map_err(io)?;
        }
        Ok((cp, done))
    }

    fn append_line(&mut self, line: &str) -> io::Result<()> {
        use std::io::Seek as _;
        self.file.seek(io::SeekFrom::End(0))?;
        self.file.write_all(line.as_bytes())?;
        self.file.write_all(b"\n")?;
        self.file.sync_data()
    }

    /// Append one finished case, synced before this returns.
    pub fn append(&mut self, result: &CaseResult) -> Result<(), CheckpointError> {
        let line = serde_json::to_string(result).expect("a case result serializes");
        self.append_line(&line).map_err(|source| CheckpointError::Io { path: self.path.clone(), source })
    }
}
