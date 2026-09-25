//! The acceptance harness: builds a workspace binary once and runs it the way a caller
//! does, against scratch state it owns.

pub mod http;
pub mod stdio;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;

static BUILT: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The workspace root, two levels above this package.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// Path to the named workspace binary, built on first request.
pub fn bin(name: &str) -> PathBuf {
    let root = workspace_root();
    let mut built = BUILT.lock().unwrap();
    if !built.iter().any(|b| b == name) {
        let status = Command::new(env!("CARGO"))
            .args(["build", "-q", "--bin", name])
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(status.success(), "building `{name}`");
        built.push(name.to_string());
    }
    let target = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| root.join("target"));
    target.join("debug").join(name)
}

/// A scratch git repository with deterministic identity and no signing.
pub struct GitRepo {
    _dir: tempfile::TempDir,
    pub root: PathBuf,
}

impl GitRepo {
    pub fn init() -> GitRepo {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let r = GitRepo { _dir: dir, root };
        r.git(&["init", "-q", "-b", "main"]);
        r
    }

    pub fn write(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    pub fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "commit.gpgsign=false", "-c", "user.name=acceptance", "-c", "user.email=acceptance@example.com"])
            .args(args)
            .current_dir(&self.root)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Commit everything and return the new head.
    pub fn commit(&self, message: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }

    /// Run a built binary with this repository as its working directory.
    pub fn run(&self, bin: &Path, args: &[&str]) -> Output {
        self.run_env(bin, args, &[])
    }

    /// Run a built binary with extra environment variables.
    pub fn run_env(&self, bin: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
        Command::new(bin).args(args).current_dir(&self.root).env_remove("CARGO_TARGET_DIR").envs(env.iter().copied()).output().unwrap()
    }

    /// Every file under the repository whose bytes contain `needle`, as paths relative to the root.
    pub fn files_containing(&self, needle: &[u8]) -> Vec<String> {
        let mut hits = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.file_name().is_some_and(|n| n == ".git") {
                    continue;
                }
                if path.is_dir() {
                    stack.push(path);
                } else if std::fs::read(&path).unwrap().windows(needle.len()).any(|w| w == needle) {
                    hits.push(path.strip_prefix(&self.root).unwrap().to_string_lossy().into_owned());
                }
            }
        }
        hits
    }
}
