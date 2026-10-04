//! The executable protocol model and Rust store agree after each generated step.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_contextful");

fn run(seed: &str) -> std::process::Output {
    Command::new(BIN)
        .args(["formal", "protocol-differential", "--seed", seed, "--cases", "12"])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .unwrap()
}

#[test]
fn fixed_seed_replays_protocol_cases_against_the_store() {
    if Command::new("lake").arg("--version").output().is_err() {
        assert!(std::env::var_os("CONTEXTFUL_REQUIRE_LEAN").is_none(), "Lean is required");
        return;
    }
    let first = run("76004");
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    let second = run("76004");
    assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
    assert_eq!(first.stdout, second.stdout);
    assert!(String::from_utf8_lossy(&first.stdout).contains("stale-fence-differential: 0"));
}
