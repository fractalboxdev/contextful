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
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Interval at which a running child is checked for exit and for a stop.
const WAIT_TICK: Duration = Duration::from_millis(10);

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

fn signal_group(pgid: u32, signal: libc::c_int) {
    if let Ok(pgid) = libc::pid_t::try_from(pgid) {
        // SAFETY: `kill` takes plain integers; a negative pid addresses the process group.
        unsafe {
            libc::kill(-pgid, signal);
        }
    }
}

/// Whether any process of group `pgid` still exists.
fn group_alive(pgid: u32) -> bool {
    match libc::pid_t::try_from(pgid) {
        // SAFETY: signal 0 checks existence and delivers nothing.
        Ok(pgid) => unsafe { libc::kill(-pgid, 0) == 0 },
        Err(_) => false,
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
        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();
        let out = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(s) = stdout.as_mut() {
                let _ = s.read_to_end(&mut buf);
            }
            buf
        });
        let err = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(s) = stderr.as_mut() {
                let _ = s.read_to_end(&mut buf);
            }
            buf
        });
        let status = loop {
            if cancel.requested() {
                // A stop signals the whole chain and returns only once the group is reaped.
                signal_group(pgid, libc::SIGTERM);
                let _ = child.wait();
                while group_alive(pgid) {
                    std::thread::sleep(WAIT_TICK);
                    signal_group(pgid, libc::SIGKILL);
                }
                return Err(Failure::canceled(format!("stopped during `{}`; its process group is reaped", request.step_label)));
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => std::thread::sleep(WAIT_TICK),
                Err(e) => return Err(Failure::new(FailureTag::Transient, format!("waiting on `{program}`: {e}"))),
            }
        };
        let stdout = out.join().unwrap_or_default();
        let stderr = err.join().unwrap_or_default();
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
