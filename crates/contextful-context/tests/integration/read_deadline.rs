//! Cancellation persists across preparation's interrupt reset and stops before reuse.

#[path = "../../src/read/deadline.rs"]
mod deadline;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

fn reset_then_stop(acknowledge: bool) {
    let interrupted = Arc::new(AtomicBool::new(false));
    let (cancel, receiver) = mpsc::channel();
    let (fired, observed) = mpsc::channel();
    let (resume, barrier) = mpsc::channel();
    let flag = interrupted.clone();
    let watcher = std::thread::spawn(move || {
        let mut first = true;
        deadline::watch(Instant::now(), Duration::ZERO, receiver, || {
            flag.store(true, Ordering::SeqCst);
            fired.send(()).unwrap();
            if first {
                first = false;
                barrier.recv().unwrap();
            }
        });
    });
    observed
        .recv_timeout(Duration::from_secs(5))
        .expect("the deadline interrupts");
    interrupted.store(false, Ordering::SeqCst);
    resume.send(()).unwrap();
    let reasserted = observed.recv_timeout(Duration::from_secs(5));
    if acknowledge {
        let _ = cancel.send(());
    }
    drop(cancel);
    watcher.join().unwrap();
    assert!(
        reasserted.is_ok(),
        "preparation erased cancellation: {reasserted:?}"
    );
    assert!(interrupted.load(Ordering::SeqCst));
    interrupted.store(false, Ordering::SeqCst);
    assert!(
        !interrupted.load(Ordering::SeqCst),
        "the joined watcher cannot interrupt the next statement"
    );
}

#[test]
fn cancellation_survives_a_preparation_reset_until_the_statement_acknowledges() {
    reset_then_stop(true);
}

#[test]
fn a_disconnected_running_statement_stops_cancellation_before_connection_reuse() {
    reset_then_stop(false);
}

#[test]
fn a_completed_or_disconnected_statement_stops_the_watcher_without_an_interrupt() {
    for acknowledge in [true, false] {
        let (cancel, receiver) = mpsc::channel();
        if acknowledge {
            cancel.send(()).unwrap();
        }
        drop(cancel);
        let mut interrupts = 0;
        deadline::watch(Instant::now(), Duration::ZERO, receiver, || interrupts += 1);
        assert_eq!(interrupts, 0, "completion acknowledgement={acknowledge}");
    }
}
