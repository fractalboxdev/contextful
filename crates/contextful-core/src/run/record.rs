//! `run.record`: the run row, its seven statuses, the owner lease, the site id and the
//! windows history is read through.

use super::failure::FailureTag;
use super::RunError;
use crate::time::Instant;
use serde::{Deserialize, Serialize};

/// A run status (`run.record.status-set`). The record and the wire snapshot share this
/// one spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Pending,
    Running,
    Waiting,
    Success,
    PartialFailure,
    Failed,
    Canceled,
}

impl RunStatus {
    pub const ALL: [RunStatus; 7] = [
        RunStatus::Pending,
        RunStatus::Running,
        RunStatus::Waiting,
        RunStatus::Success,
        RunStatus::PartialFailure,
        RunStatus::Failed,
        RunStatus::Canceled,
    ];

    pub fn name(self) -> &'static str {
        match self {
            RunStatus::Pending => "pending",
            RunStatus::Running => "running",
            RunStatus::Waiting => "waiting",
            RunStatus::Success => "success",
            RunStatus::PartialFailure => "partial_failure",
            RunStatus::Failed => "failed",
            RunStatus::Canceled => "canceled",
        }
    }

    pub fn parse(s: &str) -> Option<RunStatus> {
        RunStatus::ALL.into_iter().find(|t| t.name() == s)
    }

    /// `success`, `partial_failure`, `failed` and `canceled` end a run.
    pub fn is_terminal(self) -> bool {
        matches!(self, RunStatus::Success | RunStatus::PartialFailure | RunStatus::Failed | RunStatus::Canceled)
    }

    /// `pending`, `running` and `waiting` are in flight.
    pub fn is_in_flight(self) -> bool {
        !self.is_terminal()
    }

    /// Whether an upstream health observation counts this status. A deliberate stop is
    /// not a health signal (`run.cancel.distinct-terminal-status`).
    pub fn observed_by_health(self) -> bool {
        self != RunStatus::Canceled
    }
}

impl std::fmt::Display for RunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Renewal cadence of a run row's owner lease: 10 s (`run.record.owner-lease`).
pub const OWNER_LEASE_RENEWAL_SECS: u64 = 10;
/// Life of an unrenewed owner lease: 30 s (`run.record.owner-lease`).
pub const OWNER_LEASE_TTL_SECS: u64 = 30;

/// The process holding a `running` or `waiting` row, and until when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    pub pid: u32,
    pub boot_id: String,
    pub lease_expires_at: Instant,
}

impl Owner {
    /// A lease taken or renewed at `now`.
    pub fn leased(pid: u32, boot_id: impl Into<String>, now: Instant) -> Owner {
        Owner { pid, boot_id: boot_id.into(), lease_expires_at: now.plus_secs(OWNER_LEASE_TTL_SECS) }
    }

    pub fn renewed(&self, now: Instant) -> Owner {
        Owner::leased(self.pid, self.boot_id.clone(), now)
    }

    /// Whether the lease has lapsed at `now`.
    pub fn expired(&self, now: Instant) -> bool {
        now >= self.lease_expires_at
    }

    /// Whether a renewal is due at `now`, given the lease was last taken at `taken`.
    pub fn renewal_due(taken: Instant, now: Instant) -> bool {
        taken.secs_until(now) >= OWNER_LEASE_RENEWAL_SECS
    }
}

/// The phase a run row records: written at plan, and again at commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Plan,
    Commit,
}

/// The stop a caller wrote onto a run row (`run.cancel.catalog-channel`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopMark {
    pub requested_at: Instant,
    /// The stored scope. An unrecognized spelling reads as `run`.
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// One run row (`run.record.columns`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRow {
    pub run_id: String,
    pub pipeline_id: String,
    pub table: String,
    pub site_id: String,
    pub status: RunStatus,
    #[serde(default)]
    pub owner: Option<Owner>,
    pub started_at: Instant,
    #[serde(default)]
    pub ended_at: Option<Instant>,
    /// Rows and bytes measured at the destination (`run.record.counts-at-destination`).
    pub rows: u64,
    pub bytes: u64,
    /// Batches the run landed.
    pub batches: u64,
    #[serde(default)]
    pub error_kind: Option<FailureTag>,
    #[serde(default)]
    pub error_message: Option<String>,
    pub connector_id: String,
    pub connector_version: String,
    pub connector_hash: String,
    #[serde(default)]
    pub trace_id: Option<String>,
    pub phase: Phase,
    /// The execution whose journal this attempt resolves against (`run.own.execution-id-keys-the-journal`).
    pub execution_id: String,
    #[serde(default)]
    pub stop: Option<StopMark>,
    /// The host scope an execution opened under; `None` for a table or chunk run, whose
    /// pipeline and table name its scope (`run.own.host-scope`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_scope: Option<String>,
}

