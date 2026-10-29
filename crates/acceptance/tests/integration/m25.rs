//! Milestone 25 — Scoped callback guards.
//!
//! Reach: a guard on one item blocks its owner, ancestors and entity lease while another
//! job's admission proceeds, and a dead holder's replay-safe effect releases with its
//! original key while mismatched evidence leaves the guard.

use contextful_acceptance::s3::{S3Server, ACCESS_KEY, SECRET_KEY};
use contextful_acceptance::{bin, GitRepo};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const STORE: &str = ".contextful/context/research";

/// The paid vendor: it logs each request's idempotency key, charges a key once, and the
/// armed first run's process dies holding its guard on the second page.
const VENDOR: &str = r#"
printf 'page=%s key=%s\n' "${CONTEXTFUL_CURSOR:-start}" "$CONTEXTFUL_IDEMPOTENCY_KEY" >> effects.log
if [ ! -f "charged-$CONTEXTFUL_IDEMPOTENCY_KEY" ]; then
  : > "charged-$CONTEXTFUL_IDEMPOTENCY_KEY"
  printf 'page=%s key=%s\n' "${CONTEXTFUL_CURSOR:-start}" "$CONTEXTFUL_IDEMPOTENCY_KEY" >> charges.log
fi
case "$CONTEXTFUL_CURSOR" in
  '') printf '{"rows":[{"id":"a"},{"id":"b"}],"cursor":"p2","more":true}' ;;
  '"p2"') if [ -f crash-armed ]; then rm crash-armed; kill -9 "$PPID"; fi
          printf '{"rows":[{"id":"c"}],"cursor":"done","more":false}' ;;
  *) printf '{"rows":[],"cursor":"done","more":false}' ;;
esac
"#;

/// The loopback D1 binding the `cloudflare-d1` profile reaches.
struct Binding {
    process: Child,
    endpoint: String,
    _root: tempfile::TempDir,
}

impl Binding {
    fn start() -> Binding {
        let root = tempfile::tempdir().unwrap();
        let mut process = Command::new("node")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../contextful-d1/tests/fixture.mjs"))
            .arg(root.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut port = String::new();
        BufReader::new(process.stdout.take().unwrap()).read_line(&mut port).unwrap();
        Binding { process, endpoint: format!("http://127.0.0.1:{}", port.trim()), _root: root }
    }

    /// One statement against the binding, as the operator's database console runs it.
    fn sql(&self, statement: &str) -> Value {
        let body = json!({ "sql": statement, "params": [] }).to_string();
        let mut response = ureq::post(&format!("{}/sql", self.endpoint)).header("content-type", "application/json").send(body.as_bytes()).unwrap();
        assert_eq!(response.status(), 200);
        serde_json::from_slice(&response.body_mut().read_to_vec().unwrap()).unwrap()
    }
}

impl Drop for Binding {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

struct Project {
    repo: GitRepo,
    cf: PathBuf,
    d1: Binding,
    _s3: S3Server,
}

impl Project {
    fn new() -> Project {
        let s3 = S3Server::start("corpus");
        let repo = GitRepo::init();
        repo.write(
            &format!("{STORE}/config.toml"),
            &format!(
                "[node]\nid = 'site'\n[sync]\nendpoint = '{}'\nbucket = 'corpus'\nprefix = 'guards'\ncoordination = 'cas'\npull_before_run = true\npush_after_run = true\naccess_key_id = 'env://GUARD_TEST_ACCESS_KEY'\nsecret_access_key = 'env://GUARD_TEST_SECRET_KEY'\n",
                s3.endpoint
            ),
        );
        repo.write(
            "contextful.toml",
            "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"items\"\nprimary_key = [\"id\"]\n[[pipeline.tables]]\nname = \"notes\"\nprimary_key = [\"id\"]\n",
        );
        repo.write("vendor.sh", VENDOR);
        repo.write("feed.toml", "pipeline = \"feed\"\ntable = \"items\"\n[connector]\nid = \"vendor\"\nversion = \"1.0.0\"\nworld = \"native\"\ncommand = [\"sh\", \"vendor.sh\"]\n[cursor]\nkind = \"opaque-token\"\n");
        repo.write("notes.sh", "printf '{\"rows\":[{\"id\":\"n1\"}],\"more\":false}'\n");
        repo.write("notes.toml", "pipeline = \"notes\"\ntable = \"notes\"\n[connector]\nid = \"notes\"\nversion = \"1.0.0\"\nworld = \"native\"\ncommand = [\"sh\", \"notes.sh\"]\n");
        let project = Project { repo, cf: bin("contextful"), d1: Binding::start(), _s3: s3 };
        project.ok(&["storage", "initialize", "--project", "research"]);
        project
    }

