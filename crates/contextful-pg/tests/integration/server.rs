//! An ephemeral Postgres cluster per test: `initdb` into a temporary directory, then
//! `postgres` listening on a Unix socket inside it alone, stopped when the guard drops.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// The variable naming a running server as a key-value connection string.
const SERVER_VAR: &str = "CONTEXTFUL_TEST_PG";

/// A server one test owns, and the database it created on it.
pub struct Server {
    child: Option<Child>,
    data: Option<PathBuf>,
    _dir: Option<tempfile::TempDir>,
    /// The connection string of the test's own database.
    pub conninfo: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        if let (Some(child), Some(data)) = (self.child.as_mut(), self.data.as_ref()) {
            let stopped = Command::new("pg_ctl").arg("-D").arg(data).args(["stop", "-m", "immediate", "-w"]).stdout(Stdio::null()).stderr(Stdio::null()).status();
            if !stopped.is_ok_and(|s| s.success()) {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}

fn on_path(bin: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| std::env::split_paths(&paths).map(|d| d.join(bin)).find(|p| p.is_file()))
}

#[cfg(unix)]
fn root(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let probe = dir.join("uid");
    std::fs::write(&probe, b"").is_ok_and(|_| std::fs::metadata(&probe).is_ok_and(|m| m.uid() == 0))
}

#[cfg(not(unix))]
fn root(_dir: &Path) -> bool {
    false
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A fresh database on `base`, named uniquely within this process.
fn create_database(base: &str) -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let name = format!("t{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst));
    let mut admin = postgres::Client::connect(base, postgres::NoTls).unwrap_or_else(|e| panic!("{SERVER_VAR}: {e}"));
    admin.batch_execute(&format!("CREATE DATABASE {name}")).unwrap();
    format!("{base} dbname={name}")
}

/// A server for one test, or `None` with the reason printed when this host has none.
pub fn start() -> Option<Server> {
    if let Some(base) = std::env::var(SERVER_VAR).ok().filter(|s| !s.trim().is_empty()) {
        let conninfo = create_database(&base);
        return Some(Server { child: None, data: None, _dir: None, conninfo });
    }
    let (Some(initdb), Some(postgres)) = (on_path("initdb"), on_path("postgres")) else {
        eprintln!("skipped: no `initdb` and `postgres` on PATH and no {SERVER_VAR}");
        return None;
    };
    let dir = tempfile::tempdir().unwrap();
    if root(dir.path()) {
        eprintln!("skipped: Postgres refuses to run as root and {SERVER_VAR} is unset");
        return None;
    }
    let data = dir.path().join("data");
    let init = Command::new(initdb)
        .arg("-D")
        .arg(&data)
        .args(["-U", "contextful", "--auth=trust", "-E", "UTF8", "--no-sync"])
        .output()
        .unwrap();
    assert!(init.status.success(), "initdb: {}", String::from_utf8_lossy(&init.stderr));
    let port = free_port();
    let child = Command::new(postgres)
        .arg("-D")
        .arg(&data)
        .arg("-k")
        .arg(dir.path())
        .args(["-p", &port.to_string(), "-c", "listen_addresses=", "-c", "fsync=off"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let base = format!("host={} port={port} user=contextful dbname=postgres", dir.path().display());
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut server = Server { child: Some(child), data: Some(data), _dir: None, conninfo: String::new() };
    loop {
        match postgres::Client::connect(&base, postgres::NoTls) {
            Ok(_) => break,
            Err(e) if Instant::now() > deadline => panic!("postgres did not accept connections: {e}"),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    server.conninfo = create_database(&base);
    server._dir = Some(dir);
    Some(server)
}
