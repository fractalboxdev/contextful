//! Synthetic PostgreSQL and signed S3 services own one scenario's remote state.

use super::{Result, Running};
use crate::s3::{S3Server, ACCESS_KEY, SECRET_KEY};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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
        let required = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty()).ok_or_else(|| "ConformanceCapabilityMissing:cloudflare-d1".to_string());
        let endpoint = required("CONTEXTFUL_CONFORMANCE_D1_ENDPOINT")?;
        let url = url::Url::parse(&endpoint).map_err(|_| "ConformanceCapabilityMissing:cloudflare-d1")?;
        let host = url.host_str().ok_or("ConformanceCapabilityMissing:cloudflare-d1")?;
        if url.scheme() != "http" || !(host == "contextful.d1" || host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())) || url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
            return Err("ConformanceCapabilityMissing:cloudflare-d1".into());
        }
        let r2_endpoint = required("CONTEXTFUL_CONFORMANCE_R2_ENDPOINT")?;
        let bucket = required("CONTEXTFUL_CONFORMANCE_R2_BUCKET")?;
        let access = required("CONTEXTFUL_CONFORMANCE_R2_ACCESS_KEY_ID")?;
        let secret = required("CONTEXTFUL_CONFORMANCE_R2_SECRET_ACCESS_KEY")?;
        let recovery = required("CONTEXTFUL_CONFORMANCE_RECOVERY_ID")?;
        if required("CONTEXTFUL_CONFORMANCE_D1_DISPOSABLE")?.as_str() != "1" {
            return Err("ConformanceCapabilityMissing:disposable-d1-binding".into());
        }
        let endpoint = endpoint.trim_end_matches('/');
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
            let deletes: Vec<_> = ["contextful_guard", "contextful_reconciliation", "contextful_export", "contextful_awakeable", "contextful_sweep", "contextful_blob", "contextful_journal", "contextful_run", "contextful_scope", "contextful_lease"].into_iter()
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
