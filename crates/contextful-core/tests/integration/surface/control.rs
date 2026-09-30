//! `surface.reconcile`: the snapshot pointer's grammar and the version file it names.

use contextful_core::surface::control::{parse_pointer, snapshot_file, POINTER_FILE};

/// A pointer body that is not wholly a version raises `ControlPointerMalformed`.
// spec: surface.reconcile.pointer-malformed@1d4c5ae5
#[test]
fn a_pointer_is_wholly_a_version() {
    assert_eq!(parse_pointer("7").unwrap(), 7);
    assert_eq!(parse_pointer("12\n").unwrap(), 12);
    for bad in ["", "\n", "7x", "7 8", "+7", "-1", "v7", " 7", "0x10", "18446744073709551616"] {
        let e = parse_pointer(bad).expect_err(bad).to_string();
        assert!(e.starts_with("ControlPointerMalformed"), "{bad:?}: {e}");
    }
    assert_eq!(POINTER_FILE, "manifest@current");
    assert_eq!(snapshot_file(3), "manifest@v3.toml");
}

/// A control host is admitted only when every address it resolves to is loopback, and a
/// control URL carries its files directly beneath its path.
#[test]
fn a_control_url_admits_loopback_addresses_alone() {
    use contextful_core::surface::control::{admit_loopback, control_url, source_file};
    use std::net::SocketAddr;
    let v4: SocketAddr = "127.0.0.1:8787".parse().unwrap();
    let v6: SocketAddr = "[::1]:8787".parse().unwrap();
    let lan: SocketAddr = "10.0.0.4:8787".parse().unwrap();
    admit_loopback("localhost", &[v4, v6]).unwrap();
    for (host, addrs) in [("10.0.0.4", vec![lan]), ("mixed.example", vec![v4, lan]), ("nowhere.example", vec![])] {
        let e = admit_loopback(host, &addrs).expect_err(host).to_string();
        assert!(e.starts_with("ControlSourceNotLoopback") && e.contains(host), "{e}");
    }
    let base = control_url("http://127.0.0.1:8787/control").unwrap();
    assert_eq!(source_file(&base, POINTER_FILE).as_str(), "http://127.0.0.1:8787/control/manifest@current");
    assert_eq!(source_file(&base, &snapshot_file(2)).as_str(), "http://127.0.0.1:8787/control/manifest@v2.toml");
    let slash = control_url("http://127.0.0.1:8787/control/").unwrap();
    assert_eq!(source_file(&slash, POINTER_FILE).as_str(), "http://127.0.0.1:8787/control/manifest@current");
    for bad in ["ftp://127.0.0.1/control", "not a url", "http://user:pw@127.0.0.1/control", "http://127.0.0.1/control?v=1"] {
        let e = control_url(bad).expect_err(bad).to_string();
        assert!(e.starts_with("ControlSourceNotLoopback"), "{bad}: {e}");
    }
}

/// `poll` takes a schedule string, and a `[control]` block declaring none polls every 30 s.
// spec: surface.reconcile.poll-cadence@95f6740e
#[test]
fn a_poll_takes_a_schedule_and_defaults_to_thirty_seconds() {
    use contextful_core::surface::arm::Schedule;
    use contextful_core::surface::control::poll_schedule;
    assert_eq!(poll_schedule(None).unwrap(), Schedule::Every(30));
    assert_eq!(poll_schedule(Some("every 5m")).unwrap(), Schedule::Every(300));
    assert!(matches!(poll_schedule(Some("*/10 * * * *")).unwrap(), Schedule::Cron(_)));
    let e = poll_schedule(Some("every fortnight")).unwrap_err().to_string();
    assert!(e.starts_with("ScheduleUnreadable"), "{e}");
}

/// A conditional-write owner on a filesystem whose exclusive create and lock are not linearizable raises
/// `ConditionalWriteUnsupported`; a local filesystem, or one whose kind does not read, is admitted.
// spec: surface.apply.weak-conditional-backend@925a004b
#[test]
fn a_conditional_write_owner_refuses_a_network_filesystem() {
    use contextful_core::surface::control::admit_conditional;
    for kind in ["apfs", "ext4", "xfs", "tmpfs", "local", "btrfs"] {
        admit_conditional("the snapshot directory", "/srv/control", Some(kind)).unwrap();
    }
    admit_conditional("the catalog", "/srv/catalog.sqlite", None).unwrap();
    for kind in ["nfs", "NFS", "smbfs", "cifs", "afpfs", "webdav", "9p", "fuse.sshfs"] {
        let e = admit_conditional("the catalog", "/mnt/share/catalog.sqlite", Some(kind)).expect_err(kind);
        assert_eq!(e.status(), 503);
        let e = e.to_string();
        assert!(e.starts_with("ConditionalWriteUnsupported") && e.contains(kind) && e.contains("the catalog"), "{e}");
    }
}
