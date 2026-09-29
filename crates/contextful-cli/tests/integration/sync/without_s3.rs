//! `contextful sync` in a build linking no S3 adapter.
#![cfg(not(feature = "s3-sync"))]

use super::{cf, project, refused};

/// A build without the S3 adapter refuses an S3 or R2 endpoint by name.
#[test]
fn a_build_without_the_s3_adapter_refuses_an_s3_endpoint() {
    let p = project("ingest-a", "endpoint = \"r2://account\"\nbucket = \"context-team\"\nprefix = \"team\"\n");
    refused(&cf(p.path(), &["sync", "probe", "--project", "research"], &[]), "SyncEndpointUnsupported");
}
