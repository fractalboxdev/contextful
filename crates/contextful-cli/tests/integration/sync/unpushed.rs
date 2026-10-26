//! A run committed on a disposable disk whose push the bucket never took: the mark the
//! execution database keeps, the push a later command takes before it reads, the push a
//! stopping serve takes, and the status a release step reads.
#![cfg(all(feature = "d1-catalog", feature = "s3-sync"))]

use contextful_acceptance::s3::{S3Server, ACCESS_KEY, SECRET_KEY};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const STORE: &str = ".contextful/context/research";

/// A `cloudflare-d1` project on a fresh disk: the D1 binding, the R2 stand-in, and a run plan
/// landing one row per run.
struct Disk {
    dir: tempfile::TempDir,
    config: String,
    d1: String,
    binding: Child,
    _bridge: tempfile::TempDir,
}

impl Drop for Disk {
    fn drop(&mut self) {
        let _ = self.binding.kill();
        let _ = self.binding.wait();
    }
}

impl Disk {
    fn new(s3: &S3Server) -> Disk {
        let bridge = tempfile::tempdir().unwrap();
        let mut binding = Command::new("node")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../contextful-d1/tests/fixture.mjs"))
            .arg(bridge.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(binding.stdout.take().unwrap()).read_line(&mut line).unwrap();
        let d1 = format!("http://127.0.0.1:{}", line.trim());
        let config = format!(
            "[node]\nid = 'site'\n[sync]\nendpoint = '{}'\nbucket = 'conformance'\nprefix = 'drain'\ncoordination = 'cas'\npull_before_run = true\npush_after_run = true\npush_wait_secs = 1\naccess_key_id = 'env://DRAIN_TEST_ACCESS_KEY'\nsecret_access_key = 'env://DRAIN_TEST_SECRET_KEY'\n",
            s3.endpoint
        );
        let disk = Disk { dir: tempfile::tempdir().unwrap(), config, d1, binding, _bridge: bridge };
        disk.fresh();
        let init = disk.cf(&["storage", "initialize", "--project", "research"]);
        assert!(init.status.success(), "{}", String::from_utf8_lossy(&init.stderr));
        disk
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// Replace the disk: an empty store root holding only its `config.toml`, the execution
    /// database untouched.
    fn fresh(&self) {
        let store = self.root().join(STORE);
        let _ = std::fs::remove_dir_all(self.root().join(".contextful/context"));
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("config.toml"), &self.config).unwrap();
        std::fs::write(self.root().join("contextful.toml"), "authoring_posture = 'per_request'\n[[pipeline.tables]]\nname = 'filings'\n").unwrap();
        std::fs::write(self.root().join("one.sh"), "printf '{\"rows\":[{\"id\":1}],\"more\":false}'\n").unwrap();
        std::fs::write(self.root().join("feed.toml"), "pipeline = 'feed'\ntable = 'filings'\n[connector]\nid = 'vendor'\nversion = '1'\ncommand = ['sh', 'one.sh']\n").unwrap();
    }

    /// Declare the run plan's table as `table`, with `block` appended to its table block.
    fn declare(&self, table: &str, block: &str) {
        std::fs::write(self.root().join("contextful.toml"), format!("authoring_posture = 'per_request'\n[[pipeline.tables]]\nname = '{table}'\n{block}")).unwrap();
        std::fs::write(self.root().join("feed.toml"), format!("pipeline = 'feed'\ntable = '{table}'\n[connector]\nid = 'vendor'\nversion = '1'\ncommand = ['sh', 'one.sh']\n")).unwrap();
    }

    fn command(&self, args: &[&str]) -> Command {
        self.command_via(&self.d1, args)
    }

    /// A command reaching the execution database at `d1`.
    fn command_via(&self, d1: &str, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_contextful"));
        command
            .args(args)
            .current_dir(self.root())
            .env_remove("CONTEXTFUL_NODE_ID")
            .env_remove("CONTEXTFUL_CATALOG_URL")
            .env("CONTEXTFUL_STORAGE_PROFILE", "cloudflare-d1")
            .env("CONTEXTFUL_D1_ENDPOINT", d1)
            .env("CONTEXTFUL_RECOVERY_ID", "generation")
            .env("DRAIN_TEST_ACCESS_KEY", ACCESS_KEY)
            .env("DRAIN_TEST_SECRET_KEY", SECRET_KEY);
        command
    }

    fn cf(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn start(&self, run: &str) -> Output {
        self.cf(&["run", "start", "--plan", "feed.toml", "--project", "research", "--run-id", run, "--site-id", "site"])
    }

    /// `sync status` against the execution database at `d1`: its exit code, its JSON and its
    /// standard error.
    fn status_via(&self, d1: &str) -> (Option<i32>, Value, String) {
        let out = self.command_via(d1, &["sync", "status", "--project", "research"]).output().unwrap();
        let json = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)));
        (out.status.code(), json, String::from_utf8_lossy(&out.stderr).into_owned())
    }

    fn status(&self) -> (Option<i32>, Value, String) {
        self.status_via(&self.d1)
    }

    /// Hold the store's push guard, as a concurrent push of the store on the machine does.
    fn hold_push_guard(&self) -> std::fs::File {
        let guard = std::fs::File::options().read(true).write(true).create(true).truncate(false).open(self.root().join(STORE).join(".push.lock")).unwrap();
        guard.lock().unwrap();
        guard
    }

    /// The run row the execution database holds for `run`.
    fn row(&self, run: &str) -> Option<Value> {
        let db = contextful_d1::D1::connect(&self.d1).unwrap();
        let rows = db.query("SELECT row FROM contextful_run WHERE run_id = ?", vec![serde_json::json!(run)]).unwrap().rows;
        rows.first().map(|r| serde_json::from_str(r["row"].as_str().unwrap()).unwrap())
    }

    fn local_manifest(&self, run: &str) -> PathBuf {
        self.root().join(STORE).join(format!("tables/filings/data/runs/{run}/site/_manifest.json"))
    }
}

