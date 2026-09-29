//! `contextful pipeline run` over the Google Drive source, against a recorded fake of the Drive
//! API, with PDF bodies decoding in the binary's own worker behind the process boundary.
#![cfg(feature = "drive")]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

const ACCESS: &str = "ya29.fixture-access-7Qx";
const REFRESH: &str = "1//refresh-fixture-Zq8";
const CLIENT_SECRET: &str = "GOCSPX-fixture-secret";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../contextful-connectors/tests/fixtures/drive")
}

fn unescape(s: &str) -> String {
    let (bytes, mut out, mut i) = (s.as_bytes(), Vec::new(), 0);
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap_or(b'?'));
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// A recorded fake of the Drive API answering from `recording.json`, then from each overlay
/// pushed after it; it records each request's path.
struct Fake {
    port: u16,
    recordings: Arc<Mutex<Vec<&'static str>>>,
    paths: Arc<Mutex<Vec<String>>>,
}

impl Fake {
    fn start() -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let recordings = Arc::new(Mutex::new(vec!["recording.json"]));
        let paths: Arc<Mutex<Vec<String>>> = Arc::default();
        let (r, seen) = (recordings.clone(), paths.clone());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut parts = line.split_whitespace();
                let (method, target) = (parts.next().unwrap_or_default().to_string(), parts.next().unwrap_or_default().to_string());
                let (mut len, mut bearer) = (0usize, false);
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap() == 0 || h.trim().is_empty() {
                        break;
                    }
                    let lower = h.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                    bearer |= h.trim() == format!("authorization: Bearer {ACCESS}") || h.trim() == format!("Authorization: Bearer {ACCESS}");
                }
                let mut body = vec![0; len];
                reader.read_exact(&mut body).unwrap();
                let (path, q) = target.split_once('?').unwrap_or((target.as_str(), ""));
                seen.lock().unwrap().push(path.to_string());
                let asked: Vec<(String, String)> = q.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (unescape(k), unescape(v))).collect();
                let mut answer = (404u16, b"{}".to_vec());
                if path != "/token" && !bearer {
                    answer = (401, b"{}".to_vec());
                } else {
                    'found: for name in r.lock().unwrap().iter().rev() {
                        let recording: Value = serde_json::from_slice(&std::fs::read(fixtures().join(name)).unwrap()).unwrap();
                        for x in recording["exchanges"].as_array().unwrap() {
                            let wanted = x["query"].as_object().cloned().unwrap_or_default();
                            if x["method"] == method.as_str()
                                && x["path"] == path
                                && wanted.iter().all(|(k, v)| asked.iter().any(|(a, b)| a == k && Some(b.as_str()) == v.as_str()))
                                && (wanted.contains_key("pageToken") || !asked.iter().any(|(a, _)| a == "pageToken"))
                            {
                                let body = match x.get("body_file") {
                                    Some(f) => std::fs::read(fixtures().join(f.as_str().unwrap())).unwrap(),
                                    None => serde_json::to_vec(&x["body"]).unwrap(),
                                };
                                answer = (x["status"].as_u64().unwrap() as u16, body);
                                break 'found;
                            }
                        }
                    }
                }
                let _ = write!(stream, "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", answer.0, answer.1.len());
                let _ = stream.write_all(&answer.1);
            }
        });
        Fake { port, recordings, paths }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn count(&self, path: &str) -> usize {
        self.paths.lock().unwrap().iter().filter(|p| *p == path).count()
    }
}

fn project(fake: &Fake) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(".contextful/context/research");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    let manifest = format!(
        "[[pipeline]]\nid = \"team\"\n\n[[pipeline.tables]]\nname = \"files\"\nprimary_key = [\"file_id\"]\n\n[[pipeline.tables]]\nname = \"pages\"\nprimary_key = [\"file_id\", \"page\"]\n\n\
         [pipeline.source]\nname = \"drive\"\n\n[pipeline.source.config]\nfolder_id = \"root-f\"\napi_base = \"{}\"\ntoken_url = \"{}\"\n\n\
         [pipeline.source.config.oauth]\nrefresh_token = \"${{secret://drive-refresh}}\"\nclient_id = \"${{secret://drive-client-id}}\"\nclient_secret = \"${{secret://drive-client-secret}}\"\n",
        fake.url(""),
        fake.url("/token")
    );
    std::fs::write(dir.path().join("contextful.toml"), manifest).unwrap();
    dir
}