impl RunRow {
    /// The status startup reaps this row to: `partial_failure` for a non-terminal row
    /// whose owner lease expired, nothing for a row a live process holds
    /// (`run.record.orphan-reap`).
    pub fn reaped(&self, now: Instant) -> Option<RunStatus> {
        match &self.owner {
            Some(owner) if self.status.is_in_flight() && owner.expired(now) => Some(RunStatus::PartialFailure),
            _ => None,
        }
    }
}

/// Shortest site id: 1 char (`run.record.site-id-length`).
pub const SITE_ID_MIN_LEN: usize = 1;
/// Longest site id: 64 chars (`run.record.site-id-length`).
pub const SITE_ID_MAX_LEN: usize = 64;

/// A site id that holds to its length and character set.
pub fn check_site_id(id: &str) -> Result<&str, RunError> {
    let n = id.chars().count();
    let charset = id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if (SITE_ID_MIN_LEN..=SITE_ID_MAX_LEN).contains(&n) && charset {
        Ok(id)
    } else {
        Err(RunError::Invalid(format!(
            "site id `{id}` is not {SITE_ID_MIN_LEN} to {SITE_ID_MAX_LEN} chars of letters, digits, `.`, `_` and `-`"
        )))
    }
}

/// Where a deployment declares its site id: in the manifest, or by naming an
/// environment variable and the value that variable holds in this process.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SiteIdSources {
    pub manifest: Option<String>,
    /// `(variable name, its value if set)`.
    pub env: Option<(String, Option<String>)>,
}

/// Resolve the site id at startup (`run.record.site-id-unresolved`).
pub fn resolve_site_id(sources: &SiteIdSources) -> Result<String, RunError> {
    let id = match (&sources.manifest, &sources.env) {
        (Some(_), Some((var, _))) => {
            return Err(RunError::SiteIdUnresolved(format!("the manifest declares a site id and names `{var}` as well; declare one")))
        }
        (Some(id), None) => id.clone(),
        (None, Some((_, Some(value)))) => value.clone(),
        (None, Some((var, None))) => {
            return Err(RunError::SiteIdUnresolved(format!("the site id variable `{var}` is unset")))
        }
        (None, None) => {
            return Err(RunError::SiteIdUnresolved("no site id: declare one in the manifest or name its variable".into()))
        }
    };
    check_site_id(&id)?;
    Ok(id)
}

/// Runs the describe surface returns unasked: 5 rows (`run.record.describe-window`).
pub const DESCRIBE_DEFAULT_ROWS: usize = 5;
/// Highest ceiling the describe surface honors: 500 rows (`run.record.describe-window`).
pub const DESCRIBE_CEILING_ROWS: usize = 500;
/// Runs the export returns unasked: 500 rows (`run.record.export-ceiling`).
pub const EXPORT_DEFAULT_ROWS: usize = 500;
/// Highest ceiling the export honors, per pipeline: 5000 rows (`run.record.export-ceiling`).
pub const EXPORT_CEILING_ROWS: usize = 5000;

/// The describe surface's row ceiling for a caller's request.
pub fn describe_ceiling(requested: Option<usize>) -> usize {
    requested.unwrap_or(DESCRIBE_DEFAULT_ROWS).min(DESCRIBE_CEILING_ROWS)
}

/// The export's row ceiling for a caller's request.
pub fn export_ceiling(requested: Option<usize>) -> usize {
    requested.unwrap_or(EXPORT_DEFAULT_ROWS).min(EXPORT_CEILING_ROWS)
}

/// A history window: an inclusive lower bound on the start instant and a row ceiling
/// (`run.record.history-window`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    #[serde(default)]
    pub since: Option<Instant>,
    pub ceiling: usize,
}

impl Window {
    /// Whether a row falls inside the window's bound.
    pub fn admits(&self, row: &RunRow) -> bool {
        self.since.is_none_or(|s| row.started_at >= s)
    }
}

/// Decode a window lower bound: `YYYY-MM-DD`, the start of that UTC day, or a UTC
/// RFC 3339 instant ending `Z` (`run.record.bound-spelling`).
pub fn parse_bound(s: &str) -> Result<Instant, RunError> {
    let refuse = || {
        RunError::HistoryBoundSpelling(format!("`{s}` is neither `YYYY-MM-DD` nor a UTC RFC 3339 instant ending `Z`"))
    };
    let b = s.as_bytes();
    let is_date = b.len() == 10
        && b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() });
    if is_date {
        return Instant::parse(&format!("{s}T00:00:00Z")).map_err(|_| refuse());
    }
    if !s.ends_with('Z') {
        return Err(refuse());
    }
    Instant::parse(s).map_err(|_| refuse())
}

