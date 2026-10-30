//! A connector manifest's `[auth]` refresh through the built binary: a component source whose
//! access token the host refreshes against a loopback token endpoint, writing each replacement
//! refresh token forward to the `file` backend.
#![cfg(feature = "component-host")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

const PROBE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../contextful-wasm/tests/fixtures/probe.wasm");

/// One request a loopback server received: its target, lower-cased headers and body.
#[derive(Clone, Debug)]
struct Seen {
    headers: Vec<String>,
    body: String,
}

impl Seen {
    fn form(&self, key: &str) -> Option<String> {
        url::form_urlencoded::parse(self.body.as_bytes()).find(|(k, _)| k == key).map(|(_, v)| v.into_owned())
    }

    fn bearer(&self) -> Option<&str> {
        self.headers.iter().find_map(|h| h.strip_prefix("authorization: bearer "))
    }
}

/// A loopback server answering each request with `handler`'s status and body.
struct Loopback {
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Loopback {
    fn start(handler: impl Fn(&Seen) -> (u16, String) + Send + Sync + 'static) -> Loopback {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let log = seen.clone();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let mut headers = Vec::new();
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                        break;
                    }
                    headers.push(h.trim().to_ascii_lowercase());
                }
                let length = headers.iter().find_map(|h| h.strip_prefix("content-length: ")).and_then(|v| v.parse().ok()).unwrap_or(0);
                let mut body = vec![0; length];
                let _ = reader.read_exact(&mut body);
                let request = Seen { headers, body: String::from_utf8_lossy(&body).to_string() };
                let (status, answer) = handler(&request);
                log.lock().unwrap().push(request);
                let _ = write!(stream, "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}", answer.len());
            }
        });
        Loopback { port, seen }
    }

    fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

