//! Bucket push and pull: the filesystem and S3 object-store adapters, the manifest commit,
//! fenced bucket leases and pointer publication, the conditional-write probe and replica
//! refresh. The S3 adapter links under the `s3-sync` feature alone
//! (`topology.package.s3-sync-optional`).

pub mod fs_bucket;
#[cfg(feature = "s3-sync")]
pub mod s3_bucket;
pub mod sync;

pub use fs_bucket::{FsBucket, VolumeClass};
#[cfg(feature = "s3-sync")]
pub use s3_bucket::{S3Bucket, S3Credentials};
pub use sync::{Held, PullScope, SyncError, Syncer};
