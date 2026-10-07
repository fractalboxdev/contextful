//! A source whose pulls a command serves: one child process per pull, in a process
//! group on Unix or a private job on Windows, waited on under the run's cancellation token.
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
use std::time::{Duration, Instant};

/// Interval at which a running child is checked for exit and for a stop.
const WAIT_TICK: Duration = Duration::from_millis(10);
/// How long a signalled group has to exit on `SIGTERM` before it is killed.
#[cfg(not(windows))]
const TERM_GRACE: Duration = Duration::from_millis(500);
/// How long a killed group has to disappear before the pull fails instead of waiting on.
const KILL_CEILING: Duration = Duration::from_secs(5);
/// How long the output pipes have to reach end-of-file once the group is gone.
const PIPE_CEILING: Duration = Duration::from_secs(5);

pub struct CommandSource {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
}

#[cfg(windows)]
mod admission;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
type ProcessTree = windows::Job;
#[cfg(not(windows))]
type ProcessTree = u32;

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

    /// Whether any process of group `pgid` still runs. A zombie does not: once its
    /// parent exits it waits on PID 1, which under a container runtime may never reap it.
    #[cfg(target_os = "linux")]
    pub fn alive(pgid: u32) -> bool {
        let Ok(entries) = std::fs::read_dir("/proc") else { return exists(pgid) };
        entries.flatten().any(|e| {
            let Ok(stat) = std::fs::read_to_string(e.path().join("stat")) else { return false };
            // `pid (comm) state ppid pgrp …`; `comm` may hold spaces and parentheses.
            let Some((_, rest)) = stat.rsplit_once(')') else { return false };
            let mut fields = rest.split_whitespace();
            let state = fields.next().unwrap_or("Z");
            let pgrp = fields.nth(1).and_then(|f| f.parse::<u32>().ok());
            pgrp == Some(pgid) && !matches!(state, "Z" | "X" | "x")
        })
    }

    /// Whether any process of group `pgid` still runs; the host's init reaps orphans.
    #[cfg(not(target_os = "linux"))]
    pub fn alive(pgid: u32) -> bool {
        exists(pgid)
    }

    fn exists(pgid: u32) -> bool {
        match libc::pid_t::try_from(pgid) {
            // SAFETY: signal 0 checks existence and delivers nothing.
            Ok(pgid) => unsafe { libc::kill(-pgid, 0) == 0 },
            Err(_) => false,
        }
    }
}

/// Where no process groups exist, the child alone is signalled, through its handle.
#[cfg(not(any(unix, windows)))]
mod group {
    pub const TERM: i32 = 15;
    pub const KILL: i32 = 9;

    pub fn signal(_pgid: u32, _signal: i32) {}

    pub fn alive(_pgid: u32) -> bool {
        false
    }
}

/// Signal the group to terminate, escalate to a kill past the grace period, and return
/// once the child is reaped and no process of the group runs. A group still running
/// `KILL_CEILING` after the kill fails the pull.
#[cfg(not(windows))]
fn reap_group(child: &mut std::process::Child, tree: &ProcessTree, step: &str) -> Result<(), Failure> {
    let pgid = *tree;
    group::signal(pgid, group::TERM);
    let started = Instant::now();
    let mut reaped = false;
    loop {
        if !reaped {
            reaped = !matches!(child.try_wait(), Ok(None));
        }
        if reaped && !group::alive(pgid) {
            return Ok(());
        }
        if started.elapsed() >= TERM_GRACE {
            group::signal(pgid, group::KILL);
            if !reaped {
                let _ = child.kill();
            }
        }
        if started.elapsed() >= TERM_GRACE + KILL_CEILING {
            return Err(Failure::new(
                FailureTag::Transient,
                format!("process group {pgid} of `{step}` still runs {} s after SIGKILL", KILL_CEILING.as_secs()),
            ));
        }
        std::thread::sleep(WAIT_TICK);
    }
}

#[cfg(windows)]
fn reap_group(child: &mut std::process::Child, job: &ProcessTree, step: &str) -> Result<(), Failure> {
    job.reap(child).map_err(|error| Failure::new(FailureTag::Transient, format!("reaping the job of `{step}`: {error}")))
}

/// Read a pipe on a thread of its own, delivering each chunk over a channel; the channel
/// disconnects at end-of-file.
fn drain(pipe: Option<impl Read + Send + 'static>) -> Receiver<Vec<u8>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let Some(mut p) = pipe else { return };
        let mut buf = [0u8; 8192];
        while let Ok(n) = p.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    rx
}

/// Gather a drained pipe to end-of-file within `PIPE_CEILING`, observing the stop token. A
/// pipe still open past the ceiling is held by a process outside the reaped group, and
/// fails the pull rather than blocking it.
fn collect(rx: &Receiver<Vec<u8>>, cancel: &dyn Cancellation, step: &str, stream: &str) -> Result<Vec<u8>, Failure> {
    let deadline = Instant::now() + PIPE_CEILING;
    let mut bytes = Vec::new();
    loop {
        match rx.recv_timeout(WAIT_TICK) {
            Ok(chunk) => bytes.extend_from_slice(&chunk),
            Err(RecvTimeoutError::Disconnected) => return Ok(bytes),
            Err(RecvTimeoutError::Timeout) if cancel.requested() => {
                return Err(Failure::canceled(format!("stopped while reading the output of `{step}`")));
            }
            Err(RecvTimeoutError::Timeout) if Instant::now() >= deadline => {
                return Err(Failure::new(
                    FailureTag::Transient,
                    format!("the {stream} of `{step}` stays open {} s after its process group is gone; a process outside the group holds it", PIPE_CEILING.as_secs()),
                ));
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
        #[cfg(windows)]
        let (mut child, tree) = windows::Job::launch(&mut cmd).map_err(|e| Failure::new(FailureTag::Config, format!("spawning `{program}` in its job: {e}")))?;
        #[cfg(not(windows))]
        let mut child = cmd.spawn().map_err(|e| Failure::new(FailureTag::Config, format!("spawning `{program}`: {e}")))?;
        #[cfg(not(windows))]
        let tree = child.id();
        let out = drain(child.stdout.take());
        let err = drain(child.stderr.take());
        let status = loop {
            if cancel.requested() {
                reap_group(&mut child, &tree, &request.step_label)?;
                return Err(Failure::canceled(format!("stopped during `{}`; its process group is reaped", request.step_label)));
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => std::thread::sleep(WAIT_TICK),
                Err(e) => {
                    #[cfg(windows)]
                    reap_group(&mut child, &tree, &request.step_label)?;
                    return Err(Failure::new(FailureTag::Transient, format!("waiting on `{program}`: {e}")));
                }
            }
        };
        // A process the child left behind in its group is signalled and reaped too, which
        // also closes any pipe it held open.
        reap_group(&mut child, &tree, &request.step_label)?;
        let stdout = collect(&out, cancel, &request.step_label, "stdout")?;
        let stderr = collect(&err, cancel, &request.step_label, "stderr")?;
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
