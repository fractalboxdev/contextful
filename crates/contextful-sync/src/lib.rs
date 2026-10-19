//! Bucket push and pull: the filesystem and S3 object-store adapters, the manifest commit,
//! fenced bucket leases and pointer publication, the conditional-write probe and replica
//! refresh, and the cross-owner store's boundary check. The S3 adapter links under the `s3-sync` feature alone
//! (`topology.package.s3-sync-optional`).

pub mod clean_room;
pub mod edge;
pub mod fs_bucket;
pub mod run_state;
#[cfg(feature = "s3-sync")]
pub mod s3_bucket;
pub mod sync;

pub use edge::ServingEdge;
pub use fs_bucket::{FsBucket, VolumeClass};
#[cfg(feature = "s3-sync")]
pub use s3_bucket::{S3Bucket, S3Credentials};
pub use run_state::{CursorMark, RunMark, RunState};
pub use sync::{plan_manifest, GenerationSummary, Held, HELD_FILE, ManifestPlan, PullScope, Redistribution, SiteResidency, SyncError, Syncer};
