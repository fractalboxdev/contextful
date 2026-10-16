//! The async-report source over a scripted vendor: run-id commit, page drain, per-partition
//! watermarks, the restated window and the refusals.

use crate::support::{request, Never};
use contextful_connectors::report::{Partition, ReportConfig, ReportSource, ReportVendor, RunState, VendorFault, Window};
use contextful_core::run::ports::Source;
use contextful_core::run::{Failure, FailureTag};
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

const TODAY: &str = "2030-01-10";

/// One partition's script: the pages it serves in order, the poll answers it gives
/// (`Ready` once drained), and the page index that faults, with the fault.
#[derive(Default, Clone)]
struct Script {
    pages: Vec<Value>,
    polls: VecDeque<RunState>,
    fault_at: Option<(usize, VendorFault)>,
    create_fault: Option<VendorFault>,
}

#[derive(Default)]
struct Log {
    creates: Vec<(String, Window)>,
    polls: usize,
    pages: Vec<(String, Option<String>)>,
}

struct Vendor {
    scripts: BTreeMap<String, Script>,
    runs: usize,
    log: Arc<Mutex<Log>>,
}

impl Vendor {
    fn new(scripts: Vec<(&str, Script)>) -> (Vendor, Arc<Mutex<Log>>) {
        let log = Arc::new(Mutex::new(Log::default()));
        let scripts = scripts.into_iter().map(|(k, s)| (k.to_string(), s)).collect();
        (Vendor { scripts, runs: 0, log: log.clone() }, log)
    }
}

impl ReportVendor for Vendor {
    fn create(&mut self, partition: &Partition, window: &Window) -> Result<String, VendorFault> {
        if let Some(fault) = self.scripts[&partition.account].create_fault.clone() {
            return Err(fault);
        }
        self.runs += 1;
        self.log.lock().unwrap().creates.push((partition.account.clone(), window.clone()));
        Ok(format!("run-{}", self.runs))
    }

    fn poll(&mut self, partition: &Partition, _run_id: &str) -> Result<RunState, VendorFault> {
        self.log.lock().unwrap().polls += 1;
        let script = self.scripts.get_mut(&partition.account).unwrap();
        Ok(script.polls.pop_front().unwrap_or(RunState::Ready))
    }

    fn page(&mut self, partition: &Partition, _run_id: &str, cursor: Option<&str>) -> Result<Value, VendorFault> {
        self.log.lock().unwrap().pages.push((partition.account.clone(), cursor.map(str::to_string)));
        let script = &self.scripts[&partition.account];
        let index = cursor.map_or(0, |c| c.trim_start_matches('c').parse::<usize>().unwrap());
        if let Some((at, fault)) = &script.fault_at {
            if *at == index {
                return Err(fault.clone());
            }
        }
        Ok(script.pages[index].clone())
    }
}

/// A page of `rows` announcing the next page by cursor when one follows.
fn page(index: usize, total: usize, rows: Vec<Value>) -> Value {
    if index + 1 < total {
        json!({"data": rows, "has_more": true, "paging": {"cursors": {"after": format!("c{}", index + 1)}}})
    } else {
        json!({"data": rows, "has_more": false})
    }
}

fn pages(total: usize) -> Vec<Value> {
    (0..total)
        .map(|i| page(i, total, vec![json!({"ad_id": format!("ad-{i}"), "date_start": "2030-01-05", "age": "18-24", "spend": i})]))
        .collect()
}

fn config(accounts: &[&str], extra: Value) -> ReportConfig {
    let partitions: Vec<Value> = accounts.iter().map(|a| json!({"account": a, "grain": "day", "breakdown": "age"})).collect();
    let mut value = json!({"partitions": partitions, "metrics": ["spend"], "entity": "ad_id", "date": "date_start"});
    for (k, v) in extra.as_object().unwrap() {
        value[k] = v.clone();
    }
    ReportConfig::parse(&value).unwrap()
}

fn pull(source: &mut dyn Source, position: Option<Value>) -> Result<Value, Failure> {
    Ok(serde_json::from_slice(&source.pull(&request(position), &Never)?).unwrap())
}

/// Every pull of one read, each from the position the one before committed.
fn read(source: &mut dyn Source, mut position: Option<Value>) -> Vec<Value> {
    let mut batches = Vec::new();
    loop {
        let batch = pull(source, position.clone()).unwrap();
        position = batch.get("cursor").cloned();
        let more = batch["more"].as_bool().unwrap_or(false);
        batches.push(batch);
        if !more {
            return batches;
        }
    }
}

