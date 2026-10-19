#![cfg(feature = "s3-sync")]

#[path = "../../src/s3_retry.rs"]
mod retry;

use retry::{resend_until, Resend};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[test]
fn a_whole_answer_at_the_deadline_is_kept() {
    for (mode, status) in [
        (Resend::Repeatable, 200),
        (Resend::Repeatable, 404),
        (Resend::Conditional, 403),
    ] {
        let deadline = Instant::now() + Duration::from_millis(25);
        let attempts = AtomicUsize::new(0);
        let result = resend_until("GET part", mode, Some(deadline), |_| {
            attempts.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            assert!(Instant::now() >= deadline);
            Ok::<_, std::io::Error>((status, b"whole answer"))
        });
        assert_eq!(result.unwrap(), (status, b"whole answer"));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }
}
