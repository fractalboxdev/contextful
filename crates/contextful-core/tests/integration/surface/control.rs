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