fn bucket_holds(s3: &S3Server, run: &str) -> bool {
    s3.keys().iter().any(|k| k.ends_with(&format!("tables/filings/data/runs/{run}/site/_manifest.json")))
}

fn runs(status: &Value) -> Vec<(String, bool)> {
    status["runs"].as_array().unwrap().iter().map(|r| (r["run_id"].as_str().unwrap().to_string(), r["lost"].as_bool().unwrap())).collect()
}

/// The bucket forbids the next put, as a bucket refusing the writer does: the next push
/// fails at its first write.
fn refuse_next_push(s3: &S3Server) {
    s3.fail_puts(&["AccessDenied"]);
}

/// A run whose push the bucket refuses stays marked in the execution database; while the
/// mark stands and the push still refuses, the next start opens no run, and once the bucket
/// takes writes, that next start pushes the marked run before its own opens. The status
/// names the refusal in its JSON and exits 3 while the mark stands, and exits 0 once clear.
// spec: store.drain.before-read@94220770
// spec: store.drain.status@410e1992
#[test]
fn a_refused_push_keeps_its_run_marked_until_the_next_start_pushes_it() {
    let s3 = S3Server::start("conformance");
    let disk = Disk::new(&s3);

    refuse_next_push(&s3);
    let refused = disk.start("r1");
    assert!(!refused.status.success(), "a refused push fails its run's command");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("push_after_run"), "{}", String::from_utf8_lossy(&refused.stderr));
    assert!(disk.local_manifest("r1").exists(), "the run committed locally");
    assert!(!bucket_holds(&s3, "r1"));
    let row = disk.row("r1").unwrap();
    assert_eq!(row["status"], "success");
    assert_eq!(row["unpushed"]["node"], "site", "{row}");

    let (code, status, stderr) = disk.status();
    assert_eq!(code, Some(3), "an unpushed run refuses the status check with its own exit code: {stderr}");
    assert_eq!(status["error"], "SyncRunsUnpushed", "{status}");
    assert!(stderr.contains("SyncRunsUnpushed") && stderr.contains("r1"), "{stderr}");
    assert_eq!(runs(&status), vec![("r1".to_string(), false)]);
    let listed = &status["runs"][0];
    assert_eq!((listed["pipeline"].as_str(), listed["table"].as_str(), listed["node"].as_str()), (Some("feed"), Some("filings"), Some("site")));
    assert!(listed["ended_at"].is_string(), "{listed}");

    // The bucket still refuses: the next start stops before its first read, naming r1.
    refuse_next_push(&s3);
    let blocked = disk.start("r2");
    assert!(!blocked.status.success());
    let stderr = String::from_utf8_lossy(&blocked.stderr);
    assert!(stderr.contains("`r1`") && stderr.contains("unpushed"), "{stderr}");
    assert!(disk.row("r2").is_none(), "no run opens while its pull's push refuses");

    let next = disk.start("r3");
    assert!(next.status.success(), "{}", String::from_utf8_lossy(&next.stderr));
    assert!(bucket_holds(&s3, "r1") && bucket_holds(&s3, "r3"));
    assert!(disk.row("r1").unwrap().get("unpushed").is_none(), "the committed push clears the mark");
    assert!(disk.row("r3").unwrap().get("unpushed").is_none());
    let (code, status, stderr) = disk.status();
    assert_eq!(code, Some(0), "{stderr}");
    assert!(status["error"].is_null(), "{status}");
    assert!(runs(&status).is_empty(), "{status}");
}

