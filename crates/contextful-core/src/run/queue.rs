//! The `Queue` port (`topology.compose.queue-extension`): the one path `receive` batches,
//! object-arrival keys and post-commit wake-ups travel between ingress and the engine.
//!
//! A target profile declaring a `queue` role reaches its managed queue through an adapter
//! behind this port; a profile declaring none uses the engine's local spool. A message
//! carries keys and references only: no adapter and no consumer holds a store handle, and
//! every store write stays in the engine.

use crate::run::Failure;
use serde::{Deserialize, Serialize};

/// Where a queued `receive` body sits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "at", rename_all = "snake_case")]
pub enum BodyRef {
    /// The body rides the message, base64 in its encoded form.
    Inline {
        #[serde(with = "base64_bytes")]
        body: Vec<u8>,
    },
    /// The body sits in the bucket under `key`, written there before the message queued.
    Spilled { key: String },
}

/// A credential riding a `receive` message. Its debug form withholds the token, so a logged
/// message prints no credential.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Credential(pub String);

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credential(..)")
    }
}

/// One message on the port. Each kind names what its consumer wakes or lands; none carries
/// rows a consumer writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QueueMessage {
    /// A `receive` batch admitted at the route, landed by the engine under its batch key.
    Receive {
        pipeline: String,
        batch_key: String,
        /// The credential the route admitted, which the consumer admits again
        /// (`run.land.receive-queued-credential`); `None` lands unauthored.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credential: Option<Credential>,
        /// The instant the batch queued, in Unix seconds: its key window opens here
        /// (`run.land.receive-queued-window`).
        queued_at: i64,
        body: BodyRef,
        /// The route span the delivery queued under, which the runs landing it continue
        /// (`disclosure.record.queued-parent`); `None` lands under the consumer's trace.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace: Option<crate::trace::Carrier>,
    },
    /// An object created in `bucket` at `key`: a hint that wakes the prefix reads covering
    /// it (`run.land.object-arrival-wake`).
    ObjectArrival { bucket: String, key: String },
    /// A run's manifest published on `table` under `commit_seq`: a hint that wakes the
    /// exports reading the table (`run.export.queue-wake`). `commit_seq` is absent for a run
    /// whose parts landed without the column, and `run` for a store sealing its run state,
    /// whose run ids never leave it in clear.
    Committed {
        table: String,
        commit_seq: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run: Option<String>,
    },
}

impl QueueMessage {
    /// The identity a resend shares with its original: the batch key for a `receive`
    /// batch, the object for an arrival, the table for a wake-up. Two messages under one
    /// identity wake or land the same thing once, so an adapter may collapse them.
    /// The bytes the message occupies on a queue: its JSON encoding, which an adapter holds
    /// against [`Queue::message_cap`] (`run.land.receive-queued-spill`).
    pub fn encoded_len(&self) -> usize {
        serde_json::to_vec(self).map(|v| v.len()).unwrap_or(usize::MAX)
    }

    pub fn identity(&self) -> String {
        match self {
            QueueMessage::Receive { pipeline, batch_key, .. } => format!("receive\u{1f}{pipeline}\u{1f}{batch_key}"),
            QueueMessage::ObjectArrival { bucket, key } => format!("object\u{1f}{bucket}\u{1f}{key}"),
            QueueMessage::Committed { table, .. } => format!("committed\u{1f}{table}"),
        }
    }
}

/// One received message and the receipt that acknowledges it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    /// The adapter's handle for [`Queue::ack`]; opaque to the consumer.
    pub receipt: String,
    pub message: QueueMessage,
}

/// The queue port. Delivery is at least once: a received message stays queued until its
/// receipt is acknowledged, and a receive after a consumer's crash redelivers it. Order is
/// unspecified, a provider may deliver a message twice, and a message lost between a
/// commit and its enqueue is never sent, so a consumer treats each message as a hint its
/// own durable position confirms.
///
/// An adapter implements four calls:
///
/// | Call | Contract |
/// | --- | --- |
/// | [`send`](Queue::send) | the message is durable on return |
/// | [`receive`](Queue::receive) | at most `max` unacknowledged messages, possibly none |
/// | [`ack`](Queue::ack) | the delivery behind `receipt` leaves the queue; a stale receipt is no error |
/// | [`message_cap`](Queue::message_cap) | the largest [`QueueMessage::encoded_len`] `send` takes |
pub trait Queue: Send + Sync {
    fn send(&self, message: &QueueMessage) -> Result<(), Failure>;
    fn receive(&self, max: usize) -> Result<Vec<Delivery>, Failure>;
    fn ack(&self, receipt: &str) -> Result<(), Failure>;
    fn message_cap(&self) -> usize;
}

/// Where a `receive` body past the queue's message cap waits under its batch key: written
/// before its message queues, deleted once its outcome records
/// (`run.land.receive-queued-spill`).
pub trait BodyStore: Send + Sync {
    /// Write `body` under `key`; durable on return.
    fn put(&self, key: &str, body: &[u8]) -> Result<(), Failure>;
    /// The body under `key`; `None` once deleted.
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Failure>;
    /// Delete the body under `key`; an absent body is no error.
    fn delete(&self, key: &str) -> Result<(), Failure>;
}

mod base64_bytes {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        STANDARD.decode(text).map_err(serde::de::Error::custom)
    }
}