fn key(account: &str) -> String {
    Partition { account: account.into(), grain: "day".into(), breakdown: "age".into(), split: BTreeMap::new() }.key()
}

fn rows(batches: &[Value]) -> Vec<Value> {
    batches.iter().flat_map(|b| b["rows"].as_array().cloned().unwrap_or_default()).collect()
}

// spec: connector.source.report-id-commit@fae17566
#[test]
fn the_run_id_commits_on_creation_and_an_expired_run_recreates_for_its_window() {
    let (vendor, log) = Vendor::new(vec![("a1", Script { pages: pages(1), polls: VecDeque::from([RunState::Expired]), ..Script::default() })]);
    let mut source = ReportSource::new(config(&["a1"], json!({})), vendor, TODAY).unwrap();
    let created = pull(&mut source, None).unwrap();
    assert_eq!(created["rows"], json!([]));
    assert_eq!(created["more"], json!(true));
    assert_eq!(created["cursor"]["partitions"][key("a1")]["run"]["id"], json!("run-1"));
    let recreated = pull(&mut source, created.get("cursor").cloned()).unwrap();
    assert_eq!(recreated["rows"], json!([]));
    assert_eq!(recreated["cursor"]["partitions"][key("a1")]["run"]["id"], json!("run-2"));
    let creates = &log.lock().unwrap().creates;
    assert_eq!(creates.len(), 2);
    assert_eq!(creates[0].1, creates[1].1, "the recreated run covers the same window");
}

#[test]
fn a_run_expiring_between_reads_recreates_for_its_window_and_restarts_its_pages() {
    let (vendor, log) = Vendor::new(vec![("a1", Script { pages: pages(2), polls: VecDeque::from([RunState::Expired]), ..Script::default() })]);
    let mut source = ReportSource::new(config(&["a1"], json!({})), vendor, TODAY).unwrap();
    let held = json!({"version": 1, "partitions": {key("a1"): {"run": {"id": "run-0", "start": "2030-01-01", "end": TODAY}, "cursor": "c1"}}});
    let batches = read(&mut source, Some(held));
    let log = log.lock().unwrap();
    assert_eq!(log.creates.len(), 1);
    assert_eq!(log.creates[0].1, Window { start: "2030-01-01".into(), end: TODAY.into() }, "the recreated run covers the held window");
    assert_eq!(log.pages, vec![("a1".to_string(), None), ("a1".to_string(), Some("c1".to_string()))], "the recreated run restarts from its first page");
    assert_eq!(rows(&batches).len(), 2);
    assert_eq!(batches.last().unwrap()["cursor"]["partitions"][key("a1")]["watermark"], json!(TODAY));
}

#[test]
fn a_vendor_answering_expired_again_recreates_once_per_read_and_holds_the_partition() {
    let (vendor, log) = Vendor::new(vec![
        ("a1", Script { pages: pages(1), polls: VecDeque::from(vec![RunState::Expired; 50]), ..Script::default() }),
        ("a2", Script { pages: pages(1), ..Script::default() }),
    ]);
    let mut source = ReportSource::new(config(&["a1", "a2"], json!({})), vendor, TODAY).unwrap();
    let batches = read(&mut source, None);
    let creates: Vec<String> = log.lock().unwrap().creates.iter().map(|(a, _)| a.clone()).collect();
    assert_eq!(creates.iter().filter(|a| *a == "a1").count(), 2, "one creation and one recreation");
    let position = &batches.last().unwrap()["cursor"]["partitions"];
    assert_eq!(position[key("a1")]["run"]["id"], json!("run-2"), "the partition holds its committed run");
    assert_eq!(position[key("a2")]["watermark"], json!(TODAY));
    let audit: Vec<Value> = batches.iter().flat_map(|b| b["audit"].as_array().cloned().unwrap_or_default()).collect();
    assert_eq!(audit.len(), 1, "{audit:?}");
    assert!(audit[0].as_str().unwrap().contains(&key("a1")), "{audit:?}");
}

