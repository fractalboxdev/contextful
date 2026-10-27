//! `corpus.registry.prose-report`: `contextful-ci prose` prints Vale's alerts over the
//! guides and `references/`, and exits 0 whatever Vale finds or whether it is installed.

use crate::Repo;
use std::process::{Command, Output};

/// Run `contextful-ci prose` with `PATH` set to `path`.
fn prose(r: &Repo, path: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful-ci")).arg("prose").current_dir(&r.root).env("PATH", path).output().unwrap()
}

/// A directory holding a `vale` script that reports `version`, echoes its arguments and
/// one alert, and exits 1.
fn fake_vale(version: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("vale");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo \"vale version {version}\"; exit 0; fi\necho \"args: $*\"\necho \"spec/guide/store.md:3:7:WriteTechnically.Terms:Use 'commit fence' instead of 'fencing token'.\"\nexit 1\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

// spec: corpus.registry.prose-report@a2321bbe
#[test]
fn the_prose_report_prints_each_alert_and_exits_zero() {
    let r = Repo::init();
    let vale = fake_vale("3.25.1");
    let o = prose(&r, &format!("{}:/usr/bin:/bin", vale.path().display()));
    let out = stdout(&o);
    assert!(o.status.success(), "{out}");
    assert!(out.contains("args: --config .vale/vale.ini --output line --no-exit spec/guide references"), "{out}");
    assert!(out.contains("spec/guide/store.md:3:7:WriteTechnically.Terms:"), "{out}");
    assert!(out.contains("1 alert(s)"), "{out}");
    assert!(!out.contains("differs from the pinned"), "{out}");

    let older = fake_vale("3.0.0");
    let o = prose(&r, &format!("{}:/usr/bin:/bin", older.path().display()));
    assert!(o.status.success());
    assert!(stdout(&o).contains("`vale version 3.0.0` differs from the pinned Vale 3.25.1"), "{}", stdout(&o));

    let o = prose(&r, "/usr/bin:/bin");
    assert!(o.status.success());
    assert!(stdout(&o).contains("vale is absent from PATH"), "{}", stdout(&o));
}
