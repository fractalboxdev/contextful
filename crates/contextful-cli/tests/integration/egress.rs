//! The process refuses an admitted private range reaching loopback, link-local or the
//! metadata address before running any command.

use std::process::Command;

fn storage_check(ranges: &str) -> std::process::Output {
    let dir = tempfile::tempdir().unwrap();
    Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["storage", "check"])
        .current_dir(dir.path())
        .env_remove("CONTEXTFUL_STORAGE_PROFILE")
        .env("CONTEXTFUL_EGRESS_PRIVATE_RANGES", ranges)
        .output()
        .unwrap()
}

/// A listed range overlapping loopback, link-local `169.254.0.0/16` or `fe80::/10`, or the metadata address
/// `fd00:ec2::254`, raises `ConnectorPrivateRangeRejected` at startup, naming the range.
// spec: connector.attach.private-range-rejected@28dacd9c
#[test]
fn a_private_range_reaching_metadata_or_loopback_refuses_at_startup() {
    for range in ["169.254.0.0/16", "127.0.0.0/8", "fe80::/10", "fd00:ec2::254/128", "0.0.0.0/0", "::/0"] {
        let out = storage_check(&format!("10.0.0.0/8,{range}"));
        assert!(!out.status.success(), "{range} admitted");
        let error = String::from_utf8_lossy(&out.stderr);
        assert!(error.contains("ConnectorPrivateRangeRejected") && error.contains(range), "{range}: {error}");
        assert!(out.stdout.is_empty(), "{range}: the command ran");
    }
    let out = storage_check("10.0.0.0/8, fd12:3456::/32");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}
