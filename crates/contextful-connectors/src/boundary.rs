//! The decode process boundary (`run.land.parse-boundary`): a decode that can die runs in a
//! child process whose wall clock and data segment are bounded and which the parent kills
//! at its deadline. A clean refusal crosses back on standard error with exit status
//! [`REFUSED`]; any other non-zero exit or a fatal signal is a crash of the decode, never of
//! the serving process (`run.land.parse-crashed`).

use contextful_core::run::{Failure, FailureTag, RunError};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The exit status a worker reports a typed refusal with, its message on standard error.
pub const REFUSED: i32 = 2;
/// Wall clock one decode may take.
pub const DEADLINE: Duration = Duration::from_secs(60);
/// Data segment one decode process may map, where the operating system enforces it.
pub const MEMORY_BYTES: u64 = 512 * 1024 * 1024;

/// Decodes a PDF body into its pages' text.
pub trait PageDecoder: Send + Sync {
    fn pages(&self, body: &[u8], input: &str) -> Result<Vec<String>, Failure>;
}

/// The decode process boundary answers a PDF as a JSON array of page texts.
impl PageDecoder for Boundary {
    fn pages(&self, body: &[u8], input: &str) -> Result<Vec<String>, Failure> {
        let out = self.run(body, input)?;
        serde_json::from_slice(&out).map_err(|e| {
            Failure::deterministic(FailureTag::Permanent, RunError::PipelineParseCrashed(format!("decoding `{input}`: the decode process answered no page list: {e}")).to_string())
        })
    }
}

/// A decode command: a program, its arguments, and the bounds its process runs under.
#[derive(Debug, Clone)]
pub struct Boundary {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub deadline: Duration,
    pub memory_bytes: u64,
}

impl Boundary {
    pub fn new(program: impl Into<PathBuf>, args: &[&str]) -> Boundary {
        Boundary { program: program.into(), args: args.iter().map(|a| a.to_string()).collect(), deadline: DEADLINE, memory_bytes: MEMORY_BYTES }
    }

    pub fn with_deadline(mut self, deadline: Duration) -> Boundary {
        self.deadline = deadline;
        self
    }

    pub fn with_memory(mut self, bytes: u64) -> Boundary {
        self.memory_bytes = bytes;
        self
    }

    /// Run the decode over `input`, naming it `label` in any refusal; answer its standard output.
    pub fn run(&self, input: &[u8], label: &str) -> Result<Vec<u8>, Failure> {
        let crashed = |why: String| Failure::deterministic(FailureTag::Permanent, RunError::PipelineParseCrashed(format!("decoding `{label}`: {why}")).to_string());
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args).arg("--input").arg(label).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        limit_memory(&mut cmd, self.memory_bytes);
        own_group(&mut cmd);
        let started = Instant::now();
        let mut child = cmd.spawn().map_err(|e| crashed(format!("the decode process did not start: {e}")))?;
        let mut stdin = child.stdin.take().ok_or_else(|| crashed("no standard input".into()))?;
        let body = input.to_vec();
        // A worker dying before it reads everything closes the pipe; its exit status says why.
        let feed = std::thread::spawn(move || {
            let _ = stdin.write_all(&body);
        });
        let out = drain(child.stdout.take());
        let err = drain(child.stderr.take());
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|e| crashed(e.to_string()))? {
                break status;
            }
            if started.elapsed() >= self.deadline {
                kill_group(&mut child);
                let _ = child.wait();
                let _ = (feed.join(), out.join(), err.join());
                return Err(crashed(format!("the decode ran past its {} s wall clock and was killed", self.deadline.as_secs())));
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let _ = feed.join();
        let (stdout, stderr) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
        if status.success() {
            return Ok(stdout);
        }
        let said = String::from_utf8_lossy(&stderr).trim().to_string();
        if status.code() == Some(REFUSED) && refusal(&said) {
            return Err(Failure::deterministic(FailureTag::Permanent, said));
        }
        Err(crashed(match status.code() {
            Some(code) => format!("the decode process exited {code}"),
            None => format!("the decode process ended on a signal ({status})"),
        }))
    }
}

/// Whether a worker's message is one of the refusals a decode raises.
fn refusal(message: &str) -> bool {
    ["PipelineUnreadableInput: ", "PipelinePartialParse: "].iter().any(|p| message.starts_with(p))
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut out = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut out);
        }
        out
    })
}

#[cfg(target_os = "linux")]
fn limit_memory(cmd: &mut Command, bytes: u64) {
    use std::os::unix::process::CommandExt;
    // SAFETY: `setrlimit` is async-signal-safe and touches only the child's own limits.
    unsafe {
        cmd.pre_exec(move || {
            let lim = libc::rlimit { rlim_cur: bytes as libc::rlim_t, rlim_max: bytes as libc::rlim_t };
            if libc::setrlimit(libc::RLIMIT_DATA, &lim) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn limit_memory(_cmd: &mut Command, _bytes: u64) {}

/// Start the decode as the leader of a process group of its own, so the deadline reaches
/// every process it starts.
#[cfg(unix)]
fn own_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(not(unix))]
fn own_group(_cmd: &mut Command) {}

/// Kill the decode's whole process group: a process it started holding the output pipe
/// open would otherwise keep the parent reading past the deadline.
#[cfg(unix)]
fn kill_group(child: &mut std::process::Child) {
    match libc::pid_t::try_from(child.id()) {
        // SAFETY: `kill` takes no pointers; the negated pid names the group `own_group` made.
        Ok(pid) => unsafe {
            libc::kill(-pid, libc::SIGKILL);
        },
        Err(_) => {
            let _ = child.kill();
        }
    }
}

#[cfg(not(unix))]
fn kill_group(child: &mut std::process::Child) {
    let _ = child.kill();
}

/// The worker side: decode standard input as `kind`, print the result as JSON on standard
/// output and exit 0, or print the refusal on standard error and exit [`REFUSED`].
#[cfg(feature = "pdf")]
pub fn worker(kind: &str, label: &str) -> i32 {
    let mut input = Vec::new();
    if let Err(e) = std::io::stdin().read_to_end(&mut input) {
        eprintln!("reading the input: {e}");
        return 1;
    }
    let decoded = match kind {
        "pdf" => crate::decode::pdf::pages(&input, label),
        other => {
            eprintln!("no decoder is named `{other}`");
            return 1;
        }
    };
    match decoded {
        Ok(pages) => match serde_json::to_writer(std::io::stdout().lock(), &pages) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("writing the pages: {e}");
                1
            }
        },
        Err(f) => {
            eprintln!("{}", f.message);
            REFUSED
        }
    }
}