#[test]
fn an_exhausted_polling_budget_ends_the_read_successfully() {
    let polls = VecDeque::from(vec![RunState::Pending; 10]);
    let (vendor, log) = Vendor::new(vec![("a1", Script { pages: pages(1), polls, ..Script::default() })]);
    let mut source = ReportSource::new(config(&["a1"], json!({"poll_budget": 3})), vendor, TODAY).unwrap();
    let batches = read(&mut source, None);
    let last = batches.last().unwrap();
    assert_eq!(last["more"], json!(false));
    assert!(rows(&batches).is_empty());
    assert_eq!(last["cursor"]["partitions"][key("a1")]["run"]["id"], json!("run-1"), "the pending run stays committed");
    assert_eq!(log.lock().unwrap().polls, 3);
}

// spec: connector.source.page-drain@791759b5
#[test]
fn a_read_drains_a_fixed_page_count_one_batch_per_page() {
    let (vendor, _log) = Vendor::new(vec![("a1", Script { pages: pages(5), ..Script::default() })]);
    let mut source = ReportSource::new(config(&["a1"], json!({"pages_per_read": 3})), vendor, TODAY).unwrap();
    let batches = read(&mut source, None);
    let paged: Vec<&Value> = batches.iter().filter(|b| !b["rows"].as_array().unwrap().is_empty()).collect();
    assert_eq!(paged.len(), 3);
    for (i, batch) in paged.iter().enumerate() {
        assert_eq!(batch["rows"].as_array().unwrap().len(), 1);
        assert_eq!(batch["cursor"]["partitions"][key("a1")]["cursor"], json!(format!("c{}", i + 1)));
    }
    assert_eq!(batches.last().unwrap()["more"], json!(false));
}

// spec: connector.source.page-cursor-missing@18e97cfe
#[test]
fn an_announced_page_naming_no_cursor_is_refused() {
    let unnamed = json!({"data": [{"ad_id": "x", "date_start": "2030-01-05"}], "has_more": true});
    let (vendor, _log) = Vendor::new(vec![("a1", Script { pages: vec![unnamed], ..Script::default() })]);
    let mut source = ReportSource::new(config(&["a1"], json!({})), vendor, TODAY).unwrap();
    let created = pull(&mut source, None).unwrap();
    let failure = pull(&mut source, created.get("cursor").cloned()).unwrap_err();
    assert!(failure.message.contains("ConnectorPageCursorMissing"), "{failure}");
    assert!(failure.deterministic);
}

#[test]
fn a_next_link_names_the_cursor() {
    let first = json!({"data": [{"ad_id": "x", "date_start": "2030-01-05"}], "has_more": true, "paging": {"next": "https://vendor.example/report?limit=2&after=c1"}});
    let last = json!({"data": [{"ad_id": "y", "date_start": "2030-01-05"}], "has_more": false});
    let (vendor, log) = Vendor::new(vec![("a1", Script { pages: vec![first, last], ..Script::default() })]);
    let mut source = ReportSource::new(config(&["a1"], json!({})), vendor, TODAY).unwrap();
    assert_eq!(rows(&read(&mut source, None)).len(), 2);
    assert_eq!(log.lock().unwrap().pages[1].1.as_deref(), Some("c1"));
}

// spec: connector.source.report-watermark@ca5bc1ef
#[test]
fn watermarks_are_held_per_partition_in_one_versioned_position() {
    let config = ReportConfig::parse(&json!({
        "partitions": [
            {"account": "a1", "grain": "day", "breakdown": "age", "split": {"region": "eu"}},
            {"account": "a1", "grain": "day", "breakdown": "age", "split": {"region": "us"}},
        ],
        "metrics": ["spend"], "entity": "ad_id", "date": "date_start",
    }))
    .unwrap();
    let (vendor, _log) = Vendor::new(vec![("a1", Script { pages: pages(1), ..Script::default() })]);
    let mut source = ReportSource::new(config.clone(), vendor, TODAY).unwrap();
    let position = read(&mut source, None).last().unwrap()["cursor"].clone();
    assert_eq!(position["version"], json!(1));
    let held = position["partitions"].as_object().unwrap();
    let keys: Vec<String> = config.partitions.iter().map(Partition::key).collect();
    assert_eq!(held.len(), 2);
    assert_ne!(keys[0], keys[1]);
    for k in &keys {
        assert_eq!(held[k]["watermark"], json!(TODAY), "{k}");
    }
}

