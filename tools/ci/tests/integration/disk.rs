//! `assurance.gate.free-disk`: every stage reads the free disk under the workspace before
//! doing work.

use crate::{stderr, Repo};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output};

/// A `df` reporting `available_kib` free under any path, in POSIX `-P` form.
fn fake_df(dir: &std::path::Path, available_kib: u64) {
    let script = format!(
        "#!/bin/sh\necho 'Filesystem 1024-blocks Used Available Capacity Mounted on'\necho \"/dev/fake 20971520 1 {available_kib} 99% /\"\n"
    );
    let p = dir.join("df");
    std::fs::write(&p, script).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn gate_with_df(r: &Repo, bin: &std::path::Path, stage: &str) -> Output {
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .args(["gate", "--stage", stage])
        .current_dir(&r.root)
        .env_remove("CARGO_TARGET_DIR")
        .env("PATH", path)
        .output()
        .unwrap()
}

/// A stage starting with less than 2 GiB of free disk raises `BuildDiskPrecondition` and exits 28 before doing work.
// spec: assurance.gate.free-disk@aa5e2fd4
#[test]
fn a_stage_starting_under_two_gib_free_refuses_with_exit_28_before_work() {
    let r = Repo::init();
    r.write("evals/ledger.toml", "");
    r.commit("an empty ledger");
    let bin = tempfile::tempdir().unwrap();

    // One KiB under 2 GiB.
    fake_df(bin.path(), 2 * 1024 * 1024 - 1);
    for stage in ["evaluate", "workspace"] {
        let o = gate_with_df(&r, bin.path(), stage);
        let err = stderr(&o);
        assert_eq!(o.status.code(), Some(28), "{stage}: {err}");
        assert!(err.contains(&format!("BuildDiskPrecondition: stage `{stage}` starts with 2047 MiB free")), "{err}");
        assert!(!r.root.join("target").exists(), "{stage} built nothing: {err}");
    }

    // Exactly 2 GiB starts the stage.
    fake_df(bin.path(), 2 * 1024 * 1024);
    let o = gate_with_df(&r, bin.path(), "evaluate");
    assert!(o.status.success(), "{}", stderr(&o));
}