    fn run(&self, args: &[&str]) -> Output {
        let env = [
            ("CONTEXTFUL_STORAGE_PROFILE", "cloudflare-d1"),
            ("CONTEXTFUL_D1_ENDPOINT", self.d1.endpoint.as_str()),
            ("CONTEXTFUL_RECOVERY_ID", "generation"),
            ("GUARD_TEST_ACCESS_KEY", ACCESS_KEY),
            ("GUARD_TEST_SECRET_KEY", SECRET_KEY),
        ];
        self.repo.run_env(&self.cf, args, &env)
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn start(&self, plan: &str, run: &str) -> Output {
        self.run(&["run", "start", "--plan", plan, "--project", "research", "--run-id", run, "--site-id", "site", "--now", "2030-01-01T00:00:00Z"])
    }

    fn guards(&self) -> Vec<Value> {
        let state: Value = serde_json::from_str(&self.ok(&["storage", "guard", "--project", "research"])).unwrap();
        state["guards"].as_array().unwrap().clone()
    }

    fn reconcile(&self, token: &str, evidence: &str) -> Output {
        self.run(&["storage", "reconcile-guard", "--project", "research", "--token", token, "--evidence", evidence, "--operator", "on-call"])
    }

    /// The idempotency key of each logged request for `page`.
    fn keys(&self, log: &str, page: &str) -> Vec<String> {
        let text = std::fs::read_to_string(self.repo.root.join(log)).unwrap_or_default();
        text.lines().filter_map(|line| line.strip_prefix(&format!("page={page} key="))).map(str::to_owned).collect()
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The milestone's Reach through the built binary against the loopback D1 binding: the
/// crashed run's guard blocks its own pipeline alone, refuses a live holder's and mismatched
/// evidence, and, once its effect is replay-safe and its holder gone, releases on the resume
/// that resends the original key.
#[test]
#[cfg(unix)]
fn m25_callback_guards() {
    use std::os::unix::process::ExitStatusExt;
    let p = Project::new();
    p.repo.write("crash-armed", "armed");
    let crashed = p.start("feed.toml", "feed-1");
    assert_eq!(crashed.status.signal(), Some(9), "the first run dies holding its guard: {}", stderr(&crashed));

    let guards = p.guards();
    assert_eq!(guards.len(), 1, "{guards:?}");
    let guard = &guards[0];
    let token = guard["token"].as_str().unwrap().to_string();
    let recorded = guard["idempotency_key"].as_str().unwrap().to_string();
    assert_eq!(p.keys("effects.log", "\"p2\""), vec![recorded.clone()], "the guard records the key the request sent");
    assert_eq!(guard["scopes"][0]["pipeline_id"], "feed", "{guard}");
    assert_eq!(guard["attempt"], "feed-1");
    assert_eq!(guard["window"], "none");
    assert_eq!(guard["class"], "unknown");

    // Another job's admission proceeds while the guard holds the feed's rows.
    let notes = p.start("notes.toml", "notes-1");
    assert!(notes.status.success(), "{}", stderr(&notes));

    // A holder whose owner lease is unexpired may still be sending its effect.
    let receipt = format!("receipt:{recorded}:vendor-ledger");
    let held = p.reconcile(&token, &receipt);
    assert!(!held.status.success() && stderr(&held).contains("ExecutionGuardHeld"), "{}", stderr(&held));

    // Once the owner lease lapses, evidence not tied to the recorded key leaves the guard.
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let refused = p.reconcile(&token, "idempotency:another-key");
        assert!(!refused.status.success());
        if stderr(&refused).contains("ExecutionGuardEvidenceMismatch") {
            break;
        }
        assert!(stderr(&refused).contains("ExecutionGuardHeld") && Instant::now() < deadline, "{}", stderr(&refused));
        std::thread::sleep(Duration::from_millis(500));
    }
    let unwindowed = p.reconcile(&token, &format!("idempotency:{recorded}"));
    assert!(stderr(&unwindowed).contains("ExecutionGuardEvidenceMismatch"), "a `none` window covers no key: {}", stderr(&unwindowed));
    let blocked = p.start("feed.toml", "feed-2");
    assert!(!blocked.status.success());
    assert!(stderr(&blocked).contains(&token) && stderr(&blocked).contains("contextful storage reconcile-guard"), "{}", stderr(&blocked));
    assert_eq!(p.guards().len(), 1, "mismatched evidence and a blocked resume leave the guard");

    // The vendor deduplicates its key for an hour, and the reaping serve records the dead
    // holder's exit: the guard becomes replay-safe.
    p.d1.sql(&format!("UPDATE contextful_callback_guard SET window_us = 3600000000 WHERE token = '{token}'"));
    p.d1.sql(&format!("UPDATE contextful_guard_holder SET exited_at = 1 WHERE holder = '{}'", guard["holder"].as_str().unwrap()));
    assert_eq!(p.guards()[0]["class"], "replay-safe");

    let resumed = p.start("feed.toml", "feed-3");
    assert!(resumed.status.success(), "{}", stderr(&resumed));
    assert!(p.guards().is_empty(), "the replay released the guard with its takeover");
    let sent = p.keys("effects.log", "\"p2\"");
    assert!(sent.len() >= 2 && sent.iter().all(|key| key == &recorded), "the replay resends the original key: {sent:?}");
    assert_eq!(p.keys("charges.log", "\"p2\""), vec![recorded], "the vendor charges the guarded effect once");
}
