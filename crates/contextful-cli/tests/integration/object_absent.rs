//! The `s3` object source on a binary built without the `s3-sync` feature.
#![cfg(not(feature = "s3-sync"))]

use crate::pipeline::{cf, project, stderr};

/// A build linking no S3 bucket adapter keeps `s3` listed and answers it with the feature to
/// rebuild with.
#[test]
fn a_compiled_out_object_source_answers_with_the_feature_to_rebuild_with() {
    let dir = project("[[pipeline]]\nid = \"feed\"\ntables = [\"orders\"]\n[pipeline.source]\nname = \"s3\"\nconfig = { bucket = \"lake\", key = \"orders.jsonl\" }\n");
    let out = cf(dir.path(), &["pipeline", "validate"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("source `s3` is compiled out of this build; rebuild with `--features s3-sync`"), "{}", stderr(&out));
}
