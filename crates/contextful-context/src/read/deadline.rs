//! The connection's deadline watcher and completion acknowledgement.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

// DuckDB preparation resets its interrupt flag; cancellation persists until completion.
const INTERRUPT_RETRY: Duration = Duration::from_millis(1);

pub(crate) fn watch(
    started: Instant,
    duration: Duration,
    receiver: Receiver<()>,
    mut interrupt: impl FnMut(),
) {
    let mut wait = duration.saturating_sub(started.elapsed());
    loop {
        match receiver.recv_timeout(wait) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => interrupt(),
        }
        wait = INTERRUPT_RETRY;
    }
}
