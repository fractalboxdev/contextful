//! A fresh child process measures the heap retained by opening each audit chain.

use contextful_eval::record;
use contextful_policy::audit::AuditLog;
use contextful_policy::issue::SeedSigner;
use serde_json::json;
use std::alloc::{GlobalAlloc, Layout, System};
use std::process::Command;
use std::sync::atomic::{AtomicIsize, Ordering};

const ID: &str = "audit-open-bounded-heap";
const SEED: u64 = 0x5eed_0081;
static LIVE: AtomicIsize = AtomicIsize::new(0);

struct CountingAllocator;

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            LIVE.fetch_add(layout.size() as isize, Ordering::SeqCst);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size() as isize, Ordering::SeqCst);
        System.dealloc(ptr, layout);
    }

    unsafe fn realloc(&self, ptr: *mut u8, old: Layout, new_size: usize) -> *mut u8 {
        let grown = System.realloc(ptr, old, new_size);
        if !grown.is_null() {
            LIVE.fetch_add(new_size as isize - old.size() as isize, Ordering::SeqCst);
        }
        grown
    }
}

fn signer() -> SeedSigner {
    SeedSigner::from_seed(&format!("ed25519-private/{}", "07".repeat(32))).unwrap()
}

fn retained_on_open(entries: usize) -> isize {
    let dir = tempfile::tempdir().unwrap();
    {
        let log = AuditLog::open(dir.path(), signer()).unwrap();
        for start in (0..entries).step_by(1024) {
            let batch = (start..(start + 1024).min(entries))
                .map(|i| json!({"contextful.subject.agent": "agent://probe", "contextful.result.rows": i}))
                .collect();
            log.append_all(batch).unwrap();
        }
    }
    let before = LIVE.load(Ordering::SeqCst);
    let log = AuditLog::open(dir.path(), signer()).unwrap();
    std::hint::black_box(&log);
    let retained = LIVE.load(Ordering::SeqCst) - before;
    drop(log);
    retained
}

fn child(entries: usize) -> isize {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--measure", &entries.to_string()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--measure") {
        let entries = args.get(2).expect("an entry count").parse().unwrap();
        println!("{}", retained_on_open(entries));
        return;
    }
    let small = child(1_000);
    let large = child(100_000);
    let delta_kib = (large - small).max(0) as f64 / 1024.0;
    record::emit(ID, delta_kib, 100_000, SEED);
    if delta_kib >= 64.0 {
        eprintln!("{ID}: {delta_kib:.1} KiB retained beyond the 1k-entry open");
        std::process::exit(1);
    }
}
