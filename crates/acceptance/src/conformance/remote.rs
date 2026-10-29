//! Synthetic PostgreSQL and signed S3 services own one scenario's remote state.

use super::{Result, Running};
use crate::s3::{S3Server, ACCESS_KEY, SECRET_KEY};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub(super) struct Remote {
    _process: Option<Postgres>,
    _root: Option<tempfile::TempDir>,
    _bucket: Option<S3Server>,
    pub environment: Vec<(String, String)>,
    pub config: String,
}

struct Postgres {
    process: Running,
    timeout: Duration,
}

impl Drop for Postgres {
    fn drop(&mut self) {
        if !matches!(self.process.child.try_wait(), Ok(None)) {
            return;
        }
        // PostgreSQL's fast shutdown releases its shared memory before scratch removal.
        let _ = Command::new("/bin/kill")
            .args(["-INT", &self.process.child.id().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let deadline = Instant::now() + self.timeout;
        while Instant::now() < deadline {
            if !matches!(self.process.child.try_wait(), Ok(None)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // Running retains process-group cleanup when fast shutdown exceeds the bound.
    }
}

fn executable(name: &str) -> Result<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
        .and_then(|path| path.canonicalize().ok())
        .ok_or_else(|| format!("ConformanceCapabilityMissing:{name}"))
}

fn command(binary: &Path, root: &Path) -> Command {
    let mut command = Command::new(binary);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root)
        .env("LC_ALL", "C");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
}

/// The D1 and R2 bindings a `cloudflare-d1` run names, each validated before any request.
pub(super) struct D1Bindings {
    pub endpoint: String,
    r2_endpoint: String,
    bucket: String,
    access: String,
    secret: String,
    recovery: String,
}

impl D1Bindings {
    pub fn from_environment() -> Result<Self> {
        let required = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty()).ok_or_else(|| "ConformanceCapabilityMissing:cloudflare-d1".to_string());
        let endpoint = required("CONTEXTFUL_CONFORMANCE_D1_ENDPOINT")?;
        let url = url::Url::parse(&endpoint).map_err(|_| "ConformanceCapabilityMissing:cloudflare-d1")?;
        let host = url.host_str().ok_or("ConformanceCapabilityMissing:cloudflare-d1")?;
        if url.scheme() != "http" || !(host == "contextful.d1" || host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())) || url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
            return Err("ConformanceCapabilityMissing:cloudflare-d1".into());
        }
        let bindings = Self {
            endpoint: endpoint.trim_end_matches('/').into(),
            r2_endpoint: required("CONTEXTFUL_CONFORMANCE_R2_ENDPOINT")?,
            bucket: required("CONTEXTFUL_CONFORMANCE_R2_BUCKET")?,
            access: required("CONTEXTFUL_CONFORMANCE_R2_ACCESS_KEY_ID")?,
            secret: required("CONTEXTFUL_CONFORMANCE_R2_SECRET_ACCESS_KEY")?,
            recovery: required("CONTEXTFUL_CONFORMANCE_RECOVERY_ID")?,
        };
        if required("CONTEXTFUL_CONFORMANCE_D1_DISPOSABLE")?.as_str() != "1" {
            return Err("ConformanceCapabilityMissing:disposable-d1-binding".into());
        }
        Ok(bindings)
    }
}

/// Seconds a suite's D1 admission lease outlives its last renewal (`assurance.consumer.d1-admission`).
const ADMISSION_TTL_SECS: u64 = 120;
const ADMISSION_RENEWAL: Duration = Duration::from_secs(20);

/// One suite's lease on a shared D1 database, renewed while it lives and released on drop,
/// so a suite in any other container refuses before resetting the rows this one drives.
pub(super) struct Admission {
    endpoint: String,
    holder: String,
    lost: Arc<AtomicBool>,
    stop: Arc<(Mutex<bool>, Condvar)>,
    renewal: Option<std::thread::JoinHandle<()>>,
}

