//! Default node ids through the built binary: two checkouts of one project on one host,
//! and a checkout moved after it landed a run.

use super::{cf, file_sync, ok};
use std::path::Path;

/// A checkout of `research` whose store declares `sync` and no `[node] id`.
fn checkout(at: &Path, sync: &str) {
    let store = at.join(super::STORE);
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), format!("[sync]\n{sync}")).unwrap();
    std::fs::write(at.join("contextful.toml"), "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"filings\"\n").unwrap();
}

fn land(at: &Path, env: &[(&str, &str)]) {
    std::fs::write(at.join("batch.jsonl"), "{\"id\":1}\n").unwrap();
    ok(&cf(
        at,
        &["context", "land", "filings", "--project", "research", "--rows", "batch.jsonl", "--run-id", "run-1", "--site-id", "site", "--now", "2030-01-01T00:00:00Z"],
        env,
    ));
}

fn files(at: &Path, env: &[(&str, &str)]) -> Vec<String> {
    ok(&cf(at, &["context", "files", "filings", "--project", "research"], env)).lines().map(str::to_string).collect()
}

/// A project's default node id is `node-<8 hex>` from SHA-256 over the host id and absolute store root. Two checkouts
/// of one project on one host therefore write as distinct nodes.
// spec: store.lay-out.node-id-project@3117e7fb
#[test]
fn a_second_checkout_of_one_project_pulls_the_first_checkouts_run() {
    let host = tempfile::tempdir().unwrap();
    let bucket = tempfile::tempdir().unwrap();
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let state = host.path().to_string_lossy().into_owned();
    let env = [("CONTEXTFUL_STATE_DIR", state.as_str())];
    let sync = file_sync(bucket.path(), "");
    checkout(a.path(), &sync);
    checkout(b.path(), &sync);
    land(a.path(), &env);
    let landed = files(a.path(), &env);
    assert_eq!(landed.len(), 1, "{landed:?}");
    assert!(landed[0].starts_with("tables/filings/data/runs/run-1/node-"), "{landed:?}");
    ok(&cf(a.path(), &["sync", "push", "--project", "research"], &env));

    ok(&cf(b.path(), &["sync", "pull", "--project", "research"], &env));
    assert_eq!(files(b.path(), &env), landed);
}

/// Push reports local keys owned by other nodes that the committed bucket manifest neither lists nor tombstones.
/// Stderr groups keys by owner and names the `CONTEXTFUL_NODE_ID` required to push each group.
// spec: store.push.stranded@607eeb97
#[test]
fn a_moved_checkout_names_the_run_it_landed_before_the_move() {
    let host = tempfile::tempdir().unwrap();
    let bucket = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let (before, after) = (parent.path().join("a"), parent.path().join("moved"));
    let state = host.path().to_string_lossy().into_owned();
    let env = [("CONTEXTFUL_STATE_DIR", state.as_str())];
    checkout(&before, &file_sync(bucket.path(), ""));
    land(&before, &env);
    let landed = files(&before, &env);
    let owner = landed[0].split('/').nth(5).unwrap().to_string();
    std::fs::rename(&before, &after).unwrap();

    let out = cf(&after, &["sync", "push", "--project", "research"], &env);
    ok(&out);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("warning:") && stderr.contains(&format!("`{owner}`")) && stderr.contains(&format!("CONTEXTFUL_NODE_ID={owner}")), "{stderr}");

    // Pushing as the named node uploads the run, and the next push names nothing.
    let rebound = [("CONTEXTFUL_STATE_DIR", state.as_str()), ("CONTEXTFUL_NODE_ID", owner.as_str())];
    let pushed = ok(&cf(&after, &["sync", "push", "--project", "research"], &rebound));
    assert!(pushed.starts_with("pushed ") && !pushed.starts_with("pushed 0 "), "{pushed}");
    let again = cf(&after, &["sync", "push", "--project", "research"], &env);
    ok(&again);
    assert!(!String::from_utf8_lossy(&again.stderr).contains("warning:"), "{}", String::from_utf8_lossy(&again.stderr));
}
