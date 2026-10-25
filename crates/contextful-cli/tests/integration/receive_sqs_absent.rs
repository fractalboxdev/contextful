//! `CONTEXTFUL_QUEUE_URL` on a binary built without the `aws-sqs` feature.
#![cfg(all(feature = "data-plane", not(feature = "aws-sqs")))]

use crate::pipeline::{project, stderr};

/// A build linking no SQS adapter refuses a configured queue by the feature to rebuild with, and spools nothing.
// spec: topology.compose.queue-sqs-absent@19f9dbcc
#[test]
fn a_configured_queue_without_the_adapter_refuses_and_spools_nothing() {
    let dir = project("[[pipeline]]\nid = \"feed\"\ntables = [\"orders\"]\n[pipeline.source]\nname = \"receive\"\nconfig = { format = \"jsonl\", max_batch_bytes = 64, max_queue_depth = 1, key_retention_secs = 60 }\n");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["queue", "drain", "--project", "research"])
        .current_dir(dir.path())
        .env("CONTEXTFUL_QUEUE_URL", "https://sqs.us-east-1.amazonaws.com/000000000000/contextful")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let said = stderr(&out);
    assert!(said.contains("ProfileCapabilityAbsent") && said.contains("--features aws-sqs"), "{said}");
    assert!(!dir.path().join(".contextful/queue/research").exists(), "no spool stands in for the configured queue");
}