/// A status whose execution database answers no read fails apart from the unpushed
/// refusal: `runs` null, `error` `SyncStatusUnread`, exit 1.
// spec: store.drain.status-unread@f589e9a2
#[test]
fn a_status_reaching_no_execution_database_fails_apart_from_an_unpushed_refusal() {
    let s3 = S3Server::start("conformance");
    let disk = Disk::new(&s3);
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);

    let (code, status, stderr) = disk.status_via(&dead);
    assert_eq!(code, Some(1), "{stderr}");
    assert_eq!(status["error"], "SyncStatusUnread", "{status}");
    assert!(status["runs"].is_null(), "an unread status lists nothing: {status}");
    assert!(status["message"].is_string(), "{status}");
    assert!(stderr.contains("SyncStatusUnread") && !stderr.contains("SyncRunsUnpushed"), "{stderr}");
}

/// A container replaced before any push carried its marked run: the next push finds no copy
/// of the run, marks it `lost` and names it, and the status lists it without refusing.
// spec: store.drain.clear@53b5e343
#[test]
fn a_replacement_disk_marks_a_run_no_push_carried_lost() {
    let s3 = S3Server::start("conformance");
    let disk = Disk::new(&s3);

    refuse_next_push(&s3);
    assert!(!disk.start("r1").status.success());
    disk.fresh();

    let next = disk.start("r2");
    assert!(next.status.success(), "{}", String::from_utf8_lossy(&next.stderr));
    let stderr = String::from_utf8_lossy(&next.stderr);
    assert!(stderr.contains("`r1`") && stderr.contains("lost"), "{stderr}");
    assert!(!bucket_holds(&s3, "r1") && bucket_holds(&s3, "r2"));
    assert_eq!(disk.row("r1").unwrap()["unpushed"]["lost"], true);
    let (code, status, stderr) = disk.status();
    assert_eq!(code, Some(0), "a lost run is listed without refusing: {stderr}");
    assert_eq!(runs(&status), vec![("r1".to_string(), true)]);
}

