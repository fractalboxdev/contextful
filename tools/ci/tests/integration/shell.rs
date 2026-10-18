//! `assurance.automate.unchecked-shell`: the toolchain stage runs `shellcheck` over every
//! tracked shell file. A stand-in `shellcheck` first on `PATH` rejects a file holding
//! `$unquoted`, so the suite needs no network and no installed linter.

use crate::{stderr, Repo};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

const STAND_IN: &str = r#"#!/bin/sh
if [ "$1" = --version ]; then echo stand-in; exit 0; fi
shift 2
printf '%s\n' "$@" > "$SHELLCHECK_SEEN"
status=0
for f in "$@"; do
  if grep -q 'unquoted' "$f"; then
    echo "$f:2:6: note: Double quote to prevent globbing and word splitting. [SC2086]"
    status=1
  fi
done
exit "$status"
"#;

struct Linter {
    _dir: tempfile::TempDir,
    bin: PathBuf,
    seen: PathBuf,
}

impl Linter {
    fn new() -> Linter {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("shellcheck");
        std::fs::write(&exe, STAND_IN).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let seen = dir.path().join("seen");
        Linter { _dir: dir, bin, seen }
    }

    fn run(&self, r: &Repo, args: &[&str]) -> Output {
        let path = format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap_or_default());
        Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
            .args(args)
            .current_dir(&r.root)
            .env_remove("CARGO_TARGET_DIR")
            .env("PATH", path)
            .env("SHELLCHECK_SEEN", &self.seen)
            .output()
            .unwrap()
    }

    fn seen(&self) -> Vec<String> {
        std::fs::read_to_string(&self.seen).unwrap_or_default().lines().map(str::to_string).collect()
    }
}

/// A repository tracking a named shell file, an extensionless `sh` script, and a `zsh`
/// script `shellcheck` does not parse.
fn scripts(bad: bool) -> Repo {
    let r = Repo::init();
    let body = if bad { "echo $unquoted" } else { "echo \"$quoted\"" };
    r.write("scripts/build.sh", &format!("#!/bin/sh\n{body}\n"));
    r.write("bin/release", "#!/usr/bin/env bash\necho \"$1\"\n");
    r.write("bin/prompt", "#!/bin/zsh\necho $unquoted\n");
    r.write("README.md", "#!/bin/sh is not a shebang here\n");
    r.commit("scripts");
    r
}

// spec: assurance.automate.unchecked-shell@ae0c6b0b
#[test]
fn a_shell_file_shellcheck_rejects_fails_the_toolchain_stage() {
    let r = scripts(true);
    r.write("target/gate/pins.json", "{}\n");
    let linter = Linter::new();
    let o = linter.run(&r, &["gate", "--stage", "toolchain"]);
    assert!(!o.status.success(), "{}", stderr(&o));
    let err = stderr(&o);
    assert!(err.contains("ShellCheckFailed: scripts/build.sh:2:6"), "{err}");
    assert!(!err.contains("toolchain: recorded"), "the stage provisioned past a rejected shell file: {err}");
}

#[test]
fn every_tracked_shell_file_and_only_those_reach_shellcheck() {
    let r = scripts(false);
    let linter = Linter::new();
    let o = linter.run(&r, &["shellcheck"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stderr(&o).contains("2 shell file(s) pass"), "{}", stderr(&o));
    let mut seen = linter.seen();
    seen.sort();
    assert_eq!(seen, ["bin/release", "scripts/build.sh"]);
}

fn executable(path: &std::path::Path, script: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\n{script}")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A host whose `shellcheck` fails to run, as an image without the linter does, while the
/// toolchain stage's pinned copy sits in the gate directory: the test process the stage
/// starts resolves the pinned copy first on `PATH`.
// spec: assurance.automate.test-shellcheck@f86ba617
#[test]
fn a_test_process_the_gate_starts_resolves_the_provisioned_shellcheck() {
    let r = Repo::init();
    r.write("scripts/build.sh", "#!/bin/sh\necho \"$1\"\n");
    r.write("crates/demo/Cargo.toml", &crate::manifest("demo", ""));
    r.write("crates/demo/tests/integration/main.rs", "#[test]\nfn lints() {}\n");
    let meta = serde_json::json!({"packages": [{"name": "demo", "manifest_path": r.root.join("crates/demo/Cargo.toml"), "features": {}}]});
    r.write("metadata.json", &meta.to_string());
    r.commit("a package and a shell file");
    executable(&r.root.join("target/gate/shellcheck-v0.10.0/shellcheck"), "echo provisioned\n");
    let tools = tempfile::tempdir().unwrap();
    let bin = tools.path().join("bin");
    let wasm = tools.path().join("wasm-lib");
    std::fs::create_dir_all(&wasm).unwrap();
    std::fs::write(wasm.join("libstd.rlib"), "").unwrap();
    executable(&bin.join("shellcheck"), "exit 127\n");
    executable(&bin.join("rustc"), "printf '%s\\n' \"$WASM_LIB\"\n");
    executable(&bin.join("cargo"), r#"
if test "$1" = metadata; then cat metadata.json; exit 0; fi
test "$1" = test
test "$(shellcheck --version)" = provisioned || { echo 'the test process resolved the host shellcheck' >&2; exit 94; }
printf '%s\n' "$*" >> child-commands
"#);
    let host_path = std::env::var("PATH").unwrap();
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&host_path))).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .args(["gate", "--stage", "workspace.foundation"])
        .current_dir(&r.root)
        .env("PATH", path)
        .env("WASM_LIB", &wasm)
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    let commands = std::fs::read_to_string(r.root.join("child-commands")).unwrap_or_default();
    assert!(commands.contains("test --package demo"), "the stage started no test process: {commands}");
}

#[test]
fn a_tree_tracking_no_shell_file_needs_no_shellcheck() {
    let r = Repo::init();
    let o = r.run_ci(&["shellcheck"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stderr(&o).contains("no tracked shell file"), "{}", stderr(&o));
}
