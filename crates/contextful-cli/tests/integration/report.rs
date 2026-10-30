//! The `report` source through the built binary, against a loopback report vendor that
//! creates runs, answers their state and serves their rows in pages.
#![cfg(feature = "data-plane")]

use crate::pipeline::{cf, ok, project, stderr};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Output;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// One request the vendor received.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    target: String,
    body: serde_json::Value,
    at: Instant,
}

/// What the vendor answers each created run, by account: the polls answering `pending`
/// before the final status, and the final status.
#[derive(Default)]
struct Script {
    pending: BTreeMap<String, usize>,
    status: BTreeMap<String, String>,
    /// Accounts whose create answers 503.
    unavailable: Vec<String>,
    /// Accounts whose create answers 400.
    refused: Vec<String>,
}

struct Vendor {
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
    script: Arc<Mutex<Script>>,
}

/// The two pages of a run's rows, the cursor `p2` naming the second.
fn rows(account: &str, cursor: Option<&str>) -> String {
    let row = |ad: &str, day: &str, spend: u32| serde_json::json!({"ad_id": format!("{account}-{ad}"), "date_start": day, "age": "18-24", "spend": spend});
    match cursor {
        None => serde_json::json!({"data": [row("ad-1", "2030-01-08", 3), row("ad-2", "2030-01-09", 5)], "has_more": true, "paging": {"cursors": {"after": "p2"}}}),
        Some(_) => serde_json::json!({"data": [row("ad-3", "2030-01-09", 7)], "has_more": false}),
    }
    .to_string()
}

