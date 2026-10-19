//! `store.endpoint`: what a backend's answers, and its silence, mean.

use contextful_core::store::object::ObjectError;

#[test]
fn a_store_get_within_defaults_to_get() {
    use contextful_core::store::object::{Condition, ObjectStore, Put};
    use std::time::{Duration, Instant};
    struct Store;
    impl ObjectStore for Store {
        fn get(&self, key: &str) -> Result<Option<(Vec<u8>, String)>, ObjectError> {
            Ok(Some((key.as_bytes().to_vec(), "tag".into())))
        }
        fn put(&self, _: &str, _: &[u8], _: Condition) -> Result<Put, ObjectError> {
            unreachable!()
        }
        fn delete(&self, _: &str) -> Result<(), ObjectError> {
            unreachable!()
        }
        fn list(&self, _: &str) -> Result<Vec<String>, ObjectError> {
            unreachable!()
        }
    }
    let store: &dyn ObjectStore = &Store;
    assert_eq!(store.get_within("key", Instant::now() + Duration::from_secs(1)).unwrap(), store.get("key").unwrap());
}

/// An unanswered request names itself apart from an answered transport failure.
#[test]
fn an_unanswered_request_reads_apart_from_an_answered_failure() {
    let unanswered = ObjectError::Unavailable("LIST `corpus/`: io: Connection refused".into());
    let answered = ObjectError::Transport("LIST `corpus/`: HTTP 404 NoSuchBucket".into());
    assert_eq!(unanswered.to_string(), "unavailable: LIST `corpus/`: io: Connection refused");
    assert_eq!(answered.to_string(), "transport: LIST `corpus/`: HTTP 404 NoSuchBucket");
}