fn cf(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(args)
        .current_dir(dir)
        .env_remove("CONTEXTFUL_NODE_ID")
        .env_remove("CONTEXTFUL_SECRETS_BACKEND")
        .env("CONTEXTFUL_SECRETS_ALLOW_ENV_TEMPLATES", "1")
        .env("DRIVE_REFRESH", REFRESH)
        .env("DRIVE_CLIENT_ID", "client-7.apps.example")
        .env("DRIVE_CLIENT_SECRET", CLIENT_SECRET)
        .output()
        .unwrap()
}

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn fire(dir: &Path, run: &str, now: &str) -> String {
    ok(&cf(dir, &["pipeline", "run", "team", "--project", "research", "--run-id", run, "--site-id", "site-a", "--now", now]))
}

fn query(dir: &Path, sql: &str) -> Value {
    serde_json::from_str(&ok(&cf(dir, &["query", "--json", "--project", "research", sql]))).unwrap()
}

/// The count of files under `dir` whose bytes hold `needle`.
fn files_holding(dir: &Path, needle: &str) -> usize {
    let mut n = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            if e.path().is_dir() {
                stack.push(e.path());
            } else if String::from_utf8_lossy(&std::fs::read(e.path()).unwrap()).contains(needle) {
                n += 1;
            }
        }
    }
    n
}

/// One fire shares one minted token, one walk and one read of each file's bytes across the `files` and `pages`
/// tables.
///
/// Through the binary: a three-level tree lands one file row per file with its path, a Doc, a Sheet and a deck as
/// PDF pages decoded in the worker, a file over the cap skipped by name; a second fire re-lands the changed file
/// alone, and no token reaches the journal, the run record or a landed file.
// spec: connector.source.drive-fire@010f001b
#[test]
fn a_drive_fire_lands_files_and_pages_then_only_what_changed() {
    let fake = Fake::start();
    let dir = project(&fake);
    let out = fire(dir.path(), "run-1", "2031-03-01T00:00:00Z");
    assert!(out.contains("team_files: run-1.team_files success · 7 rows"), "{out}");
    assert!(out.contains("team_pages: run-1.team_pages success · 7 rows"), "{out}");
    assert_eq!(fake.count("/token"), 1, "one mint serves the fire");
    assert_eq!(fake.count("/drive/v3/files/doc-plan/export"), 1, "both tables share one export");

    let files = query(dir.path(), "SELECT file_id, path, skipped IS NOT NULL AS skipped FROM team_files ORDER BY path");
    assert_eq!(
        files["rows"],
        json!([
            ["short-deck", "Deck shortcut", true],
            ["slides-deck", "Finance/Board/Deck", false],
            ["bin-video", "Finance/Board/all-hands.mp4", true],
            ["pdf-report", "Finance/Board/report.pdf", false],
            ["sheet-budget", "Finance/Budget", false],
            ["form-intake", "Intake", true],
            ["doc-plan", "Plan", false],
        ])
    );
    let pages = query(dir.path(), "SELECT file_id, page, text FROM team_pages ORDER BY file_id, page");
    assert_eq!(
        pages["rows"],
        json!([
            ["doc-plan", "1", "Quarterly plan"],
            ["doc-plan", "2", "Hiring targets"],
            ["pdf-report", "1", "Board report"],
            ["sheet-budget", "1", "Budget 2031"],
            ["slides-deck", "1", "Roadmap"],
            ["slides-deck", "2", "Launch"],
            ["slides-deck", "3", "Risks"],
        ])
    );

    fake.recordings.lock().unwrap().push("second.json");
    let out = fire(dir.path(), "run-2", "2031-03-02T00:00:00Z");
    assert!(out.contains("team_files: run-2.team_files success · 2 rows"), "the edited Plan and the removed report: {out}");
    assert!(out.contains("team_pages: run-2.team_pages success · 3 rows"), "Plan's page, its dropped page, the report's page: {out}");
    assert_eq!(fake.count("/drive/v3/files/sheet-budget/export"), 1, "the unchanged sheet is not read again");

    for secret in [ACCESS, REFRESH, CLIENT_SECRET] {
        assert_eq!(files_holding(&dir.path().join(".contextful"), secret), 0, "`{secret}` persisted");
    }
}

/// `incremental` beside the drive source refuses as {{connector.package.component-position}} at validation.
// spec: connector.source.drive-position-owned@2834aee3
#[test]
fn an_incremental_field_beside_the_drive_source_refuses_at_validation() {
    let fake = Fake::start();
    let dir = project(&fake);
    let text = std::fs::read_to_string(dir.path().join("contextful.toml")).unwrap().replace("id = \"team\"\n", "id = \"team\"\nincremental = \"modified_time\"\n");
    std::fs::write(dir.path().join("contextful.toml"), text).unwrap();
    let out = cf(dir.path(), &["pipeline", "validate"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("ConnectorPositionOwned") && err.contains("drive"), "{err}");
    assert!(fake.paths.lock().unwrap().is_empty());
}
