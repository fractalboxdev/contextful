//! A process-wide stop request, raised by `SIGINT` or `SIGTERM`, that a long-running loop
//! reads between beats so it waits out its dispatched units and releases what it holds
//! before it exits.

use std::sync::atomic::{AtomicBool, Ordering};

static REQUESTED: AtomicBool = AtomicBool::new(false);

/// Route `SIGINT` and `SIGTERM` to the stop request in place of the default exit.
pub fn install() {
    #[cfg(unix)]
    unix::install();
}

/// Whether a stop signal has arrived since [`install`].
pub fn requested() -> bool {
    REQUESTED.load(Ordering::SeqCst)
}

/// Send `SIGKILL` when `kill`, else `SIGTERM`, to every process in group `group`.
#[cfg(unix)]
pub fn signal_group(group: u32, kill: bool) -> std::io::Result<()> {
    let group = libc::pid_t::try_from(group).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let signal = if kill { libc::SIGKILL } else { libc::SIGTERM };
    // SAFETY: `killpg` reads two integers and touches no memory of this process.
    match unsafe { libc::killpg(group, signal) } {
        0 => Ok(()),
        _ => Err(std::io::Error::last_os_error()),
    }
}

#[cfg(unix)]
mod unix {
    use std::sync::atomic::Ordering;

    extern "C" fn on_signal(_: libc::c_int) {
        super::REQUESTED.store(true, Ordering::SeqCst);
    }

    pub fn install() {
        let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        // SAFETY: the handler performs one atomic store, which is async-signal-safe.
        unsafe {
            libc::signal(libc::SIGINT, handler);
            libc::signal(libc::SIGTERM, handler);
        }
    }
}
