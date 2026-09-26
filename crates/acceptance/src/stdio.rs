//! A child process spoken to one line at a time over its standard input and output, as
//! a tool-protocol client speaks to a stdio server.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};

pub struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Session {
    /// Spawn `bin` with `args` in `dir`, adding `env`; standard error passes through.
    pub fn spawn(bin: &Path, args: &[&str], dir: &Path, env: &[(&str, &str)]) -> Session {
        let mut child = Command::new(bin)
            .args(args)
            .current_dir(dir)
            .env_remove("CARGO_TARGET_DIR")
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Session { child, stdin, stdout }
    }

    /// Write one line and read the one line answering it.
    pub fn exchange(&mut self, line: &str) -> String {
        self.send(line);
        self.read_line()
    }

    /// Write one line that expects no answer.
    pub fn send(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("stdin is open");
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    /// Read one line; an empty string at end of output.
    pub fn read_line(&mut self) -> String {
        let mut out = String::new();
        self.stdout.read_line(&mut out).unwrap();
        out.trim_end().to_string()
    }

    /// Close standard input and wait for the process to exit.
    pub fn close(mut self) -> ExitStatus {
        drop(self.stdin.take());
        self.child.wait().unwrap()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.stdin.is_some() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