// spec: connector.source.partition-isolation@e11cae37
#[test]
fn a_retryable_failure_on_one_partition_keeps_the_pages_others_drained() {
    let fault = VendorFault { status: 503, body: json!({}) };
    let (vendor, _log) = Vendor::new(vec![
        ("a1", Script { pages: pages(2), fault_at: Some((1, fault)), ..Script::default() }),
        ("a2", Script { pages: pages(2), ..Script::default() }),
    ]);
    let mut source = ReportSource::new(config(&["a1", "a2"], json!({})), vendor, TODAY).unwrap();
    let batches = read(&mut source, None);
    let position = &batches.last().unwrap()["cursor"]["partitions"];
    assert_eq!(position[key("a1")]["cursor"], json!("c1"), "the failed partition holds its committed position");
    assert_eq!(position[key("a1")]["watermark"], Value::Null);
    assert_eq!(position[key("a2")]["watermark"], json!(TODAY));
    let landed = rows(&batches);
    assert_eq!(landed.iter().filter(|r| r["report_partition"] == json!(key("a2"))).count(), 2);
    assert_eq!(landed.iter().filter(|r| r["report_partition"] == json!(key("a1"))).count(), 1);
    let audit: Vec<Value> = batches.iter().flat_map(|b| b["audit"].as_array().cloned().unwrap_or_default()).collect();
    assert_eq!(audit.len(), 1, "the held partition is recorded on the run's audit: {audit:?}");
    let entry = audit[0].as_str().unwrap();
    assert!(entry.contains(&key("a1")) && entry.contains("503"), "{entry}");
}

// spec: connector.source.metric-grain@2642031a
#[test]
fn a_metric_at_an_estimated_grain_is_refused() {
    let declare = |grain: &str| {
        ReportConfig::parse(&json!({
            "partitions": [{"account": "a1", "grain": grain, "breakdown": "age"}],
            "metrics": ["spend", "reach"], "estimated": {"reach": ["week", "month"]},
            "entity": "ad_id", "date": "date_start",
        }))
    };
    let refused = declare("week").unwrap_err();
    assert!(refused.to_string().contains("ConnectorMetricGrainRejected"), "{refused}");
    assert!(refused.to_string().contains("reach"), "{refused}");
    assert!(declare("day").is_ok());
}

// spec: connector.source.restated-window@110510d2
#[test]
fn a_caught_up_partition_reopens_its_lookback_window() {
    let (vendor, log) = Vendor::new(vec![("a1", Script { pages: pages(1), ..Script::default() })]);
    let mut source = ReportSource::new(config(&["a1"], json!({"lookback_days": 7})), vendor, TODAY).unwrap();
    let caught_up = json!({"version": 1, "partitions": {key("a1"): {"watermark": TODAY}}});
    read(&mut source, Some(caught_up));
    let window = log.lock().unwrap().creates[0].1.clone();
    assert_eq!(window, Window { start: "2030-01-03".into(), end: TODAY.into() });
}

#[test]
fn the_pull_window_is_at_least_the_lookback_and_row_keys_are_deterministic() {
    let rows_of = |breakdown: &str| {
        let row = json!({"ad_id": "ad-1", "date_start": "2030-01-05", "age": breakdown});
        let (vendor, log) = Vendor::new(vec![("a1", Script { pages: vec![page(0, 1, vec![row])], ..Script::default() })]);
        let mut source = ReportSource::new(config(&["a1"], json!({"lookback_days": 7, "window_days": 3})), vendor, TODAY).unwrap();
        let landed = rows(&read(&mut source, None));
        let window = log.lock().unwrap().creates[0].1.clone();
        (landed, window)
    };
    let (first, window) = rows_of("18-24");
    assert_eq!(window, Window { start: "2030-01-03".into(), end: TODAY.into() });
    let (again, _) = rows_of("18-24");
    let (other, _) = rows_of("25-34");
    let k = first[0]["report_key"].as_str().unwrap();
    assert_eq!(k.len(), 64);
    assert_eq!(first[0]["report_key"], again[0]["report_key"]);
    assert_ne!(first[0]["report_key"], other[0]["report_key"], "the breakdown enters the key");
}

