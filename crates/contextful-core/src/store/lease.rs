//! `store.lease`: the bucket lease object, its fence, and the decisions acquisition,
//! renewal and release make against the object's current state.

use super::object::Condition;
use super::StoreError;
use crate::time::Instant;
use serde::{Deserialize, Serialize};

/// Granted life of a lease: 10 min (`store.lease.ttl`).
pub const TTL_SECS: u64 = 10 * 60;
/// Interval at which a holder renews: 200 s (`store.lease.renewal`).
pub const RENEWAL_SECS: u64 = 200;
/// Largest clock difference between two machines coordination assumes: 30 s (`store.lease.clock-skew`).
pub const CLOCK_SKEW_SECS: u64 = 30;
/// The node id no bucket lease is taken under.
pub const LOCAL_NODE: &str = "local";

/// The lease object at `leases/<pipeline-id>.json` or `leases/compact/<table>.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BucketLease {
    #[serde(default)]
    pub holder: Option<String>,
    #[serde(default)]
    pub acquired_at: Option<Instant>,
    #[serde(default)]
    pub expires_at: Option<Instant>,
    pub fence: u64,
}

/// The key of a table's compaction lease under the prefix.
pub fn compaction_key(project: &str, table: &str) -> String {
    format!("{project}/leases/compact/{table}.json")
}

/// The key of a pipeline's lease under the prefix.
pub fn pipeline_key(project: &str, pipeline_id: &str) -> String {
    format!("{project}/leases/{pipeline_id}.json")
}

impl BucketLease {
    /// Whether another holder's grant still stands at `now` on the judging machine's clock,
    /// allowing the skew bound before reading it lapsed.
    pub fn held_against(&self, me: &str, now: Instant) -> bool {
        match (&self.holder, self.expires_at) {
            (Some(h), Some(e)) if h != me => now < e.plus_secs(CLOCK_SKEW_SECS),
            _ => false,
        }
    }

    /// The object an acquisition writes and the condition it writes under: a create for a
    /// key no lease holds, else a replace on the ETag read, the fence plus one. An
    /// unexpired lease another node holds refuses (`store.lease.held`).
    pub fn acquire(current: Option<(&BucketLease, &str)>, me: &str, now: Instant) -> Result<(BucketLease, Condition), StoreError> {
        if me == LOCAL_NODE {
            return Err(StoreError::LeaseNodeIdLocal(
                "a bucket lease needs a node id; set `[node] id` or `CONTEXTFUL_NODE_ID`, and this machine keeps its machine lease".into(),
            ));
        }
        let grant = |fence| BucketLease { holder: Some(me.to_string()), acquired_at: Some(now), expires_at: Some(now.plus_secs(TTL_SECS)), fence };
        match current {
            None => Ok((grant(1), Condition::IfNoneMatch)),
            Some((lease, etag)) => {
                if lease.held_against(me, now) {
                    return Err(StoreError::LeaseHeld(format!(
                        "`{}` holds the lease until {}; the run is skipped and tried again next tick",
                        lease.holder.as_deref().unwrap_or_default(),
                        lease.expires_at.map(|e| e.to_string()).unwrap_or_default()
                    )));
                }
                Ok((grant(lease.fence + 1), Condition::IfMatch(etag.to_string())))
            }
        }
    }

    /// Renew a held lease on the ETag held (`store.lease.renewal`); a fence moved past it refuses.
    pub fn renew(current: &BucketLease, etag: &str, mine: &BucketLease, now: Instant) -> Result<(BucketLease, Condition), StoreError> {
        if current.fence != mine.fence || current.holder != mine.holder {
            return Err(StoreError::LeaseFenced(format!("the lease moved to fence {} past this holder's {}", current.fence, mine.fence)));
        }
        Ok((BucketLease { expires_at: Some(now.plus_secs(TTL_SECS)), ..current.clone() }, Condition::IfMatch(etag.to_string())))
    }

    /// Release: the holder clears and the fence stays. Releasing another node's lease
    /// refuses and leaves the object untouched (`store.lease.not-held`).
    pub fn release(current: &BucketLease, etag: &str, me: &str) -> Result<(BucketLease, Condition), StoreError> {
        if current.holder.as_deref() != Some(me) {
            return Err(StoreError::LeaseNotHeld(format!(
                "the lease is held by `{}`, not `{me}`",
                current.holder.as_deref().unwrap_or("nobody")
            )));
        }
        Ok((BucketLease { holder: None, acquired_at: None, expires_at: None, fence: current.fence }, Condition::IfMatch(etag.to_string())))
    }
}

/// A table pointer in the bucket: the published snapshot and the fence of the newest
/// compaction lease acquired for the table. Acquisition raises the fence on this object,
/// so a publish carrying a lower fence loses its condition (`store.lease.stale-fence`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BucketPointer {
    #[serde(default)]
    pub snapshot_id: Option<String>,
    #[serde(default)]
    pub fence: u64,
}

impl BucketPointer {
    /// Hold a publish under `fence` to the pointer as read: a higher stored fence refuses.
    pub fn admit(&self, fence: u64) -> Result<(), StoreError> {
        if self.fence > fence {
            return Err(StoreError::LeaseFenced(format!(
                "the pointer carries fence {} and this publish carries {fence}; the snapshot stays unreadable",
                self.fence
            )));
        }
        Ok(())
    }
}
