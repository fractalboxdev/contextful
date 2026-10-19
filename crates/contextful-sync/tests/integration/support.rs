//! Scratch stores sharing one filesystem bucket, and a bucket wrapper a test scripts.

use contextful_context::land::{land, Batch, RunContext};
use contextful_context::Store;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::object::{CasScope, Condition, ObjectError, ObjectStore, Put};
use contextful_core::store::reserve::Injection;
use contextful_core::store::sync::SyncConfig;
use contextful_core::time::Instant;
use contextful_core::issue::SignatureAlgorithm;
use contextful_core::revoke::KeySetLedger;
use contextful_policy::issue::SeedSigner;
use contextful_sync::{FsBucket, Redistribution, Syncer, VolumeClass};
use std::sync::{Arc, Mutex};

pub fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

pub struct Node {
    /// Holds the node's scratch directory for the test's life.
    pub _dir: tempfile::TempDir,
    pub syncer: Syncer,
    /// The issuer key the node signs its tombstones with.
    pub signer: Arc<SeedSigner>,
}

/// A bucket classed as a local volume whatever filesystem holds the test's scratch directory.
pub fn bucket(dir: &std::path::Path) -> Arc<dyn ObjectStore> {
    Arc::new(FsBucket::open_with_volume(dir, "context-team", VolumeClass::Local("tmpfs".into())).unwrap())
}

pub fn node(id: &str, bucket: Arc<dyn ObjectStore>, extra_config: &str) -> Node {
    node_in("research", id, bucket, extra_config)
}

/// A node `id` of `project`, whose name may hold `/`-separated segments.
pub fn node_in(project: &str, id: &str, bucket: Arc<dyn ObjectStore>, extra_config: &str) -> Node {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".contextful/context").join(project);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("config.toml"), format!("[node]\nid = \"{id}\"\n{extra_config}")).unwrap();
    let store = Store::open(dir.path(), project).unwrap();
    let config = SyncConfig { endpoint: "file://bucket".into(), bucket: "context-team".into(), prefix: Some("team".into()), coordination: Some("cas".into()), ..SyncConfig::default() };
    let signer = Arc::new(SeedSigner::generate(SignatureAlgorithm::Ed25519));
    let syncer = Syncer {
        store,
        bucket,
        config,
        prefix: "team".into(),
        project: project.into(),
        node: id.into(),
        residency: None,
        control_dir: Some(dir.path().join(".contextful/control").join(project)),
        signer: Some(signer.clone()),
        key_set: Some(dir.path().join(KeySetLedger::PATH)),
        redistribution: Redistribution::default(),
    };
    Node { _dir: dir, syncer, signer }
}

/// Record every node's issuer key, from `since`, in every node's key-set ledger.
pub fn trust(nodes: &[&Node], since: &str) {
    let mut ledger = KeySetLedger::default();
    for n in nodes {
        ledger.record(&n.signer.public_key_text(), at(since));
    }
    for n in nodes {
        let path = n.syncer.key_set.clone().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, ledger.to_toml()).unwrap();
    }
}

impl Node {
    pub fn land(&self, run: &str, rows: serde_json::Value, now: &str) {
        self.land_into("filings", run, rows, now)
    }

    pub fn land_into(&self, table: &str, run: &str, rows: serde_json::Value, now: &str) {
        let rows = rows.as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect();
        let ctx = RunContext {
            node: NodeId::parse(&self.syncer.node).unwrap(),
            injection: Injection { run_id: run.into(), site_id: "site".into(), batch_seq: Some(0), authored_by: None, authored_agent: None, taint: None },
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
    /// Each get the bucket answered with an object: its key and byte count.
    pub fetched: Mutex<Vec<(String, usize)>>,
    /// Each list: its prefix and the number of keys it returned.
    pub lists: Mutex<Vec<(String, usize)>>,
}

impl Script {
    /// Keys returned by every list under `prefix`.
    pub fn listed_under(&self, prefix: &str) -> usize {
        self.lists.lock().unwrap().iter().filter(|(p, _)| p.starts_with(prefix)).map(|(_, n)| n).sum()
    }

    /// Bytes returned by every get of `key`.
    pub fn fetched_bytes(&self, key: &str) -> usize {
        self.fetched.lock().unwrap().iter().filter(|(k, _)| k == key).map(|(_, n)| n).sum()
    }

    /// Forget every get and list recorded so far.
    pub fn reset(&self) {
        self.gets.lock().unwrap().clear();
        self.fetched.lock().unwrap().clear();
        self.lists.lock().unwrap().clear();
    }
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
        let got = self.inner.get(key)?;
        if let Some((bytes, _)) = &got {
            self.script.fetched.lock().unwrap().push((key.to_string(), bytes.len()));
        }
        Ok(got)
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
        let keys = self.inner.list(prefix)?;
        self.script.lists.lock().unwrap().push((prefix.to_string(), keys.len()));
        Ok(keys)
    }

    fn cas_scope(&self) -> CasScope {
        self.inner.cas_scope()
    }
}

/// A bucket wrapper that holds each get and put for `latency`, counting the requests in
/// flight and the most ever in flight at once.
pub struct Slow {
    pub inner: Arc<dyn ObjectStore>,
    pub latency: std::time::Duration,
    pub in_flight: std::sync::atomic::AtomicUsize,
    pub peak: std::sync::atomic::AtomicUsize,
    pub gets: std::sync::atomic::AtomicUsize,
}

impl Slow {
    pub fn new(inner: Arc<dyn ObjectStore>, latency: std::time::Duration) -> Slow {
        Slow { inner, latency, in_flight: Default::default(), peak: Default::default(), gets: Default::default() }
    }

    fn hold<T>(&self, f: impl FnOnce() -> T) -> T {
        use std::sync::atomic::Ordering::SeqCst;
        let now = self.in_flight.fetch_add(1, SeqCst) + 1;
        self.peak.fetch_max(now, SeqCst);
        std::thread::sleep(self.latency);
        let out = f();
        self.in_flight.fetch_sub(1, SeqCst);
        out
    }
}

impl ObjectStore for Slow {
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, String)>, ObjectError> {
        self.gets.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.hold(|| self.inner.get(key))
    }

    fn put(&self, key: &str, bytes: &[u8], condition: Condition) -> Result<Put, ObjectError> {
        self.hold(|| self.inner.put(key, bytes, condition))
    }

    fn delete(&self, key: &str) -> Result<(), ObjectError> {
        self.inner.delete(key)
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, ObjectError> {
        self.inner.list(prefix)
    }
}

/// Seed `bucket` under prefix `team` with `n` small objects of project `research` and a
/// bucket manifest listing them, as a store of `n` committed objects would leave it.
pub fn seed_objects(bucket: &dyn ObjectStore, n: usize) -> Vec<String> {
    use contextful_core::run::journal::sha256_hex;
    use contextful_core::store::sync::{BucketManifest, Entry};
    let mut manifest = BucketManifest::default();
    let mut keys = Vec::with_capacity(n);
    for i in 0..n {
        let key = format!("research/tables/filings/_runs/runs/run-{i:05}/manifest.json");
        let bytes = format!("{{\"run\":{i}}}").into_bytes();
        bucket.put(&format!("team/{key}"), &bytes, Condition::None).unwrap();
        manifest.entries.insert(key.clone(), Entry { sha256: sha256_hex(&bytes), size: bytes.len() as u64, owner: String::new() });
        keys.push(key);
    }
    bucket.put("team/manifest.json", &serde_json::to_vec(&manifest).unwrap(), Condition::None).unwrap();
    keys
}
