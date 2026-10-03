//! The run state `sync push` carries, and the cursor a cold node resumes from.

use super::{cf, file_sync, ok, STORE};
use crate::pipeline::Vendor;
use std::path::Path;

/// A node `id` declaring an incremental `shop` pipeline against `vendor`, whose table collects folded runs at once.
fn shop(id: &str, bucket: &Path, vendor: &Vendor) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(STORE);
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), format!("[node]\nid = \"{id}\"\n\n[sync]\n{}", file_sync(bucket, ""))).unwrap();
    std::fs::write(
        dir.path().join("contextful.toml"),
        "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"shop_orders\"\nretain_runs = \"0s\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("pipelines")).unwrap();
    std::fs::write(
        dir.path().join("pipelines/shop.toml"),
        format!(
            "id = \"shop\"\nincremental = \"at\"\ntables = [\"orders\"]\n[source]\nname = \"http\"\nconfig = {{ endpoint = \"{}\", since_param = \"since\" }}\n",
            vendor.url("/v1/{table}")
        ),
    )
    .unwrap();
    dir
}

fn fire(dir: &Path, run: &str, now: &str) -> String {
    ok(&cf(dir, &["pipeline", "run", "shop", "--project", "research", "--run-id", run, "--site-id", "site", "--now", now], &[]))
}

/// `sync push` and `sync manifest --emit` first write the node's run state to `nodes/<node-id>/run-state.json`:
/// each pipeline's newest run and each cursor row with its commit marker. A run opening where a pulled run state records a commit marker for its pipeline and table newer than
/// every local one resumes from that marker's cursor.
// spec: store.push.run-state@035f3a8c
// spec: store.pull.run-state-cursor@76b40587
#[test]
fn a_cold_node_resumes_the_cursor_a_push_carried() {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"s1\",\"at\":5},{\"id\":\"s2\",\"at\":9}]".into()));
    let bucket = tempfile::tempdir().unwrap();
    let a = shop("ingest-a", bucket.path(), &vendor);
    fire(a.path(), "f1", "2030-01-01T00:00:00Z");
    // The fold collects the run, and its manifest's cursor with it.
    ok(&cf(a.path(), &["context", "compact", "shop_orders", "--project", "research", "--now", "2030-01-01T00:10:00Z"], &[]));
    assert!(!a.path().join(STORE).join("tables/shop_orders/data/runs/f1").exists());

    // The emitted plan names the run state the push commits.
    let plan: serde_json::Value = serde_json::from_str(&ok(&cf(a.path(), &["sync", "manifest", "--emit", "--project", "research"], &[]))).unwrap();
    assert_eq!(plan["entries"]["research/nodes/ingest-a/run-state.json"]["owner"], "ingest-a", "{plan}");
    ok(&cf(a.path(), &["sync", "push", "--project", "research"], &[]));
    let state: serde_json::Value = serde_json::from_slice(&std::fs::read(a.path().join(STORE).join("nodes/ingest-a/run-state.json")).unwrap()).unwrap();
    assert_eq!(state["runs"]["shop"]["run_id"], "f1", "{state}");
    assert_eq!(state["cursors"][0]["run_id"], "f1", "{state}");
    assert!(state.get("control").is_none(), "{state}");

    // A cold node pulls the run state, and its first fire carries `since`.
    let b = shop("ingest-b", bucket.path(), &vendor);
    ok(&cf(b.path(), &["sync", "pull", "--project", "research"], &[]));
    fire(b.path(), "g1", "2030-01-01T00:20:00Z");
    assert_eq!(vendor.targets(), ["/v1/orders", "/v1/orders?since=9"]);
}

/// `shop` scheduled `every 1h`, with the site id a dispatched run takes from the manifest, applied.
fn scheduled_shop(id: &str, bucket: &Path, vendor: &Vendor) -> tempfile::TempDir {
    let dir = shop(id, bucket, vendor);
    let manifest = dir.path().join("contextful.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, format!("site_id = \"site\"\n{text}")).unwrap();
    let spec = dir.path().join("pipelines/shop.toml");
    let text = std::fs::read_to_string(&spec).unwrap();
    std::fs::write(&spec, text.replacen("incremental", "schedule = \"every 1h\"\nincremental", 1)).unwrap();
    ok(&cf(dir.path(), &["pipeline", "import", "--project", "research"], &[]));
    ok(&cf(dir.path(), &["pipeline", "apply", "--project", "research"], &[]));
    dir
}

fn serve_cycle(dir: &Path, now: &str) -> serde_json::Value {
    serde_json::from_str(&ok(&cf(dir, &["pipeline", "serve", "--cycle", "--project", "research", "--now", now], &[]))).unwrap()
}

/// A pipeline's last journaled run start is the latest start across this node's catalog and every run state a
/// push carries that records the pipeline.
// spec: surface.arm.pulled-history@b8455caa
#[test]
fn a_cold_node_keeps_the_cadence_another_node_fired() {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"s1\",\"at\":5}]".into()));
    let bucket = tempfile::tempdir().unwrap();
    let a = scheduled_shop("ingest-a", bucket.path(), &vendor);
    assert_eq!(serve_cycle(a.path(), "2030-01-01T00:00:00Z")["fired"], serde_json::json!(["shop"]));
    ok(&cf(a.path(), &["sync", "push", "--project", "research"], &[]));

    // A cold node arming the same schedule ten minutes on fires nothing until the hour the other node set.
    let b = scheduled_shop("ingest-b", bucket.path(), &vendor);
    ok(&cf(b.path(), &["sync", "pull", "--project", "research"], &[]));
    let cycle = serve_cycle(b.path(), "2030-01-01T00:10:00Z");
    assert_eq!(cycle["fired"], serde_json::json!([]), "{cycle}");
    assert_eq!(cycle["next_due"], "2030-01-01T01:00:00Z", "{cycle}");
    assert_eq!(serve_cycle(b.path(), "2030-01-01T01:00:00Z")["fired"], serde_json::json!(["shop"]));
    assert_eq!(vendor.targets().len(), 2, "{:?}", vendor.targets());
}

/// A pulled run start later than the scheduler's current instant counts as no start.
// spec: surface.arm.pulled-future@71e2e5b2
#[test]
fn a_future_dated_run_state_leaves_a_cold_nodes_cadence_alone() {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"s1\",\"at\":5}]".into()));
    let bucket = tempfile::tempdir().unwrap();
    let a = scheduled_shop("ingest-a", bucket.path(), &vendor);
    fire(a.path(), "f1", "2031-06-01T00:00:00Z");
    ok(&cf(a.path(), &["sync", "push", "--project", "research"], &[]));

    let b = scheduled_shop("ingest-b", bucket.path(), &vendor);
    ok(&cf(b.path(), &["sync", "pull", "--project", "research"], &[]));
    let cycle = serve_cycle(b.path(), "2030-01-02T00:00:00Z");
    assert_eq!(cycle["fired"], serde_json::json!(["shop"]), "{cycle}");
    assert_eq!(cycle["next_due"], "2030-01-02T01:00:00Z", "{cycle}");
}
