//! Synthetic PostgreSQL and signed S3 services own one scenario's remote state.

use super::{Result, Running};
use crate::s3::{S3Server, ACCESS_KEY, SECRET_KEY};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

pub(super) struct Remote {
    _process: Running,
    _root: tempfile::TempDir,
    _bucket: S3Server,
    pub environment: Vec<(String, String)>,
    pub config: String,
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
        let mut process = Running::spawn(server)?;
        loop {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            if process
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
            (
                "CONTEXTFUL_STORAGE_PROFILE".into(),
                "cloudflare-postgres".into(),
            ),
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
            _process: process,
            _root: root,
            _bucket: bucket,
            environment,
            config,
        })
    }
}