/// A proxy in front of the execution database that forwards each request until `manifest`
/// exists, then holds every later one unanswered: the process issuing it waits there, past
/// its local commit, until it is killed.
struct StallAfterCommit {
    url: String,
    stalled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl StallAfterCommit {
    fn start(upstream: &str, manifest: PathBuf) -> StallAfterCommit {
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicBool, Ordering};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let upstream = upstream.trim_start_matches("http://").to_string();
        let stalled = std::sync::Arc::new(AtomicBool::new(false));
        let flag = stalled.clone();
        std::thread::spawn(move || {
            for client in listener.incoming().map_while(Result::ok) {
                let Ok(server) = std::net::TcpStream::connect(&upstream) else { continue };
                let (mut client_in, mut server_out) = (client.try_clone().unwrap(), server.try_clone().unwrap());
                let (mut server_in, mut client_out) = (server, client);
                std::thread::spawn(move || {
                    let mut buf = [0u8; 8192];
                    while let Ok(n) = server_in.read(&mut buf) {
                        if n == 0 || client_out.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                });
                let (manifest, flag) = (manifest.clone(), flag.clone());
                std::thread::spawn(move || {
                    let mut buf = [0u8; 8192];
                    while let Ok(n) = client_in.read(&mut buf) {
                        if n == 0 {
                            break;
                        }
                        if manifest.exists() {
                            flag.store(true, Ordering::SeqCst);
                            // Hold the request; the client dies waiting on it.
                            let _ = client_in.read(&mut buf);
                            break;
                        }
                        if server_out.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                });
            }
        });
        StallAfterCommit { url, stalled }
    }
}

/// A run killed after its local commit and before its own push, here while its execution
/// database stops answering, still carries its mark: the status refuses on it, and the next
/// push carries it and clears the mark.
// spec: store.drain.mark@8601b5e6
#[test]
fn a_run_killed_after_its_local_commit_stays_marked_until_a_push_carries_it() {
    let s3 = S3Server::start("conformance");
    let disk = Disk::new(&s3);
    let proxy = StallAfterCommit::start(&disk.d1, disk.local_manifest("r1"));

    let mut run = disk
        .command_via(&proxy.url, &["run", "start", "--plan", "feed.toml", "--project", "research", "--run-id", "r1", "--site-id", "site"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !proxy.stalled.load(std::sync::atomic::Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "the run reached its local commit");
        assert!(run.try_wait().unwrap().is_none(), "the run waits past its commit");
        std::thread::sleep(Duration::from_millis(20));
    }
    run.kill().unwrap();
    run.wait().unwrap();

    assert!(disk.local_manifest("r1").exists(), "the run committed locally");
    assert!(!bucket_holds(&s3, "r1"));
    assert_eq!(disk.row("r1").unwrap()["unpushed"]["node"], "site", "the mark predates the commit");
    let (code, status, stderr) = disk.status();
    assert_eq!(code, Some(3), "{stderr}");
    assert_eq!(runs(&status), vec![("r1".to_string(), false)]);

    let pushed = disk.cf(&["sync", "push", "--project", "research"]);
    assert!(pushed.status.success(), "{}", String::from_utf8_lossy(&pushed.stderr));
    assert!(bucket_holds(&s3, "r1"), "the push carries the killed run's commit");
    assert!(disk.row("r1").unwrap().get("unpushed").is_none());
    let (code, status, stderr) = disk.status();
    assert_eq!(code, Some(0), "{stderr}");
    assert!(runs(&status).is_empty(), "{status}");
}

/// Start `pipeline serve` and return once it serves, its standard error drained on a thread.
fn serve(disk: &Disk) -> (Child, std::sync::mpsc::Receiver<String>) {
    let mut child = disk.command(&["pipeline", "serve", "--project", "research"]).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
    let err = BufReader::new(child.stderr.take().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in err.lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut seen = String::new();
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(line) if line.starts_with("serving `research`") => return (child, rx),
            Ok(line) => {
                seen.push_str(&line);
                seen.push('\n');
            }
            Err(_) => panic!("serve never served: {seen}"),
        }
    }
}

/// Send serve a stop signal and wait for it: its exit, its later standard error, and how long it took.
fn stop(mut child: Child, rx: std::sync::mpsc::Receiver<String>) -> (bool, String, Duration) {
    let started = Instant::now();
    let sent = Command::new("kill").args(["-TERM", &child.id().to_string()]).status().unwrap();
    assert!(sent.success());
    let status = child.wait().unwrap();
    let took = started.elapsed();
    let mut stderr = String::new();
    while let Ok(line) = rx.recv_timeout(Duration::from_secs(2)) {
        stderr.push_str(&line);
        stderr.push('\n');
    }
    (status.success(), stderr, took)
}

/// A stopping serve pushes the run its node left unpushed and exits clean; one whose push
/// waits on a held push guard gives up at its deadline, exits non-zero and leaves the run
/// marked.
// spec: store.drain.deadline@a745585a
// spec: store.drain.unfinished@fa106369
#[test]
fn a_stopping_serve_pushes_its_unpushed_run_within_its_deadline() {
    let s3 = S3Server::start("conformance");
    let disk = Disk::new(&s3);
    // A guard wait past the deadline, so the deadline alone ends the stop push.
    std::fs::write(disk.root().join(STORE).join("config.toml"), disk.config.replace("push_wait_secs = 1", "push_wait_secs = 120")).unwrap();

    let (serving, rx) = serve(&disk);
    refuse_next_push(&s3);
    assert!(!disk.start("r1").status.success());
    let (clean, stderr, _) = stop(serving, rx);
    assert!(clean, "{stderr}");
    assert!(bucket_holds(&s3, "r1"), "the stop push carries the run: {stderr}");
    assert!(disk.row("r1").unwrap().get("unpushed").is_none());

    let (serving, rx) = serve(&disk);
    refuse_next_push(&s3);
    assert!(!disk.start("r2").status.success());
    let guard = disk.hold_push_guard();
    let (clean, stderr, took) = stop(serving, rx);
    drop(guard);
    assert!(!clean, "a stop push past its deadline fails serve: {stderr}");
    assert!(stderr.contains("`r2`"), "{stderr}");
    assert!(took >= Duration::from_secs(14) && took < Duration::from_secs(40), "the stop push ends at its 15 s deadline, took {took:?}");
    assert!(!bucket_holds(&s3, "r2"));
    assert_eq!(disk.row("r2").unwrap()["unpushed"]["node"], "site");
}

fn bucket_holds_in(s3: &S3Server, table: &str, run: &str) -> bool {
    s3.keys().iter().any(|k| k.ends_with(&format!("tables/{table}/data/runs/{run}/site/_manifest.json")))
}

/// A run committed into a nested table name keeps its mark through a refused push, and the
/// next start's push clears it rather than marking it lost.
#[test]
fn a_nested_tables_refused_run_stays_marked_until_a_push_carries_it() {
    let s3 = S3Server::start("conformance");
    let disk = Disk::new(&s3);
    disk.declare("ns/events", "");

    refuse_next_push(&s3);
    let refused = disk.start("r1");
    assert!(!refused.status.success(), "a refused push fails its run's command");
    assert!(disk.root().join(STORE).join("tables/ns/events/data/runs/r1/site/_manifest.json").exists(), "the run committed locally");
    let row = disk.row("r1").unwrap();
    assert_eq!(row["unpushed"]["node"], "site", "a nested table's committed run keeps its mark: {row}");

    let next = disk.start("r2");
    let stderr = String::from_utf8_lossy(&next.stderr);
    assert!(next.status.success(), "{stderr}");
    assert!(!stderr.contains("marked lost"), "{stderr}");
    assert!(bucket_holds_in(&s3, "ns/events", "r1"));
    assert!(disk.row("r1").unwrap().get("unpushed").is_none(), "the committed push clears the mark");
    let (code, status, stderr) = disk.status();
    assert_eq!(code, Some(0), "{stderr}");
    assert!(runs(&status).is_empty(), "{status}");
}

/// A run committing only into a table the declaration withholds from every push carries
/// nothing a push ships, so its mark drops at its close and the status lists nothing.
#[test]
fn a_withheld_tables_run_carries_no_mark_past_its_close() {
    let s3 = S3Server::start("conformance");
    let disk = Disk::new(&s3);
    disk.declare("filings", "[pipeline.tables.policy.redistribution]\nreplicate = false\n");

    // The push itself may refuse what the withholding leaves at the root; the mark drops first.
    let _ = disk.start("r1");
    assert!(disk.local_manifest("r1").exists(), "the run committed locally");
    assert!(!bucket_holds(&s3, "r1"), "the push withholds the table");
    assert!(disk.row("r1").unwrap().get("unpushed").is_none(), "{:?}", disk.row("r1"));
    let (code, status, stderr) = disk.status();
    assert_eq!(code, Some(0), "{stderr}");
    assert!(runs(&status).is_empty(), "{status}");
}

/// A received batch whose run opens and fails commits nothing, so its run drops its mark
/// before the push, and no later push marks it lost.
#[test]
fn a_received_run_committing_nothing_drops_its_mark() {
    let s3 = S3Server::start("conformance");
    let disk = Disk::new(&s3);
    std::fs::write(
        disk.root().join("contextful.toml"),
        "authoring_posture = 'per_request'\n[[pipeline]]\nid = 'otel'\ntables = ['spans']\n[pipeline.source]\nname = 'receive'\nconfig = { format = 'jsonl', max_batch_bytes = 4096, max_queue_depth = 4, key_retention_secs = 3600 }\n",
    )
    .unwrap();
    let mut child = disk
        .command(&["pipeline", "receive", "otel", "--key", "flush-1", "--project", "research", "--site-id", "site"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let _ = child.stdin.take().unwrap().write_all(b"{\"table\":\"spans\",\"row\":{\"id\":\"s1\",\"_modality\":\"7\"}}\n");
    }
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));

    let db = contextful_d1::D1::connect(&disk.d1).unwrap();
    let opened = db.query("SELECT row FROM contextful_run", vec![]).unwrap().rows;
    assert!(!opened.is_empty(), "the batch's run opened: {}", String::from_utf8_lossy(&out.stderr));
    let (code, status, stderr) = disk.status();
    assert_eq!(code, Some(0), "{stderr}");
    assert!(runs(&status).is_empty(), "{status}");
}