fn d1_sql(endpoint: &str, sql: &str, params: Vec<serde_json::Value>) -> Result<serde_json::Value> {
    let bytes = serde_json::to_vec(&serde_json::json!({"sql":sql,"params":params})).map_err(|_| "ConformanceD1BindingUnreadable")?;
    // A bounded request keeps a stalled renewal from holding the suite's exit.
    let agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30))).build().new_agent();
    let mut response = agent.post(&format!("{endpoint}/sql")).header("content-type", "application/json")
        .send(bytes.as_slice()).map_err(|_| "ConformanceD1BindingUnavailable")?;
    if response.status() != 200 { return Err("ConformanceD1BindingUnavailable".into()) }
    let body = response.body_mut().read_to_vec().map_err(|_| "ConformanceD1BindingUnreadable")?;
    serde_json::from_slice(&body).map_err(|_| "ConformanceD1BindingUnreadable".into())
}

impl Admission {
    /// Take the lease, or refuse with `ConformanceAlreadyRunning` while another holder's lives.
    pub fn acquire(endpoint: &str) -> Result<Self> {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let holder = format!("{}-{nanos}", std::process::id());
        // The table sits outside the `contextful_` prefix every recovery check enumerates.
        d1_sql(endpoint, "CREATE TABLE IF NOT EXISTS conformance_admission (id INTEGER PRIMARY KEY CHECK (id = 1), holder TEXT NOT NULL, expires_at INTEGER NOT NULL)", vec![])?;
        let taken = d1_sql(endpoint, "INSERT INTO conformance_admission (id, holder, expires_at) VALUES (1, ?, unixepoch() + ?) ON CONFLICT (id) DO UPDATE SET holder = excluded.holder, expires_at = excluded.expires_at WHERE conformance_admission.expires_at <= unixepoch()",
            vec![serde_json::json!(holder), serde_json::json!(ADMISSION_TTL_SECS)])?;
        if taken["changes"] != 1 {
            return Err("ConformanceAlreadyRunning".into());
        }
        let lost = Arc::new(AtomicBool::new(false));
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let renewal = {
            let (endpoint, holder, lost, stop) = (endpoint.to_owned(), holder.clone(), lost.clone(), stop.clone());
            std::thread::spawn(move || {
                let (stopped, wake) = &*stop;
                loop {
                    if *wake.wait_timeout(stopped.lock().unwrap(), ADMISSION_RENEWAL).unwrap().0 { return }
                    let renewed = d1_sql(&endpoint, "UPDATE conformance_admission SET expires_at = unixepoch() + ? WHERE id = 1 AND holder = ?",
                        vec![serde_json::json!(ADMISSION_TTL_SECS), serde_json::json!(holder)]);
                    // A failed request retries at the next renewal; a lease another holder took is lost.
                    if renewed.is_ok_and(|answer| answer["changes"] == 0) {
                        lost.store(true, Ordering::SeqCst);
                    }
                }
            })
        };
        Ok(Self { endpoint: endpoint.into(), holder, lost, stop, renewal: Some(renewal) })
    }

    /// Whether this suite still holds the lease.
    pub fn held(&self) -> bool {
        !self.lost.load(Ordering::SeqCst)
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        *self.stop.0.lock().unwrap() = true;
        self.stop.1.notify_all();
        if let Some(renewal) = self.renewal.take() {
            let _ = renewal.join();
        }
        let _ = d1_sql(&self.endpoint, "DELETE FROM conformance_admission WHERE id = 1 AND holder = ?", vec![serde_json::json!(self.holder)]);
    }
}

