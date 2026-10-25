//! The local spool and the in-process queue behind the `Queue` port, and the drain routing
//! their deliveries to the reads they wake and the batches they land.

use contextful_core::run::queue::{BodyRef, BodyStore, Queue, QueueMessage};
use contextful_core::run::{Failure, FailureTag};
use contextful_engine::queue::{drain, enqueue, Batch, Drained, LocalSpool, MemoryQueue, Wake, SPOOL_MESSAGE_CAP};
use std::collections::BTreeSet;

fn committed(table: &str, seq: i64) -> QueueMessage {
    QueueMessage::Committed { table: table.into(), commit_seq: Some(seq), run: Some(format!("load-{seq}")) }
}

fn arrival(key: &str) -> QueueMessage {
    QueueMessage::ObjectArrival { bucket: "lake".into(), key: key.into() }
}

fn batch(key: &str) -> QueueMessage {
    QueueMessage::Receive { pipeline: "events".into(), batch_key: key.into(), credential: None, queued_at: 1_900_000_000, body: BodyRef::Inline { body: format!("{key}\n").into_bytes() } }
}

#[test]
fn the_spool_redelivers_until_acknowledged_and_survives_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let spool = LocalSpool::open(dir.path()).unwrap();
    for m in [committed("spans", 1), arrival("feed/01.jsonl"), batch("b-1")] {
        spool.send(&m).unwrap();
    }
    let first = spool.receive(10).unwrap();
    assert_eq!(first.iter().map(|d| d.message.clone()).collect::<Vec<_>>(), [committed("spans", 1), arrival("feed/01.jsonl"), batch("b-1")], "oldest first");
    assert_eq!(spool.receive(10).unwrap(), first, "a received message stays queued until acknowledged");
    assert_eq!(spool.receive(2).unwrap().len(), 2);

    spool.ack(&first[0].receipt).unwrap();
    spool.ack(&first[0].receipt).unwrap();
    spool.ack("not-a-receipt").unwrap();
    let reopened = LocalSpool::open(dir.path()).unwrap();
    let rest: Vec<QueueMessage> = reopened.receive(10).unwrap().into_iter().map(|d| d.message).collect();
    assert_eq!(rest, [arrival("feed/01.jsonl"), batch("b-1")], "a process restart redelivers what no consumer acknowledged");
}

#[test]
fn a_resend_replaces_its_pending_copy_and_survives_the_originals_acknowledgement() {
    let dir = tempfile::tempdir().unwrap();
    let spool = LocalSpool::open(dir.path()).unwrap();
    spool.send(&committed("spans", 1)).unwrap();
    spool.send(&committed("spans", 2)).unwrap();
    spool.send(&committed("documents", 3)).unwrap();
    let got: Vec<QueueMessage> = spool.receive(10).unwrap().into_iter().map(|d| d.message).collect();
    assert_eq!(got, [committed("spans", 2), committed("documents", 3)], "one pending wake-up per table");

    // A commit lands between a consumer's receive and its acknowledgement: its wake-up stays.
    let received = spool.receive(10).unwrap();
    spool.send(&committed("spans", 4)).unwrap();
    for d in &received {
        spool.ack(&d.receipt).unwrap();
    }
    let got: Vec<QueueMessage> = spool.receive(10).unwrap().into_iter().map(|d| d.message).collect();
    assert_eq!(got, [committed("spans", 4)]);
}

#[test]
fn the_memory_queue_delivers_each_send_and_redelivers_until_acknowledged() {
    let queue = MemoryQueue::default();
    queue.send(&arrival("feed/02.jsonl")).unwrap();
    queue.send(&arrival("feed/02.jsonl")).unwrap();
    let got = queue.receive(10).unwrap();
    assert_eq!(got.len(), 2, "a managed queue collapses no resend");
    queue.ack(&got[0].receipt).unwrap();
    assert_eq!(queue.pending(), [arrival("feed/02.jsonl")]);
}

#[derive(Default)]
struct Woken {
    exports: Vec<BTreeSet<String>>,
    arrivals: Vec<BTreeSet<(String, String)>>,
    batches: Vec<Batch>,
    refuse_exports: bool,
    /// Batch keys whose landing fails retryable.
    retry: BTreeSet<String>,
}

impl Wake for Woken {
    fn exports(&mut self, tables: &BTreeSet<String>) -> Result<(), Failure> {
        self.exports.push(tables.clone());
        if self.refuse_exports {
            return Err(Failure::new(FailureTag::Transient, "the target answered 503"));
        }
        Ok(())
    }

    fn arrivals(&mut self, arrivals: &BTreeSet<(String, String)>) -> Result<(), Failure> {
        self.arrivals.push(arrivals.clone());
        Ok(())
    }

    fn batches(&mut self, batches: &[Batch]) -> Vec<Result<(), Failure>> {
        self.batches.extend(batches.iter().cloned());
        batches.iter().map(|b| if self.retry.contains(&b.batch_key) { Err(Failure::new(FailureTag::Transient, "the push guard is held")) } else { Ok(()) }).collect()
    }
}

