//! `store.endpoint`: what a backend's answers, and its silence, mean.

use contextful_core::store::object::ObjectError;

/// An unanswered request names itself apart from an answered transport failure.
#[test]
fn an_unanswered_request_reads_apart_from_an_answered_failure() {
    let unanswered = ObjectError::Unavailable("LIST `corpus/`: io: Connection refused".into());
    let answered = ObjectError::Transport("LIST `corpus/`: HTTP 404 NoSuchBucket".into());
    assert_eq!(unanswered.to_string(), "unavailable: LIST `corpus/`: io: Connection refused");
    assert_eq!(answered.to_string(), "transport: LIST `corpus/`: HTTP 404 NoSuchBucket");
}
