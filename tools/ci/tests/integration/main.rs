//! The gate's one integration binary. Suites drive the built `contextful-ci` against
//! scratch git repositories holding a small cargo workspace.

mod acceptance_surface;
mod allowlist;
mod artifact;
mod cli_parts;
mod dependency_versions;
mod deny;
mod disk;
mod domain;
mod e2e;
mod features;
mod image;
mod lean;
mod measure;
mod mirrors;
mod mandatory_toolchains;
mod probe;
mod release;
mod secrets;
mod source_lint;
mod source_binding;
mod target_dirs;
mod stages;
mod test_first;
mod topology;
mod wasm;
mod workflow;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub struct Repo {
    _dir: tempfile::TempDir,
    pub root: PathBuf,
}

impl Repo {
    /// A repository whose first commit holds `crates/demo` with `double` and its test.
    pub fn init() -> Repo {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let r = Repo { _dir: dir, root };
        r.git(&["init", "-q", "-b", "main"]);
        r.git(&["config", "--local", "user.name", "t"]);
        r.git(&["config", "--local", "user.email", "t@example.com"]);
        r.git(&["config", "--local", "commit.gpgsign", "false"]);
        r.write("Cargo.toml", "[workspace]\nresolver = \"2\"\nmembers = [\"crates/*\"]\n");
        r.write("crates/demo/Cargo.toml", &manifest("demo", ""));
        r.write("crates/demo/src/lib.rs", "pub fn double(x: i32) -> i32 {\n    x * 2\n}\n");
        r.write(
            "crates/demo/tests/integration/main.rs",
            "mod double;\n",
        );
        r.write(
            "crates/demo/tests/integration/double.rs",
            "#[test]\nfn doubles() {\n    assert_eq!(demo::double(2), 4);\n}\n",
        );
        r.write(".gitignore", "/target\n");
        r.commit("base");
        r
    }

    pub fn write(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    pub fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    pub fn commit(&self, message: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }

    /// Write the `Cargo.lock` the manifests resolve to, as a workspace commits it: the
    /// crate-graph stage resolves `--locked`.
    pub fn lock(&self) {
        let o = Command::new("cargo")
            .args(["metadata", "--format-version", "1", "-q"])
            .current_dir(&self.root)
            .env_remove("CARGO_TARGET_DIR")
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }

    pub fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"])
    }

    pub fn gate(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
            .arg("gate")
            .args(args)
            .current_dir(&self.root)
            .env_remove("CARGO_TARGET_DIR")
            .output()
            .unwrap()
    }

    /// `contextful-ci` run with `args` at the repository root.
    pub fn run_ci(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_contextful-ci")).args(args).current_dir(&self.root).env_remove("CARGO_TARGET_DIR").output().unwrap()
    }

    /// The gate with `cargo` answered by `script`, a POSIX shell script placed first on PATH.
    pub fn gate_with_cargo(&self, script: &str, args: &[&str]) -> Output {
        let bin = tempfile::tempdir().unwrap();
        let cargo = bin.path().join("cargo");
        std::fs::write(&cargo, format!("#!/bin/sh\n{script}")).unwrap();
        std::fs::set_permissions(&cargo, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let path = format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap_or_default());
        Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
            .arg("gate")
            .args(args)
            .current_dir(&self.root)
            .env_remove("CARGO_TARGET_DIR")
            .env("PATH", path)
            .output()
            .unwrap()
    }
}

pub fn manifest(name: &str, deps: &str) -> String {
    format!(
        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\nlicense = \"Apache-2.0\"\n\n[dependencies]\n{deps}\n[[test]]\nname = \"integration\"\npath = \"tests/integration/main.rs\"\n"
    )
}

pub fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

pub fn repo_root() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}
