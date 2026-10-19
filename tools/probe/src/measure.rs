//! What the heap and resident-set probes share: a counting allocator holding the live and
//! peak heap, a process's own peak resident set, child processes, the fixture scale and a
//! seeded generator. Each probe binary includes this file as a module and uses its part.

#![allow(dead_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::process::Command;
use std::sync::atomic::{AtomicIsize, Ordering};

/// Set to `small` by the probe suite outside a measure run: a probe then builds the
/// workspace-stage fixture, not the ledger's.
pub const SCALE_VAR: &str = "CONTEXTFUL_PROBE_SCALE";

static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);

/// A system allocator counting live heap bytes and their high-water mark.
pub struct PeakAllocator;

unsafe impl GlobalAlloc for PeakAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            grew(layout.size() as isize);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size() as isize, Ordering::SeqCst);
        System.dealloc(ptr, layout);
    }

    unsafe fn realloc(&self, ptr: *mut u8, old: Layout, new_size: usize) -> *mut u8 {
        let moved = System.realloc(ptr, old, new_size);
        if !moved.is_null() {
            grew(new_size as isize - old.size() as isize);
        }
        moved
    }
}

fn grew(by: isize) {
    let live = LIVE.fetch_add(by, Ordering::SeqCst) + by;
    PEAK.fetch_max(live, Ordering::SeqCst);
}

/// The heap `f` holds at its peak above the heap live when it starts, in bytes, and its result.
pub fn peak_heap_of<T>(f: impl FnOnce() -> T) -> (isize, T) {
    let before = LIVE.load(Ordering::SeqCst);
    PEAK.store(before, Ordering::SeqCst);
    let out = f();
    (PEAK.load(Ordering::SeqCst) - before, out)
}

/// This process's peak resident set in MiB: `ru_maxrss` counts KiB on Linux and bytes on macOS.
#[cfg(unix)]
pub fn peak_rss_mib() -> f64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    assert_eq!(rc, 0, "getrusage failed");
    let max = unsafe { usage.assume_init() }.ru_maxrss as f64;
    if cfg!(target_os = "macos") {
        max / (1024.0 * 1024.0)
    } else {
        max / 1024.0
    }
}

#[cfg(not(unix))]
pub fn peak_rss_mib() -> f64 {
    panic!("a resident-set probe reads getrusage, which this platform lacks")
}

/// Whether the probe builds the workspace-stage fixture rather than the ledger's.
pub fn small() -> bool {
    std::env::var(SCALE_VAR).as_deref() == Ok("small")
}

/// Run this binary again with `args`, returning its trimmed standard output.
pub fn child(args: &[&str]) -> String {
    let output = Command::new(std::env::current_exe().unwrap()).args(args).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// The median of `xs`, the lower middle of an even count.
pub fn median(mut xs: Vec<f64>) -> f64 {
    xs.sort_by(f64::total_cmp);
    xs[(xs.len() - 1) / 2]
}

/// A linear congruential generator: every fixture value derives from the entry's seed.
pub struct Draw(u64);

impl Draw {
    pub fn new(seed: u64) -> Draw {
        Draw(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    /// A value in `[-1, 1)`.
    pub fn unit(&mut self) -> f32 {
        (self.next() as f32 / (1u64 << 31) as f32) * 2.0 - 1.0
    }
}

/// Copy the directory tree at `from` into `to`.
pub fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}
