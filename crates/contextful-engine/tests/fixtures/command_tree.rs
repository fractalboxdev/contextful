//! A finite command-source process tree for native integration tests.
use std::io::Write;
use std::time::{Duration, Instant};

#[cfg(windows)]
fn creation_identity() -> u64 {
    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn GetProcessTimes(
            process: *mut std::ffi::c_void,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
    }
    let (mut creation, mut exit, mut kernel, mut user) = (
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
    );
    // SAFETY: the pseudo-handle denotes this fixture process and all output
    // pointers address initialized FILETIME-layout values for the call.
    assert_ne!(
        unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        },
        0
    );
    (u64::from(creation.high) << 32) | u64::from(creation.low)
}
#[cfg(not(windows))]
fn creation_identity() -> u64 {
    0
}

fn main() {
    let mode = std::env::args().nth(1).expect("fixture mode");
    if mode == "grandchild" {
        std::fs::write(
            "grandchild.pid",
            format!("{} {}", std::process::id(), creation_identity()),
        )
        .unwrap();
        std::thread::sleep(Duration::from_secs(30));
        return;
    }
    assert!(matches!(mode.as_str(), "cancel" | "finished"));
    // The finished-parent case deliberately leaves the descendant alive; the
    // command source and retained-handle test own its bounded cleanup.
    #[allow(clippy::zombie_processes)]
    let _child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("grandchild")
        .stdin(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !std::path::Path::new("grandchild.pid").exists() {
        assert!(Instant::now() < deadline, "grandchild readiness");
        std::thread::sleep(Duration::from_millis(5));
    }
    std::fs::write("parent.ready", "ready").unwrap();
    if mode == "finished" {
        print!("{{\"rows\":[]}}");
        std::io::stdout().flush().unwrap();
    } else {
        std::thread::sleep(Duration::from_secs(30));
    }
}
