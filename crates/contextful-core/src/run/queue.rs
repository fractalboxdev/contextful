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

/// One message on the port. Each kind names what its consumer wakes or lands; none carries
/// rows a consumer writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QueueMessage {
    /// A `receive` batch admitted at the route, landed by the engine under its batch key.
    Receive { pipeline: String, batch_key: String, body: BodyRef },
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
/// An adapter implements three calls:
///
/// | Call | Contract |
/// | --- | --- |
/// | [`send`](Queue::send) | the message is durable on return |
/// | [`receive`](Queue::receive) | at most `max` unacknowledged messages, possibly none |
/// | [`ack`](Queue::ack) | the delivery behind `receipt` leaves the queue; a stale receipt is no error |
pub trait Queue: Send + Sync {
    fn send(&self, message: &QueueMessage) -> Result<(), Failure>;
    fn receive(&self, max: usize) -> Result<Vec<Delivery>, Failure>;
    fn ack(&self, receipt: &str) -> Result<(), Failure>;
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