// spec: connector.source.report-event-time@28f6968d
#[test]
fn a_row_event_time_is_midnight_utc_of_its_window_start_day() {
    let landed = |date: Value| {
        let row = json!({"ad_id": "ad-1", "date_start": date});
        let (vendor, _log) = Vendor::new(vec![("a1", Script { pages: vec![page(0, 1, vec![row])], ..Script::default() })]);
        let mut source = ReportSource::new(config(&["a1"], json!({})), vendor, TODAY).unwrap();
        rows(&read(&mut source, None))[0]["event_time"].clone()
    };
    assert_eq!(landed(json!("2030-01-05")), json!("2030-01-05T00:00:00Z"));
    assert_eq!(landed(json!("2030-02-30")), Value::Null);
    assert_eq!(landed(json!("soon")), Value::Null);
    assert_eq!(landed(Value::Null), Value::Null);
}

// spec: connector.source.ordering-column@cddfe23b
#[test]
fn ordering_by_the_ingest_stamp_is_refused() {
    let declare = |order_by: &str| {
        ReportConfig::parse(&json!({
            "partitions": [{"account": "a1", "grain": "day", "breakdown": "age"}],
            "metrics": ["spend"], "entity": "ad_id", "date": "date_start", "order_by": order_by,
        }))
    };
    let refused = declare(contextful_core::store::reserve::INGESTED_AT).unwrap_err();
    assert!(refused.to_string().contains("ConnectorOrderingColumnRejected"), "{refused}");
    assert!(declare("date_start").is_ok());
}

// spec: connector.source.field-drift@4205f82c
#[test]
fn report_rows_stay_untyped_json_and_a_throttle_code_in_the_body_is_recognized() {
    let row = json!({"ad_id": "ad-1", "date_start": "2030-01-05", "actions": [{"type": "click", "value": "3"}], "new_field": {"nested": true}});
    let (vendor, _log) = Vendor::new(vec![("a1", Script { pages: vec![page(0, 1, vec![row.clone()])], ..Script::default() })]);
    let mut source = ReportSource::new(config(&["a1"], json!({})), vendor, TODAY).unwrap();
    let batches = read(&mut source, None);
    assert_eq!(batches.len(), 3, "the creation, the page and the closing pull");
    assert!(batches.iter().all(|b| b.get("types").is_none()), "no column types are declared");
    let landed = &rows(&batches)[0];
    assert_eq!(landed["actions"], row["actions"]);
    assert_eq!(landed["new_field"], row["new_field"]);

    let throttled = |fault: VendorFault| {
        let (vendor, _log) = Vendor::new(vec![
            ("a1", Script { pages: pages(1), fault_at: Some((0, fault)), ..Script::default() }),
            ("a2", Script { pages: pages(1), ..Script::default() }),
        ]);
        let mut source = ReportSource::new(config(&["a1", "a2"], json!({"throttle_codes": [17]})), vendor, TODAY).unwrap();
        read(&mut source, None).last().unwrap()["cursor"]["partitions"].clone()
    };
    for fault in [
        VendorFault { status: 400, body: json!({"error": {"code": 17}}) },
        VendorFault { status: 403, body: json!({"error": {"code": 17}}) },
        VendorFault { status: 429, body: json!({}) },
    ] {
        let held = throttled(fault);
        assert_eq!(held[key("a1")]["watermark"], Value::Null);
        assert_eq!(held[key("a2")]["watermark"], json!(TODAY));
    }
    for status in [400, 401, 403] {
        let fault = VendorFault { status, body: json!({"error": {"code": 17}}) };
        assert_eq!(ReportSource::<Vendor>::classify(&config(&["a1"], json!({"throttle_codes": [17]})), &fault).tag, FailureTag::RateLimited, "{status}");
    }
}

// spec: connector.source.report-auth@f3a74c26
#[test]
fn an_authentication_failure_is_terminal() {
    for status in [401, 403] {
        let fault = VendorFault { status, body: json!({}) };
        let (vendor, _log) = Vendor::new(vec![
            ("a1", Script { pages: pages(1), create_fault: Some(fault), ..Script::default() }),
            ("a2", Script { pages: pages(1), ..Script::default() }),
        ]);
        let mut source = ReportSource::new(config(&["a1", "a2"], json!({})), vendor, TODAY).unwrap();
        let failure = pull(&mut source, None).unwrap_err();
        assert!(failure.message.contains("ConnectorAuthTerminal"), "{failure}");
        assert!(failure.deterministic, "an auth failure spends no retry budget and stops the read");
    }
}
