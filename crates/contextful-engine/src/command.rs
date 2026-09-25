//! A source whose pulls a command serves: one child process per pull, in a process
//! group of its own, waited on under the run's cancellation token.
//!
//! The child reads its request from the environment — `CONTEXTFUL_CURSOR` holds the
//! position as JSON, empty from the start; `CONTEXTFUL_IDEMPOTENCY_KEY` and
//! `CONTEXTFUL_STEP` name the pull — and writes one pull to stdout. A non-zero exit
//! reports `{"error": {"tag", "message", "retry_after_secs"}}` on stdout; an exit with
//! no tagged error reads as `Permanent`.

use contextful_core::run::ports::{Cancellation, PullRequest, Source};
use contextful_core::run::{Failure, FailureTag};
use serde::Deserialize;
use std::io::Read;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Interval at which a running child is checked for exit and for a stop.
const WAIT_TICK: Duration = Duration::from_millis(10);
/// How long a signalled group has to exit on `SIGTERM` before it is killed.
const TERM_GRACE: Duration = Duration::from_millis(500);

pub struct CommandSource {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
}

#[derive(Deserialize)]
struct Reported {
    error: ReportedError,
}

#[derive(Deserialize)]
struct ReportedError {
    tag: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    retry_after_secs: Option<u64>,
}

#[cfg(unix)]
mod group {
    pub const TERM: libc::c_int = libc::SIGTERM;
    pub const KILL: libc::c_int = libc::SIGKILL;

    pub fn signal(pgid: u32, signal: libc::c_int) {
        if let Ok(pgid) = libc::pid_t::try_from(pgid) {
            // SAFETY: `kill` takes plain integers; a negative pid addresses the process group.
            unsafe {
                libc::kill(-pgid, signal);
            }
        }
    }

    /// Whether any process of group `pgid` still exists.
    pub fn alive(pgid: u32) -> bool {
        match libc::pid_t::try_from(pgid) {
            // SAFETY: signal 0 checks existence and delivers nothing.
            Ok(pgid) => unsafe { libc::kill(-pgid, 0) == 0 },
            Err(_) => false,
        }
    }
}

/// Where no process groups exist, the child alone is signalled, through its handle.
#[cfg(not(unix))]
mod group {
    pub const TERM: i32 = 15;
    pub const KILL: i32 = 9;

    pub fn signal(_pgid: u32, _signal: i32) {}

    pub fn alive(_pgid: u32) -> bool {
        false
    }
}

/// Signal the group to terminate, escalate to a kill past the grace period, and return
/// once the child is reaped and no process of the group remains.
fn reap_group(child: &mut std::process::Child, pgid: u32) {
    group::signal(pgid, group::TERM);
    let started = std::time::Instant::now();
    let mut reaped = false;
    loop {
        if !reaped {
            reaped = !matches!(child.try_wait(), Ok(None));
        }
        if reaped && !group::alive(pgid) {
            return;
        }
        if started.elapsed() >= TERM_GRACE {
            group::signal(pgid, group::KILL);
            if !reaped {
                let _ = child.kill();
            }
        }
        std::thread::sleep(WAIT_TICK);
    }
}

/// Read a pipe to its end on a thread of its own, delivering the bytes over a channel.
fn drain(pipe: Option<impl Read + Send + 'static>) -> Receiver<Vec<u8>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut buf);
        }
        let _ = tx.send(buf);
    });
    rx
}

/// Wait for a drained pipe, observing the stop token.
fn collect(rx: &Receiver<Vec<u8>>, cancel: &dyn Cancellation, step: &str) -> Result<Vec<u8>, Failure> {
    loop {
        match rx.recv_timeout(WAIT_TICK) {
            Ok(bytes) => return Ok(bytes),
            Err(RecvTimeoutError::Disconnected) => return Ok(Vec::new()),
            Err(RecvTimeoutError::Timeout) if cancel.requested() => {
                return Err(Failure::canceled(format!("stopped while reading the output of `{step}`")));
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

impl Source for CommandSource {
    fn pull(&mut self, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        let (program, args) = self.argv.split_first().ok_or_else(|| Failure::deterministic(FailureTag::Config, "the connector names no command"))?;
        let cursor = request.position.as_ref().map(|p| p.to_string()).unwrap_or_default();
        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(&self.cwd)
            .env("CONTEXTFUL_CURSOR", cursor)
            .env("CONTEXTFUL_IDEMPOTENCY_KEY", &request.idempotency_key)
            .env("CONTEXTFUL_STEP", &request.step_label)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        let mut child = cmd.spawn().map_err(|e| Failure::new(FailureTag::Config, format!("spawning `{program}`: {e}")))?;
        let pgid = child.id();
        let out = drain(child.stdout.take());
        let err = drain(child.stderr.take());
        let status = loop {
            if cancel.requested() {
                reap_group(&mut child, pgid);
                return Err(Failure::canceled(format!("stopped during `{}`; its process group is reaped", request.step_label)));
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => std::thread::sleep(WAIT_TICK),
                Err(e) => return Err(Failure::new(FailureTag::Transient, format!("waiting on `{program}`: {e}"))),
            }
        };
        // A process the child left behind in its group is signalled and reaped too, which
        // also closes any pipe it held open.
        reap_group(&mut child, pgid);
        let stdout = collect(&out, cancel, &request.step_label)?;
        let stderr = collect(&err, cancel, &request.step_label)?;
        if status.success() {
            return Ok(stdout);
        }
        match serde_json::from_slice::<Reported>(&stdout) {
            Ok(r) => {
                let tag = FailureTag::parse(&r.error.tag).unwrap_or(FailureTag::Permanent);
                let mut f = Failure::new(tag, r.error.message);
                f.retry_after_secs = r.error.retry_after_secs;
                Err(f)
            }
            Err(_) => Err(Failure::new(
                FailureTag::Permanent,
                format!("`{program}` exited {status}: {}", String::from_utf8_lossy(&stderr).trim()),
            )),
        }
    }
}
