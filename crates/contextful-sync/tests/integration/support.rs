//! Scratch stores sharing one filesystem bucket, and a bucket wrapper a test scripts.

use contextful_context::land::{land, Batch, RunContext};
use contextful_context::Store;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::object::{Condition, ObjectError, ObjectStore, Put};
use contextful_core::store::reserve::Injection;
use contextful_core::store::sync::SyncConfig;
use contextful_core::time::Instant;
use contextful_sync::{FsBucket, Syncer};
use std::sync::{Arc, Mutex};

pub fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

pub struct Node {
    /// Holds the node's scratch directory for the test's life.
    pub _dir: tempfile::TempDir,
    pub syncer: Syncer,
}

pub fn bucket(dir: &std::path::Path) -> Arc<dyn ObjectStore> {
    Arc::new(FsBucket::open(dir, "context-team").unwrap())
}

pub fn node(id: &str, bucket: Arc<dyn ObjectStore>, extra_config: &str) -> Node {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".contextful/context/research");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("config.toml"), format!("[node]\nid = \"{id}\"\n{extra_config}")).unwrap();
    let store = Store::open(dir.path(), "research").unwrap();
    let config = SyncConfig { endpoint: "file://bucket".into(), bucket: "context-team".into(), prefix: Some("team".into()), coordination: Some("cas".into()), ..SyncConfig::default() };
    let syncer = Syncer { store, bucket, config, prefix: "team".into(), project: "research".into(), node: id.into() };
    Node { _dir: dir, syncer }
}

impl Node {
    pub fn land(&self, run: &str, rows: serde_json::Value, now: &str) {
        self.land_into("filings", run, rows, now)
    }

    pub fn land_into(&self, table: &str, run: &str, rows: serde_json::Value, now: &str) {
        let rows = rows.as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect();
        let ctx = RunContext {
            node: NodeId::parse(&self.syncer.node).unwrap(),
            injection: Injection { run_id: run.into(), site_id: "site".into(), batch_seq: Some(0), authored_by: None },
            committed_at: at(now),
        };
        land(&self.syncer.store, &TableDecl::named(table), &Batch { rows, types: Default::default() }, &ctx).unwrap();
    }

    pub fn root(&self) -> std::path::PathBuf {
        self.syncer.store.root().to_path_buf()
    }
}

/// A scripted answer to a get, in place of the bucket's.
pub type OnGet = Box<dyn Fn(&str) -> Option<Result<Option<(Vec<u8>, String)>, ObjectError>> + Send + Sync>;
/// A scripted answer to a put, in place of the bucket's.
pub type OnPut = Box<dyn Fn(&str, &Condition) -> Option<Result<Put, ObjectError>> + Send + Sync>;

/// A bucket wrapper: `on_get` may answer a get in place of the bucket, and `ignore_conditions`
/// makes every put apply as if unconditional.
#[derive(Default)]
pub struct Script {
    pub on_get: Option<OnGet>,
    pub on_put: Option<OnPut>,
    pub ignore_conditions: bool,
    pub gets: Mutex<Vec<String>>,
}

pub struct Scripted {
    pub inner: Arc<dyn ObjectStore>,
    pub script: Script,
}

impl ObjectStore for Scripted {
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, String)>, ObjectError> {
        self.script.gets.lock().unwrap().push(key.to_string());
        if let Some(answer) = self.script.on_get.as_ref().and_then(|f| f(key)) {
            return answer;
        }
        self.inner.get(key)
    }

    fn put(&self, key: &str, bytes: &[u8], condition: Condition) -> Result<Put, ObjectError> {
        if let Some(answer) = self.script.on_put.as_ref().and_then(|f| f(key, &condition)) {
            return answer;
        }
        self.inner.put(key, bytes, if self.script.ignore_conditions { Condition::None } else { condition })
    }

    fn delete(&self, key: &str) -> Result<(), ObjectError> {
        self.inner.delete(key)
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, ObjectError> {
        self.inner.list(prefix)
    }
}
