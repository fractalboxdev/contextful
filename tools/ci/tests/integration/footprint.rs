//! `assurance.gate.footprint`: `contextful-ci footprint --build --plan` builds the one
//! x86_64 static-linked Linux target on every host, natively on an x86_64 Linux host and
//! in the amd64 builder elsewhere.

use crate::repo_root;
use std::process::Command;

/// The planned build command of the edge profile on `host`, as `<os>-<arch>`.
fn plan(host: &str) -> String {
    let o = Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .args(["footprint", "--build", "--plan", "--profile", "contextful-edge", "--host", host])
        .current_dir(repo_root())
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap()
}

/// The value following `flag` in `command`.
fn after<'a>(command: &'a str, flag: &str) -> Option<&'a str> {
    let words: Vec<&str> = command.split_whitespace().collect();
    words.windows(2).find(|w| w[0] == flag).map(|w| w[1])
}

#[test]
fn every_other_host_builds_the_x86_64_musl_target_on_the_amd64_builder() {
    for host in ["macos-aarch64", "macos-x86_64", "linux-aarch64", "windows-x86_64"] {
        let command = plan(host);
        assert!(command.contains(" docker run "), "{host}: {command}");
        assert_eq!(after(&command, "--platform"), Some("linux/amd64"), "{host}: {command}");
        assert_eq!(after(&command, "--target"), Some("x86_64-unknown-linux-musl"), "{host}: {command}");
    }
}

#[test]
fn an_x86_64_linux_host_builds_the_static_glibc_target_natively() {
    let command = plan("linux-x86_64");
    assert!(!command.contains("docker"), "{command}");
    assert!(command.contains("+crt-static"), "{command}");
    assert_eq!(after(&command, "--target"), Some("x86_64-unknown-linux-gnu"), "{command}");
}