impl Vendor {
    fn start(script: Script) -> Vendor {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let script = Arc::new(Mutex::new(script));
        let (log, state) = (seen.clone(), script.clone());
        std::thread::spawn(move || {
            let mut created = 0;
            let mut runs: BTreeMap<String, (String, usize)> = BTreeMap::new();
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut parts = line.split_whitespace();
                let (method, target) = (parts.next().unwrap_or_default().to_string(), parts.next().unwrap_or_default().to_string());
                let mut length = 0;
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap() == 0 || h.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        if k.trim().eq_ignore_ascii_case("content-length") {
                            length = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let mut body = vec![0; length];
                let _ = reader.read_exact(&mut body);
                let body: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
                let path: Vec<&str> = target.split('?').next().unwrap_or_default().trim_matches('/').split('/').collect();
                let script = state.lock().unwrap();
                let (status, answer) = match (method.as_str(), path.as_slice()) {
                    ("POST", ["v1", account, "reports"]) if script.unavailable.iter().any(|a| a == account) => (503, "{}".to_string()),
                    ("POST", ["v1", account, "reports"]) if script.refused.iter().any(|a| a == account) => (400, "{}".to_string()),
                    ("POST", ["v1", account, "reports"]) => {
                        created += 1;
                        let id = format!("r-{created}");
                        runs.insert(id.clone(), (account.to_string(), script.pending.get(*account).copied().unwrap_or(0)));
                        (201, serde_json::json!({"report": {"id": id}}).to_string())
                    }
                    ("GET", ["v1", "reports", id]) => match runs.get_mut(*id) {
                        Some((_, pending)) if *pending > 0 => {
                            *pending -= 1;
                            (200, r#"{"status":"pending"}"#.to_string())
                        }
                        Some((account, _)) => {
                            let status = script.status.get(account.as_str()).cloned().unwrap_or_else(|| "done".into());
                            (200, serde_json::json!({"status": status, "rows_url": format!("/v1/reports/{id}/rows")}).to_string())
                        }
                        None => (404, "{}".to_string()),
                    },
                    ("GET", ["v1", "reports", id, "rows"]) => match runs.get(*id) {
                        Some((account, _)) => (200, rows(account, target.split_once("after=").map(|(_, c)| c))),
                        None => (404, "{}".to_string()),
                    },
                    _ => (404, "{}".to_string()),
                };
                drop(script);
                log.lock().unwrap().push(Seen { method, target, body, at: Instant::now() });
                let _ = write!(stream, "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}", answer.len());
            }
        });
        Vendor { port, seen, script }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn creates(&self) -> Vec<Seen> {
        self.seen().into_iter().filter(|s| s.method == "POST").collect()
    }

    fn polls(&self) -> Vec<Seen> {
        self.seen().into_iter().filter(|s| s.method == "GET" && !s.target.contains("/rows")).collect()
    }
}

/// A `report` pipeline over `accounts`, its vendor keys set by `vendor_keys`.
fn manifest(vendor: &Vendor, accounts: &[&str], extra: &str, result: &str) -> String {
    let partitions: Vec<String> = accounts.iter().map(|a| format!("{{ account = \"{a}\", grain = \"day\", breakdown = \"age\" }}")).collect();
    format!(
        r#"[[pipeline]]
id = "ads"
tables = ["insights"]

[pipeline.source]
name = "report"

[pipeline.source.config]
partitions = [{partitions}]
metrics = ["spend", "clicks"]
entity = "ad_id"
date = "date_start"
lookback_days = 2
window_days = 3
{extra}

[pipeline.source.config.create]
url = "{create}"
body = {{ since = "{{start}}", until = "{{end}}", fields = "{{metrics}}", breakdown = "{{breakdown}}" }}
id = "/report/id"

[pipeline.source.config.poll]
url = "{poll}"
interval = "1s"

[pipeline.source.config.state]
pointer = "/status"
done = "done"
failed = ["failed", "error"]

[pipeline.source.config.result]
{result}
"#,
        partitions = partitions.join(", "),
        create = vendor.url("/v1/{account}/reports"),
        poll = vendor.url("/v1/reports/{id}"),
    )
}

fn result_url(vendor: &Vendor) -> String {
    format!("url = \"{}\"", vendor.url("/v1/reports/{id}/rows"))
}

fn fire(dir: &Path, run: &str, now: &str) -> Output {
    cf(dir, &["pipeline", "run", "ads", "--project", "research", "--run-id", run, "--site-id", "site-a", "--now", now])
}

fn landed(dir: &Path) -> Vec<serde_json::Value> {
    let sql = "SELECT report_partition, ad_id, event_time FROM ads_insights ORDER BY report_partition, ad_id";
    let out: serde_json::Value = serde_json::from_str(&ok(&cf(dir, &["query", "--json", "--project", "research", sql]))).unwrap();
    out["rows"].as_array().cloned().unwrap_or_default()
}

/// The `report` source declares its vendor's create, poll, state and result fields beside the partition and
/// watermark keys; a fire creates each partition's run, polls it at its interval until done, drains its pages and
/// holds one watermark per partition, which the next fire's window opens from.
// spec: connector.source.report-declaration@2762cc3e
// spec: connector.source.report-templates@25b5c892
// spec: connector.source.report-poll@b296c7bf
// spec: connector.source.report-result@87a6e08c
// spec: connector.source.report-watermark@2a91444e
// spec: connector.source.partition-isolation@e11cae37
#[test]
fn a_fire_creates_polls_and_lands_a_report_with_a_watermark_per_partition() {
    let vendor = Vendor::start(Script { pending: BTreeMap::from([("a1".into(), 1)]), unavailable: vec!["a2".into()], ..Script::default() });
    let dir = project(&manifest(&vendor, &["a1", "a2"], "", &result_url(&vendor)));
    ok(&cf(dir.path(), &["pipeline", "validate"]));

    // The first fire lands a1's two pages; a2's create answers 503 and holds a2 at its committed position.
    let out = ok(&fire(dir.path(), "f1", "2030-01-10T06:00:00Z"));
    assert!(out.contains("ads_insights: f1 success · 3 rows"), "{out}");
    let creates = vendor.creates();
    assert_eq!(creates.iter().map(|c| c.target.as_str()).collect::<Vec<_>>(), ["/v1/a1/reports", "/v1/a2/reports"]);
    assert_eq!(creates[0].body, serde_json::json!({"since": "2030-01-07", "until": "2030-01-10", "fields": "spend,clicks", "breakdown": "age"}));
    let polls = vendor.polls();
    assert_eq!(polls.iter().map(|p| p.target.as_str()).collect::<Vec<_>>(), ["/v1/reports/r-1", "/v1/reports/r-1"]);
    assert!(polls[1].at.duration_since(polls[0].at).as_millis() >= 1000, "two polls of one run wait the declared interval");
    let pages: Vec<String> = vendor.seen().into_iter().filter(|s| s.target.contains("/rows")).map(|s| s.target).collect();
    assert_eq!(pages, ["/v1/reports/r-1/rows", "/v1/reports/r-1/rows?after=p2"]);
    let rows = landed(dir.path());
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert!(rows.iter().all(|r| r[0] == "a1|day|age" && r[1].as_str().is_some_and(|ad| ad.starts_with("a1-"))), "{rows:?}");
    assert_eq!(rows[0][2], "2030-01-08T00:00:00Z");

    // Two days on, a1's window opens its lookback before its watermark, and a2, holding none, its full width.
    vendor.script.lock().unwrap().unavailable.clear();
    let out = ok(&fire(dir.path(), "f2", "2030-01-12T06:00:00Z"));
    assert!(out.contains("ads_insights: f2 success · 6 rows"), "{out}");
    let creates = vendor.creates();
    assert_eq!(creates.len(), 4, "{creates:?}");
    let window = |c: &Seen| (c.target.clone(), c.body["since"].clone(), c.body["until"].clone());
    assert_eq!(window(&creates[2]), ("/v1/a1/reports".into(), "2030-01-08".into(), "2030-01-12".into()));
    assert_eq!(window(&creates[3]), ("/v1/a2/reports".into(), "2030-01-09".into(), "2030-01-12".into()));
    assert!(landed(dir.path()).iter().any(|r| r[0] == "a2|day|age"), "a2 lands once its vendor answers");
}

/// A poll answering a declared failed value fails the fire with `ConnectorReportFailed`; the position drops that
/// run, so the next fire creates a new one.
// spec: connector.source.report-failed@3c7c02fd
#[test]
fn a_failed_report_state_fails_the_fire_and_the_next_fire_creates_a_new_report() {
    let vendor = Vendor::start(Script { status: BTreeMap::from([("a1".into(), "error".into())]), ..Script::default() });
    let dir = project(&manifest(&vendor, &["a1"], "", &result_url(&vendor)));
    let out = fire(dir.path(), "f1", "2030-01-10T06:00:00Z");
    assert!(!out.status.success(), "a failed report fails the fire");
    let err = stderr(&out);
    assert!(err.contains("ConnectorReportFailed") && err.contains("r-1"), "{err}");
    let seen = vendor.seen();
    assert_eq!(seen.len(), 2, "one create and one poll: {seen:?}");
    assert!(seen.iter().all(|s| !s.target.contains("/rows")), "a failed run's rows are never read");

    vendor.script.lock().unwrap().status.clear();
    let out = ok(&fire(dir.path(), "f2", "2030-01-10T07:00:00Z"));
    assert!(out.contains("3 rows"), "{out}");
    let creates = vendor.creates();
    assert_eq!(creates.len(), 2, "the next fire creates a new run: {creates:?}");
    assert_eq!(vendor.polls().last().map(|p| p.target.clone()), Some("/v1/reports/r-2".into()));
}

/// A run still building when the polling budget runs out ends the fire successfully, its id committed; the next
/// fire polls that id rather than creating another, and reads its rows at the URL the done poll names.
// spec: connector.source.report-id-commit@73c56a64
// spec: connector.source.report-result@87a6e08c
#[test]
fn a_report_still_building_ends_the_fire_and_the_next_fire_polls_its_id() {
    let vendor = Vendor::start(Script { pending: BTreeMap::from([("a1".into(), 2)]), ..Script::default() });
    let dir = project(&manifest(&vendor, &["a1"], "poll_budget = 2", "pointer = \"/rows_url\""));
    let out = ok(&fire(dir.path(), "f1", "2030-01-10T06:00:00Z"));
    assert!(out.contains("f1 success · 0 rows"), "{out}");
    assert_eq!(vendor.creates().len(), 1);
    assert_eq!(vendor.polls().len(), 2);

    let out = ok(&fire(dir.path(), "f2", "2030-01-10T07:00:00Z"));
    assert!(out.contains("f2 success · 3 rows"), "{out}");
    assert_eq!(vendor.creates().len(), 1, "the resumed fire creates no second run");
    let polls = vendor.polls();
    assert!(!polls.is_empty());
    assert!(polls.iter().all(|p| p.target == "/v1/reports/r-1"), "{polls:?}");
    let pages: Vec<String> = vendor.seen().into_iter().filter(|s| s.target.contains("/rows")).map(|s| s.target).collect();
    assert_eq!(pages, ["/v1/reports/r-1/rows", "/v1/reports/r-1/rows?after=p2"]);
}

/// Each malformed vendor field fails `pipeline validate` naming its key, before any request.
// spec: connector.source.report-declaration@2762cc3e
// spec: connector.source.report-templates@25b5c892
#[test]
fn a_malformed_report_declaration_fails_validation_naming_its_key() {
    let vendor = Vendor::start(Script::default());
    let good = manifest(&vendor, &["a1"], "", &result_url(&vendor));
    ok(&cf(project(&good).path(), &["pipeline", "validate"]));
    let poll = format!("url = \"{}\"\ninterval", vendor.url("/v1/reports/{id}"));
    let other_port = format!("url = \"http://127.0.0.1:{}/v1/reports/{{id}}\"\ninterval", vendor.port.wrapping_add(1));
    let cases = [
        (good.replace("interval = \"1s\"", "interval = \"soon\""), "poll.interval"),
        (good.replace(&poll, &format!("url = \"{}\"\ninterval", vendor.url("/v1/reports"))), "poll.url"),
        (good.replace(&poll, &other_port), "poll.url"),
        (good.replace("{start}", "{starts}"), "create.body"),
        (good.replace("127.0.0.1:", "{account}.example:"), "create.url"),
        (good.replace("[pipeline.source.config.create]\n", "[pipeline.source.config.create]\nmethod = \"DELETE\"\n"), "create.method"),
        (good.replace("id = \"/report/id\"", "id = \"report.id\""), "create.id"),
        (good.replace("failed = [\"failed\", \"error\"]\n", ""), "failed"),
        (good.replace("failed = [\"failed\", \"error\"]", "failed = [\"done\"]"), "two outcomes"),
        (good.replace("[pipeline.source.config.result]\n", "[pipeline.source.config.result]\npointer = \"/rows_url\"\n"), "result"),
        (good.replace("interval = \"1s\"", "interval = \"1s\"\nevery = \"1s\""), "every"),
    ];
    for (declaration, key) in cases {
        assert_ne!(declaration, good, "the case for `{key}` changes the declaration");
        let out = cf(project(&declaration).path(), &["pipeline", "validate"]);
        assert!(!out.status.success(), "{key}: {}", String::from_utf8_lossy(&out.stdout));
        assert!(stderr(&out).contains(key), "{key}: {}", stderr(&out));
    }
    assert!(vendor.seen().is_empty(), "validation reaches no vendor: {:?}", vendor.seen());
}

/// A project over `a1` and `a2` whose fire `f1` failed once its journal held both of `a1`'s pages: `f0` ended with
/// `a1`'s run still building, its id committed, and `a2`'s create answered 400 in `f1`.
fn interrupted(vendor: &Vendor) -> tempfile::TempDir {
    let dir = project(&manifest(vendor, &["a1", "a2"], "poll_budget = 1", &result_url(vendor)));
    ok(&fire(dir.path(), "f0", "2030-01-10T06:00:00Z"));
    vendor.script.lock().unwrap().refused.push("a2".into());
    let failed = fire(dir.path(), "f1", "2030-01-10T07:00:00Z");
    assert!(!failed.status.success(), "a 400 fails the fire: {}", String::from_utf8_lossy(&failed.stdout));
    let pages: Vec<String> = vendor.seen().into_iter().filter(|s| s.target.contains("/rows")).map(|s| s.target).collect();
    assert_eq!(pages, ["/v1/reports/r-1/rows", "/v1/reports/r-1/rows?after=p2"], "the journal holds a1's two pages");
    vendor.script.lock().unwrap().refused.clear();
    dir
}

/// A fire resumed after its journal holds a finished partition's pages replays them and reads on from the next
/// partition: it creates and polls no report for `a1` again, and lands each row once.
// spec: run.journal.replay-lands@0cc34691
#[test]
fn a_resumed_report_fire_replays_a_finished_partition_without_creating_or_polling_its_report_again() {
    let vendor = Vendor::start(Script { pending: BTreeMap::from([("a1".into(), 1)]), ..Script::default() });
    let dir = interrupted(&vendor);
    let before = vendor.seen().len();

    let out = ok(&fire(dir.path(), "f2", "2030-01-10T08:00:00Z"));
    let resumed: Vec<(String, String)> = vendor.seen()[before..].iter().map(|s| (s.method.clone(), s.target.clone())).collect();
    assert!(out.contains("6 rows"), "{out}{resumed:?}");
    assert!(!resumed.is_empty(), "the resumed fire reads a2 live");
    assert_eq!(resumed[0], ("POST".to_string(), "/v1/a2/reports".to_string()), "{resumed:?}");
    assert!(resumed.iter().all(|(_, target)| !target.contains("/a1/") && !target.contains("/r-1")), "{resumed:?}");
    let landed: Vec<(String, String)> = landed(dir.path()).iter().map(|r| (r[0].as_str().unwrap_or_default().into(), r[1].as_str().unwrap_or_default().into())).collect();
    let once: Vec<(String, String)> = ["a1", "a2"].iter().flat_map(|a| ["ad-1", "ad-2", "ad-3"].map(|ad| (format!("{a}|day|age"), format!("{a}-{ad}")))).collect();
    assert_eq!(landed, once, "each row lands once");
}

/// The first live pull after a resume opens its window on the interrupted fire's day, not the resuming fire's, so the
/// resumed read creates the report an uninterrupted fire creates.
// spec: run.journal.replay-lands@0cc34691
#[test]
fn a_resumed_report_fire_opens_its_live_window_on_the_interrupted_fires_day() {
    let vendor = Vendor::start(Script { pending: BTreeMap::from([("a1".into(), 1)]), ..Script::default() });
    let dir = interrupted(&vendor);

    let out = ok(&fire(dir.path(), "f2", "2030-01-12T06:00:00Z"));
    assert!(out.contains("6 rows"), "{out}");
    let creates = vendor.creates();
    let last = creates.last().expect("the resumed fire creates a2's report");
    assert_eq!(last.target, "/v1/a2/reports", "{creates:?}");
    assert_eq!((last.body["since"].clone(), last.body["until"].clone()), ("2030-01-07".into(), "2030-01-10".into()), "{creates:?}");
}