impl Remote {
    pub fn start(timeout: Duration) -> Result<Self> {
        let initdb = executable("initdb")?;
        let postgres = executable("postgres")?;
        let root = tempfile::tempdir().map_err(|_| "ConformanceScratchUnavailable")?;
        let data = root.path().join("postgres");
        let mut initialize = command(&initdb, root.path());
        initialize
            .args([
                "--no-locale",
                "--encoding=UTF8",
                "--auth=trust",
                "-U",
                "conformance",
                "-D",
            ])
            .arg(&data);
        let mut init = Running::spawn(initialize)?;
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = init
                .child
                .try_wait()
                .map_err(|_| "ConformanceProcessUnavailable")?
            {
                if !status.success() {
                    return Err("ConformanceCapabilityMissing:postgres-fixture".into());
                }
                break;
            }
            if Instant::now() >= deadline {
                return Err("ConformanceProcessTimeout".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|_| "ConformanceFixtureNetworkUnavailable")?;
        let port = listener
            .local_addr()
            .map_err(|_| "ConformanceFixtureNetworkUnavailable")?
            .port();
        drop(listener);
        let mut server = command(&postgres, root.path());
        server
            .arg("-D")
            .arg(&data)
            .args(["-h", "127.0.0.1", "-p", &port.to_string(), "-k"])
            .arg(root.path())
            .arg("-F");
        let mut process = Postgres {
            process: Running::spawn(server)?,
            timeout: timeout.min(Duration::from_secs(5)),
        };
        loop {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            if process
                .process
                .child
                .try_wait()
                .map_err(|_| "ConformanceProcessUnavailable")?
                .is_some()
            {
                return Err("ConformanceCapabilityMissing:postgres-fixture".into());
            }
            if Instant::now() >= deadline {
                return Err("ConformanceProcessTimeout".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let bucket = S3Server::start("conformance");
        let config = format!("[node]\nid = \"conformance\"\n[sync]\nendpoint = \"{}\"\nbucket = \"conformance\"\nprefix = \"consumer\"\ncoordination = \"cas\"\npull_before_run = true\npush_after_run = true\naccess_key_id = \"env://CONTEXTFUL_SYNC_ACCESS_KEY_ID\"\nsecret_access_key = \"env://CONTEXTFUL_SYNC_SECRET_ACCESS_KEY\"\n", bucket.endpoint);
        let environment = vec![
            ("CONTEXTFUL_STORAGE_PROFILE".into(), "postgres".into()),
            (
                "CONTEXTFUL_CATALOG_URL".into(),
                format!("postgres://conformance@127.0.0.1:{port}/postgres?sslmode=disable"),
            ),
            (
                "CONTEXTFUL_RECOVERY_ID".into(),
                "synthetic-consumer-v1".into(),
            ),
            ("CONTEXTFUL_SYNC_ACCESS_KEY_ID".into(), ACCESS_KEY.into()),
            (
                "CONTEXTFUL_SYNC_SECRET_ACCESS_KEY".into(),
                SECRET_KEY.into(),
            ),
        ];
        Ok(Self {
            _process: Some(process),
            _root: Some(root),
            _bucket: Some(bucket),
            environment,
            config,
        })
    }

    /// A disposable D1 binding and an R2 bucket supply one replacement scenario.
    pub fn start_d1(root: &Path) -> Result<Self> {
        let D1Bindings { endpoint, r2_endpoint, bucket, access, secret, recovery } = D1Bindings::from_environment()?;
        let endpoint = endpoint.as_str();
        let query = |sql: &str| -> Result<serde_json::Value> {
            let payload = serde_json::json!({"sql":sql,"params":[]});
            let bytes = serde_json::to_vec(&payload).map_err(|_| "ConformanceD1BindingUnreadable")?;
            let mut response = ureq::post(&format!("{endpoint}/sql")).header("content-type", "application/json").send(bytes.as_slice()).map_err(|_| "ConformanceD1BindingUnavailable")?;
            if response.status() != 200 { return Err("ConformanceD1BindingUnavailable".into()) }
            let body = response.body_mut().read_to_vec().map_err(|_| "ConformanceD1BindingUnreadable")?;
            serde_json::from_slice(&body).map_err(|_| "ConformanceD1BindingUnreadable".into())
        };
        let tables = query("SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'contextful_%'")?;
        let has_recovery = tables["rows"].as_array().is_some_and(|rows| rows.iter().any(|row| row["name"] == "contextful_recovery"));
        if has_recovery {
            let has_marker = tables["rows"].as_array().is_some_and(|rows| rows.iter().any(|row| row["name"] == "contextful_conformance"));
            if !has_marker { return Err("ConformanceCapabilityMissing:disposable-d1-binding".into()) }
            let marker = query("SELECT generation FROM contextful_conformance WHERE id = 1")?;
            if marker["rows"][0]["generation"].as_str() != Some(recovery.as_str()) {
                return Err("ConformanceCapabilityMissing:disposable-d1-binding".into());
            }
            let bound = query("SELECT project, generation FROM contextful_recovery WHERE id = 1")?;
            if bound["rows"][0]["project"] != "research" || bound["rows"][0]["generation"] != recovery {
                return Err("ConformanceRecoveryMismatch".into());
            }
            // A schema-2 binding gains the publication table, and a schema-3 one the scoped
            // guard tables, at `storage initialize`.
            let present = |table: &str| tables["rows"].as_array().is_some_and(|rows| rows.iter().any(|row| row["name"] == table));
            let deletes: Vec<_> = ["contextful_guard", "contextful_callback_guard", "contextful_guard_holder", "contextful_guard_reconciliation", "contextful_reconciliation", "contextful_export", "contextful_publication", "contextful_awakeable", "contextful_sweep", "contextful_blob", "contextful_journal", "contextful_run", "contextful_scope", "contextful_lease"].into_iter()
                .filter(|table| !matches!(*table, "contextful_publication" | "contextful_callback_guard" | "contextful_guard_holder" | "contextful_guard_reconciliation") || present(table))
                .map(|table| serde_json::json!({"sql":format!("DELETE FROM {table}"),"params":[]})).collect();
            let bytes = serde_json::to_vec(&deletes).map_err(|_| "ConformanceStorageResetFailed")?;
            let response = ureq::post(&format!("{endpoint}/batch")).header("content-type", "application/json").send(bytes.as_slice()).map_err(|_| "ConformanceStorageResetFailed")?;
            if response.status() != 200 { return Err("ConformanceStorageResetFailed".into()) }
        } else if !tables["rows"].as_array().is_some_and(Vec::is_empty) {
            return Err("ConformanceRecoveryMismatch".into());
        }
        let prefix = root.file_name().and_then(|n| n.to_str()).ok_or("ConformanceScratchUnavailable")?;
        let config = format!("[node]\nid = \"conformance\"\n[sync]\nendpoint = \"{r2_endpoint}\"\nbucket = \"{bucket}\"\nprefix = \"consumer-d1/{prefix}\"\ncoordination = \"cas\"\npull_before_run = true\npush_after_run = true\naccess_key_id = \"env://CONTEXTFUL_SYNC_ACCESS_KEY_ID\"\nsecret_access_key = \"env://CONTEXTFUL_SYNC_SECRET_ACCESS_KEY\"\n");
        Ok(Self { _process: None, _root: None, _bucket: None,
            environment: vec![
                ("CONTEXTFUL_STORAGE_PROFILE".into(), "cloudflare-d1".into()),
                ("CONTEXTFUL_D1_ENDPOINT".into(), endpoint.into()),
                ("CONTEXTFUL_RECOVERY_ID".into(), recovery),
                ("CONTEXTFUL_SYNC_ACCESS_KEY_ID".into(), access),
                ("CONTEXTFUL_SYNC_SECRET_ACCESS_KEY".into(), secret),
            ], config })
    }

    /// A D1 database receives its disposable marker after initialization succeeds.
    pub fn mark_disposable(&self) -> Result<()> {
        let Some(endpoint) = self.environment.iter().find(|(key, _)| key == "CONTEXTFUL_D1_ENDPOINT").map(|(_, value)| value) else { return Ok(()) };
        let generation = self.environment.iter().find(|(key, _)| key == "CONTEXTFUL_RECOVERY_ID").map(|(_, value)| value).ok_or("ConformanceRecoveryMismatch")?;
        let query = |sql: &str, params: Vec<serde_json::Value>| -> Result<serde_json::Value> {
            let bytes = serde_json::to_vec(&serde_json::json!({"sql":sql,"params":params})).map_err(|_| "ConformanceD1BindingUnreadable")?;
            let mut response = ureq::post(&format!("{endpoint}/sql")).header("content-type", "application/json")
                .send(bytes.as_slice()).map_err(|_| "ConformanceD1BindingUnavailable")?;
            if response.status() != 200 { return Err("ConformanceD1BindingUnavailable".into()) }
            let body = response.body_mut().read_to_vec().map_err(|_| "ConformanceD1BindingUnreadable")?;
            serde_json::from_slice(&body).map_err(|_| "ConformanceD1BindingUnreadable".into())
        };
        query("CREATE TABLE IF NOT EXISTS contextful_conformance (id INTEGER PRIMARY KEY CHECK (id = 1), generation TEXT NOT NULL)", vec![])?;
        query("INSERT OR IGNORE INTO contextful_conformance (id, generation) VALUES (1, ?)", vec![serde_json::json!(generation)])?;
        let marked = query("SELECT generation FROM contextful_conformance WHERE id = 1", vec![])?;
        if marked["rows"][0]["generation"].as_str() != Some(generation.as_str()) { return Err("ConformanceRecoveryMismatch".into()) }
        Ok(())
    }
}