/// A token endpoint rotating the refresh token on every grant: `refresh-<n>` with the client
/// secret answers access token `access-<n+1>` and replacement `refresh-<n+1>`.
fn token_endpoint() -> Loopback {
    Loopback::start(|r| {
        let n = r.form("refresh_token").and_then(|t| t.strip_prefix("refresh-").and_then(|n| n.parse::<u32>().ok()));
        match n {
            Some(n) if r.form("client_secret").as_deref() == Some("client-secret") && r.form("grant_type").as_deref() == Some("refresh_token") => {
                (200, format!(r#"{{"access_token":"access-{}","refresh_token":"refresh-{}","expires_in":3600}}"#, n + 1, n + 1))
            }
            _ => (400, r#"{"error":"invalid_grant"}"#.into()),
        }
    })
}

/// A project whose component source reads `fetch <vendor>/v1` under the probe, attaching the
/// `[auth]` access token, beside a secrets directory holding the client secret and `refresh-0`.
fn project(token: &Loopback, vendor: &Loopback) -> (tempfile::TempDir, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(".contextful/context/research");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    std::fs::create_dir_all(dir.path().join("connectors")).unwrap();
    std::fs::copy(PROBE, dir.path().join("connectors/probe.wasm")).unwrap();
    let connector = format!(
        "name = \"probe\"\n[capabilities]\nallow_hosts = [\"127.0.0.1\"]\nclock = true\n\n[auth]\ntoken_endpoint = \"http://127.0.0.1:{}/oauth/token\"\nclient_id = \"probe-reader\"\n\
         client_secret = \"${{secret://vendor-client-secret}}\"\nrefresh_token = \"${{secret://vendor-refresh}}\"\naccess_token = \"${{secret://vendor-token}}\"\nscope = \"records.read\"\n",
        token.port
    );
    std::fs::write(dir.path().join("connector.toml"), connector).unwrap();
    let pipeline = format!(
        "authoring_posture = \"per_request\"\n[[pipeline]]\nid = \"probe\"\ntables = [\"fetch http://127.0.0.1:{}/v1\"]\n\n[pipeline.source]\nname = \"connectors/probe.wasm\"\n\n\
         [pipeline.source.config]\nmanifest = \"connector.toml\"\nallow = [\"127.0.0.1\"]\nattach = {{ Authorization = \"Bearer ${{secret://vendor-token}}\" }}\n",
        vendor.port
    );
    std::fs::write(dir.path().join("contextful.toml"), pipeline).unwrap();
    let secrets = tempfile::tempdir().unwrap();
    std::fs::write(secrets.path().join("vendor-client-secret"), "client-secret").unwrap();
    std::fs::write(secrets.path().join("vendor-refresh"), "refresh-0").unwrap();
    (dir, secrets)
}

fn fire(dir: &Path, env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["pipeline", "run", "probe", "--project", "research", "--run-id", "run-1", "--site-id", "site-a", "--now", "2030-01-01T00:00:00Z"])
        .current_dir(dir)
        .env_remove("CONTEXTFUL_NODE_ID")
        .env_remove("CONTEXTFUL_SECRETS_BACKEND")
        .env_remove("CONTEXTFUL_SECRETS_DIR")
        .env_remove("CONTEXTFUL_COMPONENT_TARGET")
        .envs(env.iter().copied())
        .output()
        .unwrap()
}

fn file_backend(secrets: &Path) -> [(&'static str, String); 2] {
    [("CONTEXTFUL_SECRETS_BACKEND", "file".to_string()), ("CONTEXTFUL_SECRETS_DIR", secrets.to_str().unwrap().to_string())]
}

fn env<'a>(pairs: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    pairs.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

/// Output text with every token value the endpoints issue checked absent.
fn tokenless(out: &Output) -> String {
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    for token in ["access-1", "access-2", "refresh-0", "refresh-1", "refresh-2", "client-secret"] {
        assert!(!text.contains(token), "the run printed `{token}`: {text}");
    }
    text
}

/// The vendor rejects the first access token as expired; the run refreshes it once, writes the
/// replacement refresh token forward, resends the request and lands its row.
// spec: connector.rotate.oauth-credential@391c7c68
// spec: connector.rotate.rejected-once@161fc964
// spec: connector.rotate.auth-policy@a81bc2d3
// spec: connector.rotate.access-token@69382394
#[test]
fn an_expired_access_token_refreshes_once_and_the_run_lands_its_rows() {
    let token = token_endpoint();
    let vendor = Loopback::start(|r| if r.bearer() == Some("access-2") { (200, "ok".into()) } else { (401, "expired".into()) });
    let (dir, secrets) = project(&token, &vendor);
    let out = fire(dir.path(), &env(&file_backend(secrets.path())));
    let text = tokenless(&out);
    assert!(out.status.success() && text.contains("success · 1 rows"), "{text}");
    let bearers: Vec<Option<String>> = vendor.requests().iter().map(|r| r.bearer().map(str::to_string)).collect();
    assert_eq!(bearers, [Some("access-1".to_string()), Some("access-2".to_string())]);
    let grants = token.requests();
    assert_eq!(grants.iter().map(|g| g.form("refresh_token")).collect::<Vec<_>>(), [Some("refresh-0".to_string()), Some("refresh-1".to_string())]);
    assert_eq!(grants[0].form("scope").as_deref(), Some("records.read"));
    assert_eq!(grants[0].form("client_id").as_deref(), Some("probe-reader"));
    assert_eq!(std::fs::read_to_string(secrets.path().join("vendor-refresh")).unwrap(), "refresh-2", "each replacement is written forward");
}

/// A refused put of the replacement refresh token fails the run, and no request carries the
/// refreshed access token; an adapter with no write half fails it before any grant.
// spec: connector.rotate.write-back-denied@13fed6fb
#[test]
fn a_refused_write_back_fails_the_run() {
    let token = token_endpoint();
    let vendor = Loopback::start(|_| (200, "ok".into()));
    let (dir, secrets) = project(&token, &vendor);
    let path = secrets.path().join("vendor-refresh");
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&path, perms).unwrap();
    let out = fire(dir.path(), &env(&file_backend(secrets.path())));
    let text = tokenless(&out);
    assert!(!out.status.success() && text.contains("SecretRotationWriteDenied"), "{text}");
    assert!(vendor.requests().is_empty(), "a request carried the refreshed token: {:?}", vendor.requests());
    assert!(!token.requests().is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "refresh-0");

    // The environment adapter answers the refresh token and has no write half.
    let quiet = token_endpoint();
    let (dir, _secrets) = project(&quiet, &vendor);
    let pairs = [("VENDOR_REFRESH", "refresh-0".to_string()), ("VENDOR_CLIENT_SECRET", "client-secret".to_string()), ("CONTEXTFUL_SECRETS_ALLOW_ENV_TEMPLATES", "1".to_string())];
    let out = fire(dir.path(), &env(&pairs));
    let text = tokenless(&out);
    assert!(!out.status.success() && text.contains("SecretRotationWriteDenied") && text.contains("`env`"), "{text}");
    assert!(quiet.requests().is_empty(), "a refresh ran with no write half");
    assert!(vendor.requests().is_empty());
}

/// A refresh is one token request per run attempt: a `503` from the token endpoint is retried
/// by the run's default schedule alone and fails the run with the vendor unreached, and a vendor
/// rejecting every token sees the request twice under one refresh.
// spec: connector.rotate.one-retry-layer@7def2c94
#[test]
fn each_refresh_stays_under_one_retry_layer() {
    let down = Loopback::start(|_| (503, "{}".into()));
    let vendor = Loopback::start(|_| (200, "ok".into()));
    let (dir, secrets) = project(&down, &vendor);
    let out = fire(dir.path(), &env(&file_backend(secrets.path())));
    let text = tokenless(&out);
    assert!(!out.status.success() && text.contains("503"), "{text}");
    let attempts = contextful_core::run::retry::DEFAULT_ATTEMPTS as usize;
    assert_eq!(down.requests().len(), attempts, "one token request per attempt of the run's schedule, none retried below it");
    assert!(vendor.requests().is_empty());

    let token = token_endpoint();
    let rejecting = Loopback::start(|_| (401, "expired".into()));
    let (dir, secrets) = project(&token, &rejecting);
    let out = fire(dir.path(), &env(&file_backend(secrets.path())));
    tokenless(&out);
    assert_eq!(token.requests().len(), 2, "one grant to start and one refresh on the rejection");
    assert_eq!(rejecting.requests().len(), 2, "the rejected request went out once more, then returned to the guest");
}
