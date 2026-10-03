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
fn m10_cadence() {
    let cf = bin("contextful");
    let vendor = Server::start(|_| Response::json(200, "[{\"id\":\"a\"}]"));
    let p = GitRepo::init();
    p.write(&format!("{STORE}/config.toml"), "[node]\nid = \"ingest-a\"\n");
    p.write(
        "contextful.toml",
        &format!(
            "site_id = \"site-a\"\nauthoring_posture = \"per_request\"\n\n[control]\npool = 2\n\n{}{}{}",
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

    // A published hostname is probed for the posture its descriptor declares.
    let ci = bin("contextful-ci");
    let docs = Server::start(|_| Response::json(200, "{}"));
    let admin = Server::start(|_| Response::json(302, ""));
    p.write("deploy/hostnames/docs.toml", &descriptor("docs.example.org", "public"));
    p.write("deploy/hostnames/admin.toml", &descriptor("admin.example.org", "access"));
    p.write(
        "deploy/probe.toml",
        "[[probe]]\nhostname = \"docs.example.org\"\ngate = \"public\"\n\n[[probe]]\nhostname = \"admin.example.org\"\ngate = \"access\"\n",
    );
    let resolve = [format!("docs.example.org={}", docs.url("")), format!("admin.example.org={}", admin.url(""))];
    let probe = |resolve: &[String]| {
        let mut args = vec!["deploy", "probe"];
        for r in resolve {
            args.extend(["--resolve", r.as_str()]);
        }
        p.run(&ci, &args)
    };
    ok(&probe(&resolve));
    assert_eq!(docs.received("/").len(), 1);
    assert!(docs.received("/")[0].header("authorization").is_none());

    // A hostname declaring `public` that answers 302 fails the probe, naming all three.
    let resolve = [format!("docs.example.org={}", admin.url("")), format!("admin.example.org={}", admin.url(""))];
    let refused = probe(&resolve);
    assert!(!refused.status.success());
    let err = String::from_utf8_lossy(&refused.stderr);
    for needle in ["HostnamePostureMismatch", "docs.example.org", "public", "302"] {
        assert!(err.contains(needle), "{needle} missing from: {err}");
    }

    // An unmodelled descriptor key is refused before any probe.
    p.write("deploy/hostnames/docs.toml", &format!("{}cache = true\n", descriptor("docs.example.org", "public")));
    let refused = probe(&resolve);
    assert!(String::from_utf8_lossy(&refused.stderr).contains("DescriptorUnknownField"));
}

fn descriptor(hostname: &str, gate: &str) -> String {
    format!("version = 1\nhostname = \"{hostname}\"\nworker = \"gateway\"\ngate = \"{gate}\"\nacknowledged = true\n")
}
