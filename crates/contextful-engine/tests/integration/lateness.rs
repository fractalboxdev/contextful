//! `run.advance.allowed-lateness`: the window a `monotonic` poll re-reads behind its
//! stored position.

use crate::support::{ids, plan, Rig, Sink};
use contextful_core::run::advance::{rewind, ALLOWED_LATENESS_DEFAULT_SECS};
use contextful_core::run::plan::Plan;
use contextful_core::run::ports::{Cancellation, PullRequest, Source};
use contextful_core::run::Failure;
use serde_json::{json, Value};

/// A vendor serving every row whose `seen_at` instant is at or after the asked position.
struct Feed {
    rows: Vec<Value>,
    asked: Vec<Option<Value>>,
}

impl Source for Feed {
    fn pull(&mut self, request: &PullRequest, _: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        self.asked.push(request.position.clone());
        let from = request.position.as_ref().and_then(|p| p.get("at")).and_then(Value::as_str).unwrap_or("");
        let rows: Vec<&Value> = self.rows.iter().filter(|r| r["seen_at"].as_str().unwrap() >= from).collect();
        Ok(serde_json::to_vec(&json!({ "rows": rows, "more": false })).unwrap())
    }
}

fn polled(lateness: Option<&str>) -> Plan {
    let window = lateness.map(|l| format!("\nallowed_lateness = \"{l}\"")).unwrap_or_default();
    plan(&format!("kind = \"monotonic\"\nfield = \"seen_at\"{window}"), "")
}

/// A table declares `allowed_lateness`, default 0 s; a `monotonic` poll re-reads from its stored position minus
/// that window, so a row arriving that late still lands.
#[test]
fn a_poll_re_reads_the_declared_lateness_behind_its_position() {
    assert_eq!(ALLOWED_LATENESS_DEFAULT_SECS, 0);
    assert_eq!(polled(None).spec.cursor.allowed_lateness_secs().unwrap(), 0, "an undeclared window is 0 s");
    assert_eq!(polled(Some("2m")).spec.cursor.allowed_lateness_secs().unwrap(), 120);
    assert!(Plan::compile(b"pipeline = \"feed\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1.0.0\"\ncommand = [\"vendor\"]\n[cursor]\nkind = \"monotonic\"\nfield = \"seen_at\"\nallowed_lateness = \"soon\"\n").is_err(), "a window no span spells refuses at compile");
    assert_eq!(rewind(&json!("2030-01-01T00:10:00Z"), 120).unwrap(), json!("2030-01-01T00:08:00Z"));
    assert_eq!(rewind(&json!(900), 120).unwrap(), json!(780));

    for (lateness, relands) in [(None, false), (Some("2m"), true)] {
        let rig = Rig::new();
        let p = polled(lateness);
        let mut feed = Feed { rows: vec![json!({"id": "a", "seen_at": "2030-01-01T00:10:00Z"})], asked: Vec::new() };
        let mut sink = Sink::default();
        rig.run(&p, "1.0.0", "run-1", &mut feed, &mut sink).unwrap();
        // A row stamped 90 s before the frontier reaches the vendor after the first poll.
        feed.rows.push(json!({"id": "late", "seen_at": "2030-01-01T00:08:30Z"}));
        rig.run(&p, "1.0.0", "run-2", &mut feed, &mut sink).unwrap();
        let landed: Vec<String> = sink.commits.iter().flat_map(|c| ids(c).concat()).collect();
        assert!(!landed.is_empty(), "the polls landed rows");
        assert_eq!(landed.contains(&"late".to_string()), relands, "lateness {lateness:?} decides whether the late row lands: {landed:?}");
        let second = feed.asked[1].clone().unwrap();
        let asked_from = if relands { "2030-01-01T00:08:00Z" } else { "2030-01-01T00:10:00Z" };
        assert_eq!(second, json!({"field": "seen_at", "at": asked_from}), "the poll asks from the position less its window");
        assert_eq!(rig.catalog().cursor("feed", "filings").unwrap().position, Some(json!({"field": "seen_at", "at": "2030-01-01T00:10:00Z"})), "the stored position never rewinds");
    }
}