#[test]
fn a_drain_wakes_each_read_once_and_acknowledges_only_what_its_wake_settled() {
    let queue = MemoryQueue::default();
    for m in [committed("spans", 1), arrival("feed/02.jsonl"), committed("spans", 2), arrival("feed/01.jsonl"), arrival("feed/02.jsonl"), batch("b-1")] {
        queue.send(&m).unwrap();
    }
    let mut woken = Woken::default();
    let tally = drain(&queue, &queue, 100, &mut woken).unwrap();
    assert_eq!(tally, Drained { wakes: 2, arrivals: 3, batches: 1, left: 0 });
    assert_eq!(woken.exports, [BTreeSet::from(["spans".to_string()])], "duplicate wake-ups wake one export pass");
    assert_eq!(woken.arrivals, [BTreeSet::from([("lake".to_string(), "feed/01.jsonl".to_string()), ("lake".to_string(), "feed/02.jsonl".to_string())])]);
    assert!(queue.pending().is_empty());

    // A failed export wake leaves its wake-ups queued and still acknowledges the arrivals.
    queue.send(&committed("spans", 3)).unwrap();
    queue.send(&arrival("feed/03.jsonl")).unwrap();
    let mut refusing = Woken { refuse_exports: true, ..Woken::default() };
    let failed = drain(&queue, &queue, 100, &mut refusing).unwrap_err();
    assert!(failed.message.contains("503"), "{failed:?}");
    assert_eq!(queue.pending(), [committed("spans", 3)]);

    // A drain over nothing to wake calls no wake.
    let empty = MemoryQueue::default();
    let mut idle = Woken::default();
    assert_eq!(drain(&empty, &empty, 100, &mut idle).unwrap(), Drained::default());
    assert!(idle.exports.is_empty() && idle.arrivals.is_empty() && idle.batches.is_empty());
}

/// A queued batch is acknowledged once its outcome records; a retryable landing leaves it queued.
#[test]
fn a_drain_lands_queued_batches_and_leaves_a_retryable_one_queued() {
    let queue = MemoryQueue::default();
    queue.send(&batch("b-1")).unwrap();
    queue.send(&batch("b-2")).unwrap();
    let mut woken = Woken { retry: BTreeSet::from(["b-2".to_string()]), ..Woken::default() };
    let failed = drain(&queue, &queue, 10, &mut woken).unwrap_err();
    assert!(failed.message.contains("push guard"), "{failed:?}");
    assert_eq!(woken.batches.iter().map(|b| (b.batch_key.as_str(), b.body.as_deref())).collect::<Vec<_>>(), [("b-1", Some(&b"b-1\n"[..])), ("b-2", Some(&b"b-2\n"[..]))]);
    assert_eq!(queue.pending(), [batch("b-2")], "the retryable batch redelivers");
}

/// A body whose message passes the cap waits in the body store and leaves it once its batch settles; a
/// redelivery after that finds no body.
#[test]
fn a_body_past_the_cap_spills_and_leaves_once_its_outcome_records() {
    let dir = tempfile::tempdir().unwrap();
    let spool = LocalSpool::open(dir.path()).unwrap();
    assert_eq!(spool.message_cap(), SPOOL_MESSAGE_CAP);
    let small = b"{\"id\":1}\n".to_vec();
    let big = vec![b'x'; SPOOL_MESSAGE_CAP];
    enqueue(&spool, &spool, "events", "small", Some("token-1"), 7, &small).unwrap();
    enqueue(&spool, &spool, "events", "big", Some("token-1"), 7, &big).unwrap();
    let pending = spool.receive(10).unwrap();
    let spilled: Vec<String> = pending
        .iter()
        .filter_map(|d| match &d.message {
            QueueMessage::Receive { body: BodyRef::Spilled { key }, .. } => Some(key.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(spilled.len(), 1, "only the body past the cap spills");
    assert_eq!(pending.len(), 2);
    assert!(pending.iter().all(|d| d.message.encoded_len() <= SPOOL_MESSAGE_CAP));
    assert_eq!(spool.get(&spilled[0]).unwrap().as_deref(), Some(&big[..]));

    let mut woken = Woken::default();
    let tally = drain(&spool, &spool, 10, &mut woken).unwrap();
    assert_eq!(tally.batches, 2);
    let bodies: Vec<(String, Option<usize>, Option<String>)> = woken.batches.iter().map(|b| (b.batch_key.clone(), b.body.as_ref().map(Vec::len), b.credential.clone())).collect();
    assert_eq!(bodies, [("small".into(), Some(small.len()), Some("token-1".into())), ("big".into(), Some(big.len()), Some("token-1".into()))]);
    assert_eq!(spool.get(&spilled[0]).unwrap(), None, "the spill leaves once its outcome records");

    // A redelivery of the spilled message after its outcome recorded finds no body.
    spool.send(&pending.iter().find(|d| matches!(&d.message, QueueMessage::Receive { body: BodyRef::Spilled { .. }, .. })).unwrap().message).unwrap();
    let mut again = Woken::default();
    drain(&spool, &spool, 10, &mut again).unwrap();
    assert_eq!(again.batches[0].body, None);
}
