//! Milestone 10 — cadence and the operator plane.
//!
//! Reach: due work dispatches into a bounded pool, and a published hostname is probed for
//! the posture it declares.

use contextful_acceptance::http::{Response, Server};
use contextful_acceptance::{bin, GitRepo};
use serde_json::{json, Value};
use std::process::Output;

const STORE: &str = ".contextful/context/research";

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn scheduled(id: &str, endpoint: &str, schedule: &str) -> String {
    format!(
        "[[pipeline]]\nid = \"{id}\"\nschedule = \"{schedule}\"\ntables = [\"items\"]\n\n[pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"{endpoint}\" }}\n\n"
    )
}

#[test]
#[ignore = "milestone 10 is open: no published hostname is probed for the posture it declares"]
fn m10_cadence() {
    let cf = bin("contextful");
    let vendor = Server::start(|_| Response::json(200, "[{\"id\":\"a\"}]"));
    let p = GitRepo::init();
    p.write(&format!("{STORE}/config.toml"), "[node]\nid = \"ingest-a\"\n");
    p.write(
        "contextful.toml",
        &format!(
            "site_id = \"site-a\"\n\n[control]\npool = 2\n\n{}{}{}",
            scheduled("filings", &vendor.url("/v1/filings"), "every 1h"),
            scheduled("orders", &vendor.url("/v1/orders"), "every 1h"),
            scheduled("returns", &vendor.url("/v1/returns"), "0 3 * * *"),
        ),
    );
    let cycle = |now: &str| -> Value {
        serde_json::from_str(&ok(&p.run(&cf, &["pipeline", "serve", "--cycle", "--project", "research", "--now", now]))).unwrap()
    };

    // Applying claims a version and fires nothing.
    assert!(ok(&p.run(&cf, &["pipeline", "apply", "--project", "research"])).contains("applied v1"));
    assert!(vendor.received("/v1/filings").is_empty());

    // Due work dispatches into a pool of two; the third due unit stays pending.
    let first = cycle("2030-01-01T00:00:00Z");
    assert_eq!(first["fired"], json!(["filings", "orders"]));
    assert_eq!(first["pending"], json!(["returns"]));
    assert_eq!(cycle("2030-01-01T00:00:00Z")["fired"], json!(["returns"]));

    // A daemon down for six hours fires each hourly pipeline once on its return.
    let back = cycle("2030-01-01T06:00:00Z");
    assert_eq!(back["fired"], json!(["filings", "orders"]));
    assert_eq!(vendor.received("/v1/filings").len(), 2);
    let rest = cycle("2030-01-01T06:00:00Z");
    assert_eq!(rest["fired"], json!(["returns"]));
    assert_eq!(rest["next_due"], "2030-01-01T07:00:00Z");
    assert_eq!(cycle("2030-01-01T06:00:00Z")["fired"], json!([]));
}
