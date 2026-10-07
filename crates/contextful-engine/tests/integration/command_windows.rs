//! Native Windows process-tree lifetime, verified through retained handles.
use contextful_core::run::ports::{PullRequest, Source};
use contextful_core::run::FailureTag;
use contextful_engine::{cancel::CancelToken, command::CommandSource};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
    fn WaitForSingleObject(handle: *mut std::ffi::c_void, millis: u32) -> u32;
    fn TerminateProcess(handle: *mut std::ffi::c_void, code: u32) -> i32;
}

struct Descendant(OwnedHandle);
impl Descendant {
    fn open(pid: u32) -> Self {
        // SAFETY: OpenProcess receives plain integers and returns a newly owned
        // handle. The fixture remains alive until this test terminates it.
        let raw = unsafe { OpenProcess(0x0010_0000 | 0x0001, 0, pid) };
        assert!(
            !raw.is_null(),
            "open descendant: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: successful OpenProcess returns an exclusively owned live handle.
        Self(unsafe { OwnedHandle::from_raw_handle(raw) })
    }
    fn exited(&self) -> bool {
        // SAFETY: the borrowed handle remains owned throughout the wait.
        match unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) } {
            0 => true,
            258 => false,
            other => panic!(
                "descendant wait {other}: {}",
                std::io::Error::last_os_error()
            ),
        }
    }
}
impl Drop for Descendant {
    fn drop(&mut self) {
        // SAFETY: the retained fixture handle has terminate/synchronize rights;
        // no PID lookup or unrelated process is involved in cleanup.
        unsafe {
            TerminateProcess(self.0.as_raw_handle(), 1);
            WaitForSingleObject(self.0.as_raw_handle(), 5_000);
        }
    }
}

fn fixture() -> &'static Path {
    static EXE: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    EXE.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("command-tree-fixture.exe");
        let abi = if cfg!(target_env = "gnu") {
            "gnu"
        } else {
            "msvc"
        };
        let target = format!("{}-pc-windows-{abi}", std::env::consts::ARCH);
        let out = std::process::Command::new("rustc")
            .arg("--edition=2021")
            .args(["--target", &target])
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/command_tree.rs"))
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        (dir, executable)
    })
    .1
    .as_path()
}

fn source(dir: &Path, mode: &str) -> CommandSource {
    CommandSource {
        argv: vec![fixture().to_string_lossy().into_owned(), mode.into()],
        cwd: dir.into(),
    }
}
fn ready(dir: &Path) -> Descendant {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !dir.join("parent.ready").exists() {
        assert!(Instant::now() < deadline, "parent and descendant readiness");
        std::thread::sleep(Duration::from_millis(5));
    }
    let pid = std::fs::read_to_string(dir.join("grandchild.pid"))
        .unwrap()
        .parse()
        .unwrap();
    Descendant::open(pid)
}
fn request() -> PullRequest {
    PullRequest {
        step_label: "windows-tree".into(),
        position: None,
        idempotency_key: "windows-tree".into(),
    }
}

// spec: run.cancel.child-reaped@8c92882f
#[test]
fn cancellation_returns_only_after_the_descendant_handle_is_signalled() {
    let dir = tempfile::tempdir().unwrap();
    let mut src = source(dir.path(), "cancel");
    let token = CancelToken::default();
    let child_token = token.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        tx.send(src.pull(&request(), &child_token)).unwrap();
    });
    let descendant = ready(dir.path());
    assert!(!descendant.exited(), "fixture alive before cancellation");
    token.fire();
    let result = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("bounded cancellation");
    worker.join().unwrap();
    assert_eq!(result.unwrap_err().tag, FailureTag::Canceled);
    assert!(
        descendant.exited(),
        "a cancelled pull leaves a live descendant"
    );
}

#[test]
fn a_finished_parent_reaps_its_pipe_holding_descendant() {
    let dir = tempfile::tempdir().unwrap();
    let mut src = source(dir.path(), "finished");
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        tx.send(src.pull(&request(), &CancelToken::default()))
            .unwrap();
    });
    let descendant = ready(dir.path());
    let result = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("bounded finished pull");
    worker.join().unwrap();
    assert_eq!(result.unwrap(), br#"{"rows":[]}"#);
    assert!(
        descendant.exited(),
        "a finished pull leaves a live descendant"
    );
}
