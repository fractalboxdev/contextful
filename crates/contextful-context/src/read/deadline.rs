//! The connection's deadline watcher and completion acknowledgement.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

pub(crate) fn watch(_started: Instant, duration: Duration, receiver: Receiver<()>, mut interrupt: impl FnMut()) {
    if receiver.recv_timeout(duration).is_err_and(|e| e == RecvTimeoutError::Timeout) {
        interrupt();
    }
}
