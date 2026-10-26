//! A table's compaction lease through the built binary: taken, renewed by its holder, and
//! folded under.

use super::{cf, file_sync, ok, project, refused};
use serde_json::Value;
use std::path::Path;

fn lease(bucket: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(bucket.join("context-team/team/research/leases/compact/filings.json")).unwrap()).unwrap()
}

/// `sync lease renew <table>` renews the compaction lease this node holds, for `sync compact --held`.
// spec: store.lease.operator-renew@647f6ee6
#[test]
fn an_operator_held_lease_renewed_by_its_holder_outlasts_its_grant() {
    let bucket = tempfile::tempdir().unwrap();
    let sync = file_sync(bucket.path(), "");
    let a = project("ingest-a", &sync);
    let b = project("ingest-b", &sync);
    std::fs::write(a.path().join("batch.jsonl"), "{\"id\":1}\n").unwrap();
    ok(&cf(a.path(), &["context", "land", "filings", "--project", "research", "--rows", "batch.jsonl", "--run-id", "run-1", "--site-id", "site", "--now", "2030-01-01T00:00:00Z"], &[]));
    ok(&cf(a.path(), &["sync", "push", "--project", "research"], &[]));

    // A node holding no lease has nothing to renew.
    refused(&cf(a.path(), &["sync", "lease", "renew", "filings", "--project", "research", "--now", "2030-01-01T00:00:00Z"], &[]), "LeaseNotHeld");

    ok(&cf(a.path(), &["sync", "lease", "acquire", "filings", "--project", "research", "--now", "2030-01-01T00:00:00Z"], &[]));
    assert_eq!(lease(bucket.path())["expires_at"], "2030-01-01T00:10:00Z");
    let renewed = ok(&cf(a.path(), &["sync", "lease", "renew", "filings", "--project", "research", "--now", "2030-01-01T00:03:20Z"], &[]));
    assert!(renewed.contains("2030-01-01T00:13:20Z"), "{renewed}");
    let object = lease(bucket.path());
    assert_eq!((&object["holder"], &object["fence"], &object["expires_at"]), (&Value::from("ingest-a"), &Value::from(1), &Value::from("2030-01-01T00:13:20Z")));

    // Past the first grant and its skew bound, the renewed grant still stands against another node.
    refused(&cf(b.path(), &["sync", "lease", "acquire", "filings", "--project", "research", "--now", "2030-01-01T00:11:00Z"], &[]), "LeaseHeld");
    // A node that never took the lease renews nothing.
    refused(&cf(b.path(), &["sync", "lease", "renew", "filings", "--project", "research", "--now", "2030-01-01T00:11:00Z"], &[]), "LeaseNotHeld");

    let folded = ok(&cf(a.path(), &["sync", "compact", "filings", "--held", "--project", "research", "--now", "2030-01-01T00:12:00Z"], &[]));
    assert!(folded.contains("folded"), "{folded}");
    let remote: Value = serde_json::from_slice(&std::fs::read(bucket.path().join("context-team/team/research/tables/filings/_pointer.json")).unwrap()).unwrap();
    assert_eq!(remote["fence"], 1, "{remote}");
    ok(&cf(a.path(), &["sync", "lease", "release", "filings", "--project", "research", "--now", "2030-01-01T00:12:30Z"], &[]));
    refused(&cf(a.path(), &["sync", "lease", "renew", "filings", "--project", "research", "--now", "2030-01-01T00:13:00Z"], &[]), "LeaseNotHeld");
}