/// One page of history, echoing the window it answers (`run.record.truncation-flag`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryPage {
    pub window: Window,
    pub runs: Vec<RunRow>,
    /// More runs of the pipeline fell inside the window than the ceiling returned.
    pub truncated: bool,
}

/// Select a pipeline's history: rows inside the window, newest first, clipped at the
/// ceiling. An empty window answers an empty page (`run.record.empty-window`).
pub fn select_history(rows: impl IntoIterator<Item = RunRow>, window: &Window) -> HistoryPage {
    let mut inside: Vec<RunRow> = rows.into_iter().filter(|r| window.admits(r)).collect();
    inside.sort_by(|a, b| b.started_at.cmp(&a.started_at).then_with(|| b.run_id.cmp(&a.run_id)));
    let truncated = inside.len() > window.ceiling;
    inside.truncate(window.ceiling);
    HistoryPage { window: window.clone(), runs: inside, truncated }
}

/// Size a recorded error is clipped to: 2 KiB (`run.record.error-cap`).
pub const ERROR_CAP_BYTES: usize = 2 * 1024;
/// Marks a clipped error.
pub const ERROR_ELLIPSIS: &str = "…";
/// Replaces a credential-shaped span.
pub const MASK: &str = "***";

/// Keys whose value is masked in `key=value` and `key: value` spellings.
const SECRET_KEYS: [&str; 8] = ["password", "passwd", "secret", "token", "api_key", "apikey", "authorization", "access_key"];
/// Prefixes of provider tokens masked wherever they appear.
const TOKEN_PREFIXES: [&str; 6] = ["sk-", "ghp_", "gho_", "github_pat_", "xoxb-", "AKIA"];

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '+' | '=')
}

/// Mask credential-shaped spans: `Bearer` credentials, the value after a secret-named
/// key, URL userinfo, provider-prefixed tokens and any unbroken run of 32 or more token
/// characters.
pub fn mask_credentials(text: &str) -> String {
    let words: Vec<&str> = text.split_inclusive(char::is_whitespace).collect();
    let mut out = String::with_capacity(text.len());
    let mut mask_next = false;
    for w in words {
        let trimmed = w.trim_end();
        let tail = &w[trimmed.len()..];
        let lower = trimmed.to_ascii_lowercase();
        if lower == "bearer" || lower == "basic" {
            mask_next = true;
            out.push_str(w);
            continue;
        }
        if mask_next && !trimmed.is_empty() {
            out.push_str(MASK);
            out.push_str(tail);
            mask_next = false;
            continue;
        }
        if SECRET_KEYS.iter().any(|k| lower.trim_end_matches(':') == *k) && trimmed.ends_with(':') {
            mask_next = true;
            out.push_str(w);
            continue;
        }
        out.push_str(&mask_word(trimmed));
        out.push_str(tail);
    }
    out
}

fn mask_word(word: &str) -> String {
    // `key=value`
    if let Some((k, _)) = word.split_once('=') {
        if SECRET_KEYS.iter().any(|s| k.to_ascii_lowercase().ends_with(s)) {
            return format!("{k}={MASK}");
        }
    }
    // `scheme://user:pass@host`
    if let Some(i) = word.find("://") {
        let rest = &word[i + 3..];
        if let Some(at) = rest.find('@') {
            if !rest[..at].contains('/') {
                return format!("{}{MASK}@{}", &word[..i + 3], &rest[at + 1..]);
            }
        }
    }
    let core = word.trim_matches(|c: char| !is_token_char(c));
    let masked = TOKEN_PREFIXES.iter().any(|p| core.starts_with(p) && core.len() > p.len() + 8)
        || (core.chars().count() >= 32 && core.chars().all(is_token_char) && core.chars().any(|c| c.is_ascii_digit()));
    if masked && !core.is_empty() {
        word.replacen(core, MASK, 1)
    } else {
        word.to_string()
    }
}

/// The one projection of a recorded error every surface serves: masked, then capped at
/// 2 KiB with an ellipsis marker on a char boundary.
pub fn cap_error(message: &str) -> String {
    let masked = mask_credentials(message);
    if masked.len() <= ERROR_CAP_BYTES {
        return masked;
    }
    let budget = ERROR_CAP_BYTES - ERROR_ELLIPSIS.len();
    let mut end = budget;
    while !masked.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{ERROR_ELLIPSIS}", &masked[..end])
}
