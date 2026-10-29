//! Synthetic source and receiver fixtures exercise native contracts through the CLI.

use super::{failed, Fixture, Result, Running};
use crate::delegate::{request_signed, signed_answer};
use crate::http::{Request, Response, Server};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const PROJECT: &str = "research";
const AUDIENCE: &str = "contextful://conformance";

pub(super) fn run(id: &str, fixture: &Fixture) -> Result<()> {
    match id {
        "pagination-checkpoint" => paged(fixture, false),
        "hard-crash-replay" => paged(fixture, true),
        "graceful-restart" => graceful(fixture),
        "empty-replacement" => replacement(fixture),
        "applied-scheduling" => scheduling(fixture),
        "public-identity-masking" => masking(fixture),
        "typed-export-acknowledgement" => export(fixture),
        "coordinated-group-publication" => group(fixture),
        "delegated-item-fence" => fenced(fixture),
        _ => Err("ConformanceScenarioUnknown".into()),
    }
}

fn require(condition: bool, diagnostic: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(diagnostic.into())
    }
}

/// Whether the fixture's state lives behind the `cloudflare-d1` profile's bindings.
fn on_d1(f: &Fixture) -> bool {
    f.remote.as_ref().is_some_and(|remote| {
        remote.environment.iter().any(|(key, value)| key == "CONTEXTFUL_STORAGE_PROFILE" && value == "cloudflare-d1")
    })
}

fn rows(value: &Value) -> Result<&Vec<Value>> {
    value["rows"]
        .as_array()
        .ok_or_else(|| "ConformanceRowsMalformed".into())
}

fn scheduled(endpoint: &str, extra: &str) -> String {
    format!("site_id = \"conformance\"\nauthoring_posture = \"per_request\"\n[[pipeline]]\nid = \"feed\"\nschedule = \"every 1h\"\ntables = [{{ name = \"items\", primary_key = [\"id\"]{extra} }}]\n[pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"{endpoint}\" }}\n")
}

fn import(f: &Fixture) -> Result<String> {
    let public = f.ok(
        &["token", "keygen", "--out", ".contextful/issuer.seed"],
        &[],
    )?;
    let admin = f.ok(
        &[
            "token",
            "mint",
            "--issuer-key",
            ".contextful/issuer.seed",
            "--on-behalf-of",
            "svc://conformance-control",
            "--action",
            "admin",
            "--table",
            "*",
            "--ttl",
            "3600",
        ],
        &[],
    )?;
    f.ok(
        &[
            "pipeline",
            "import",
            "--project",
            PROJECT,
            "--issuer-key",
            ".contextful/issuer.seed",
            "--public-key",
            public.trim(),
            "--audience",
            AUDIENCE,
        ],
        &[("CONTEXTFUL_TOKEN", admin.trim())],
    )?;
    Ok(public.trim().into())
}

fn fire(f: &Fixture, run: &str, now: &str) -> Result<String> {
    f.ok(
        &[
            "pipeline",
            "run",
            "feed",
            "--project",
            PROJECT,
            "--run-id",
            run,
            "--site-id",
            "conformance",
            "--now",
            now,
        ],
        &[],
    )
}

// The protocol and crash point match the milestone-three acceptance fixture.
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

fn paged(f: &Fixture, crash: bool) -> Result<()> {
    f.write("contextful.toml", "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"items\"\nprimary_key = [\"id\"]\n")?;
    f.write("vendor.sh", VENDOR)?;
    f.write("plan.toml", "pipeline = \"feed\"\ntable = \"items\"\n[connector]\nid = \"vendor\"\nversion = \"1.0.0\"\nworld = \"native\"\ncommand = [\"sh\", \"vendor.sh\"]\n[cursor]\nkind = \"opaque-token\"\n")?;
    if crash {
        f.write("crash-armed", "armed")?;
    }
    let start = |id: &str, now: &str| {
        f.invoke(
            &[
                "run",
                "start",
                "--plan",
                "plan.toml",
                "--project",
                PROJECT,
                "--run-id",
                id,
                "--site-id",
                "conformance",
                "--now",
                now,
            ],
            &[],
        )
    };
    let first = start("capture-1", "2030-01-01T00:00:00Z")?;
    if crash {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            require(
                first.status.signal() == Some(9),
                "ConformanceHardCrashNotObserved",
            )?;
        }
        #[cfg(not(unix))]
        return Err("ConformanceHardCrashUnsupported".into());
        let state: Value =
            serde_json::from_str(&f.ok(&["run", "show", "capture-1", "--project", PROJECT], &[])?)
                .map_err(|_| "ConformanceRunMalformed")?;
        require(
            state["status"] == "running",
            "ConformanceCrashRecordMissing",
        )?;
        f.replace_local_state()?;
        if on_d1(f) {
            let attempts = std::fs::read_to_string(f.dir.path().join("effects.log"))
                .map_err(|_| "ConformanceEffectsMissing")?;
            let key = attempts.lines().find_map(|line| line.strip_prefix("page=\"p2\" key="))
                .filter(|key| !key.is_empty()).ok_or("ConformanceRetryIdentityMissing")?;
            let state: Value = serde_json::from_str(&f.ok(&["storage", "guard", "--project", PROJECT], &[])?)
                .map_err(|_| "ConformanceGuardReportMalformed")?;
            let token = state["guards"][0]["token"].as_str().ok_or("ConformanceGuardMissing")?.to_string();
            // The killed holder records no exit, so its guard reconciles once its owner lease
            // lapses, against a receipt carrying the key its request sent.
            let evidence = format!("receipt:{key}:effects-log");
            let deadline = Instant::now() + f.timeout;
            loop {
                let reconciled = f.invoke(&["storage", "reconcile-guard", "--project", PROJECT, "--token", &token,
                    "--evidence", &evidence, "--operator", "conformance"], &[])?;
                if reconciled.status.success() {
                    break;
                }
                let held = reconciled.stderr.contains("ExecutionGuardHeld");
                if !held || Instant::now() >= deadline {
                    return Err(failed("ConformanceGuardReconcileFailed", &["storage"], &reconciled.stderr));
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
        let deadline = Instant::now() + f.timeout;
        let mut attempt = 0;
        loop {
            attempt += 1;
            let resumed = start(&format!("capture-resume-{attempt}"), "2030-01-01T00:01:00Z")?;
            if resumed.status.success() {
                break;
            }
            if f.remote.is_none() || Instant::now() >= deadline {
                return Err(failed("ConformanceResumeFailed", &["run"], &resumed.stderr));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    } else if !first.status.success() {
        return Err(failed("ConformanceCaptureFailed", &["run"], &first.stderr));
    }
    let answer = f.query("SELECT id FROM items ORDER BY id")?;
    require(
        rows(&answer)? == &[json!(["a"]), json!(["b"]), json!(["c"])],
        "ConformanceCursorSkipOrDuplicateIdentity",
    )?;
    let log = std::fs::read_to_string(f.dir.path().join("effects.log"))
        .map_err(|_| "ConformanceEffectsMissing")?;
    let pages: Vec<_> = log.lines().collect();
    require(
        pages
            .iter()
            .filter(|l| l.starts_with("page=start "))
            .count()
            == 1,
        "ConformanceRecordedCallRepeated",
    )?;
    let retry: Vec<_> = pages
        .iter()
        .filter(|l| l.starts_with("page=\"p2\" "))
        .collect();
    require(
        retry.len() == if crash { 2 } else { 1 },
        "ConformanceRetryCountMismatch",
    )?;
    if crash {
        require(retry[0] == retry[1], "ConformanceRetryIdentityChanged")?;
        let charges = std::fs::read_to_string(f.dir.path().join("charges.log"))
            .map_err(|_| "ConformanceChargesMissing")?;
        require(charges.lines().filter(|line| line.starts_with("page=\"p2\" ")).count() == 1,
            "ConformanceAmbiguousCallBilledTwice")?;
    }
    let resumed = start("capture-3", "2030-01-01T00:02:00Z")?;
    if !resumed.status.success() {
        return Err(failed("ConformanceCheckpointResumeFailed", &["run"], &resumed.stderr));
    }
    let after = f.query("SELECT id FROM items ORDER BY id")?;
    require(
        after["rows"] == answer["rows"],
        "ConformanceCheckpointRewinds",
    )?;
    let log = std::fs::read_to_string(f.dir.path().join("effects.log"))
        .map_err(|_| "ConformanceEffectsMissing")?;
    require(
        log.lines().filter(|l| l.starts_with("page=start ")).count() == 1,
        "ConformanceCheckpointRewinds",
    )?;
    require(
        log.lines()
            .filter(|l| l.starts_with("page=\"done\" "))
            .count()
            == 1,
        "ConformanceCheckpointNotAdvanced",
    )
}

fn replacement(f: &Fixture) -> Result<()> {
    let capture = Arc::new(Mutex::new(json!([{"id":"a"},{"id":"b"}])));
    let state = capture.clone();
    let source = Server::start(move |_| Response::json(200, &state.lock().unwrap().to_string()));
    f.write(
        "contextful.toml",
        &scheduled(&source.url("/items"), ", write_mode = \"replace\""),
    )?;
    fire(f, "replace-1", "2030-01-01T00:00:00Z")?;
    require(
        rows(&f.query("SELECT id FROM feed_items ORDER BY id")?)?.len() == 2,
        "ConformanceInitialCaptureMissing",
    )?;
    *capture.lock().unwrap() = json!([{"id":"c"}]);
    fire(f, "replace-2", "2030-01-01T00:01:00Z")?;
    require(
        f.query("SELECT id FROM feed_items ORDER BY id")?["rows"] == json!([["c"]]),
        "ConformanceReplacementRetainsRows",
    )?;
    *capture.lock().unwrap() = json!([]);
    fire(f, "replace-3", "2030-01-01T00:02:00Z")?;
    require(
        rows(&f.query("SELECT id FROM feed_items")?)?.is_empty(),
        "ConformanceEmptyCaptureRetainsRows",
    )
}

fn scheduling(f: &Fixture) -> Result<()> {
    let source = Server::start(|_| Response::json(200, "[{\"id\":\"applied\"}]"));
    let manifest = format!("{}\n[[model]]\nid = \"summary\"\nsql = \"SELECT id FROM feed_items\"\nunique_key = [\"id\"]\n[model.contract]\nversion = \"1.0.0\"\ncolumns = [{{ name = \"id\", type = \"utf8\", nullable = false }}]\n[[job]]\nname = \"refresh-summary\"\nkind = \"build\"\ntarget = \"summary\"\nafter = \"feed\"\n", scheduled(&source.url("/applied"), ""));
    f.write("contextful.toml", &manifest)?;
    let public = import(f)?;
    // The editable document is not the applied execution identity.
    f.write(
        "contextful.toml",
        &manifest.replace("/applied", "/unapplied"),
    )?;
    let first: Value = serde_json::from_str(&f.ok(
        &[
            "pipeline",
            "serve",
            "--cycle",
            "--project",
            PROJECT,
            "--now",
            "2030-01-01T00:00:00Z",
            "--public-key",
            &public,
        ],
        &[],
    )?)
    .map_err(|_| "ConformanceCycleMalformed")?;
    require(
        first["fired"]
            .as_array()
            .is_some_and(|fired| fired.contains(&json!("feed"))),
        "ConformanceAppliedNotScheduled",
    )?;
    f.ok(
        &[
            "pipeline",
            "serve",
            "--cycle",
            "--project",
            PROJECT,
            "--now",
            "2030-01-01T00:00:00Z",
            "--public-key",
            &public,
        ],
        &[],
    )?;
    require(
        !source.received("/applied").is_empty(),
        "ConformanceAppliedSnapshotUnused",
    )?;
    require(
        source.received("/applied").len() == 1,
        "ConformanceScheduleRepeatedWithinWindow",
    )?;
    require(
        source.received("/unapplied").is_empty(),
        "ConformanceUnappliedDocumentExecuted",
    )?;
    require(
        f.query("SELECT id FROM summary")?["rows"] == json!([["applied"]]),
        "ConformanceDependencyOutputMissing",
    )
}

fn graceful(f: &Fixture) -> Result<()> {
    let source = Server::start(|_| Response::json(200, "[{\"id\":\"stable\"}]"));
    f.write("contextful.toml", &scheduled(&source.url("/items"), ""))?;
    let public = import(f)?;
    let mut daemon = Running::spawn(f.command(
        &[
            "pipeline",
            "serve",
            "--project",
            PROJECT,
            "--public-key",
            &public,
        ],
        &[],
    ))?;
    let deadline = Instant::now() + f.timeout;
    // A read before the daemon lands its table fails; one still failing at the deadline
    // names its own cause rather than a timeout.
    let mut polled = None;
    loop {
        if !source.received("/items").is_empty() {
            match f.query("SELECT id FROM feed_items") {
                Ok(answer) if answer["rows"] == json!([["stable"]]) => break,
                Ok(_) => polled = None,
                Err(error) => polled = Some(error),
            }
        }
        require(
            daemon
                .child
                .try_wait()
                .map_err(|_| "ConformanceProcessUnavailable")?
                .is_none(),
            "ConformanceDaemonExited",
        )?;
        if Instant::now() >= deadline {
            return Err(polled.unwrap_or_else(|| "ConformanceDaemonTimeout".into()));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let signal = Command::new("/bin/kill")
        .args(["-TERM", &daemon.child.id().to_string()])
        .status()
        .map_err(|_| "ConformanceSignalUnavailable")?;
    require(signal.success(), "ConformanceSignalUnavailable")?;
    let status = loop {
        if let Some(status) = daemon
            .child
            .try_wait()
            .map_err(|_| "ConformanceProcessUnavailable")?
        {
            break status;
        }
        require(Instant::now() < deadline, "ConformanceGracefulStopTimeout")?;
        std::thread::sleep(Duration::from_millis(20));
    };
    require(status.success(), "ConformanceGracefulStopFailed")?;
    let calls = source.received("/items").len();
    f.ok(
        &[
            "pipeline",
            "serve",
            "--cycle",
            "--project",
            PROJECT,
            "--public-key",
            &public,
        ],
        &[],
    )?;
    require(
        source.received("/items").len() == calls,
        "ConformanceGracefulRestartRepeatedCall",
    )?;
    require(
        f.query("SELECT id FROM feed_items")?["rows"] == json!([["stable"]]),
        "ConformanceGracefulRestartLostRows",
    )
}

fn masking(f: &Fixture) -> Result<()> {
    let source = Server::start(|_| {
        Response::json(200, "[{\"id\":\"public-0001\",\"note\":\"x-api-key: sk_live_0123456789abcdefghij\"},{\"id\":\"public-0002\",\"note\":\"ordinary public text\"}]")
    });
    f.write("contextful.toml", &scheduled(&source.url("/items"), ""))?;
    fire(f, "masked", "2030-01-01T00:00:00Z")?;
    let answer = f.query("SELECT id, note FROM feed_items ORDER BY id")?;
    let actual = rows(&answer)?;
    require(
        actual.len() == 2 && actual[0][0] == "public-0001" && actual[1][0] == "public-0002",
        "ConformancePublicIdentityChanged",
    )?;
    require(
        actual[0][1]
            .as_str()
            .is_some_and(|note| note.contains("[REDACTED:secret]")),
        "ConformanceCredentialUnmasked",
    )?;
    require(
        actual[1][1] == "ordinary public text",
        "ConformancePublicValueMasked",
    )
}

fn export(f: &Fixture) -> Result<()> {
    #[derive(Default)]
    struct Sink {
        attempts: Vec<Value>,
        events: BTreeMap<String, Value>,
        active: BTreeMap<String, Value>,
    }
    let state = Arc::new(Mutex::new(Sink::default()));
    let target = state.clone();
    let sink = Server::start(move |request| {
        let Ok(batch) = serde_json::from_slice::<Value>(&request.body) else {
            return Response::json(400, "{}");
        };
        let mut sink = target.lock().unwrap();
        sink.attempts.push(batch.clone());
        let Some(events) = batch["events"].as_array() else {
            return Response::json(400, "{}");
        };
        let mut transaction = sink.events.clone();
        for event in events {
            let Some(id) = event["id"].as_str() else {
                return Response::json(400, "{}");
            };
            if transaction.get(id).is_some_and(|old| old != event) {
                return Response::json(409, "{}");
            }
            transaction.insert(id.into(), event.clone());
        }
        sink.events = transaction;
        if events
            .iter()
            .any(|event| event["kind"] == "publication_complete")
        {
            sink.active = sink.events.clone();
        }
        // The first committed transaction loses its acknowledgement.
        Response::json(if sink.attempts.len() == 1 { 503 } else { 200 }, "{}")
    });
    f.write("contextful.toml", &format!("authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"items\"\nprimary_key = [\"id\"]\n[[export]]\nname = \"mirror\"\ntable = \"items\"\nendpoint = \"{}\"\nformat = \"changes-v1\"\nkey = [\"id\"]\nschedule = \"every 30s\"\n", sink.url("/changes")))?;
    f.write(
        "rows.jsonl",
        "{\"id\":\"a\",\"value\":\"first\"}\n{\"id\":\"b\",\"value\":\"second\"}\n",
    )?;
    f.ok(
        &[
            "context",
            "land",
            "items",
            "--project",
            PROJECT,
            "--rows",
            "rows.jsonl",
            "--run-id",
            "load",
            "--site-id",
            "conformance",
        ],
        &[],
    )?;
    if f.remote.is_some() {
        f.ok(&["context", "compact", "items", "--project", PROJECT], &[])?;
        f.ok(&["sync", "push", "--project", PROJECT], &[])?;
    }
    let public = f.ok(
        &["token", "keygen", "--out", ".contextful/issuer.seed"],
        &[],
    )?;
    let token = f.ok(
        &[
            "token",
            "mint",
            "--issuer-key",
            ".contextful/issuer.seed",
            "--on-behalf-of",
            "svc://conformance",
            "--zone",
            "on-prem:hq",
            "--table",
            "items",
            "--ttl",
            "3600",
        ],
        &[],
    )?;
    let args = [
        "export",
        "run",
        "mirror",
        "--project",
        PROJECT,
        "--public-key",
        &public,
        "--audience",
        AUDIENCE,
    ];
    require(
        !f.invoke(&args, &[("CONTEXTFUL_TOKEN", &token)])?
            .status
            .success(),
        "ConformanceAcknowledgementBeforeCommit",
    )?;
    require(
        !state.lock().unwrap().active.is_empty(),
        "ConformanceReceiverCompletionMissing",
    )?;
    f.replace_local_state()?;
    f.ok(&args, &[("CONTEXTFUL_TOKEN", &token)])?;
    {
        let sink = state.lock().unwrap();
        require(sink.attempts.len() == 2, "ConformancePendingExportLost")?;
        require(
            sink.attempts[0] == sink.attempts[1],
            "ConformanceExportRetryChanged",
        )?;
        require(
            sink.active
                .values()
                .filter(|event| event["kind"] == "upsert")
                .count()
                == 2,
            "ConformanceReceiverRowsMissing",
        )?;
    }
    f.ok(&args, &[("CONTEXTFUL_TOKEN", &token)])?;
    let attempts = state.lock().unwrap().attempts.clone();
    if f.remote.is_some() {
        require(attempts.len() <= 3, "ConformanceAcknowledgedExportRepeated")?;
        for batch in &attempts[2..] {
            let Some(events) = batch["events"].as_array() else {
                return Err("ConformanceExportRetryChanged".into());
            };
            require(events.len() == 1 && events[0]["kind"] == "publication_complete" && events[0]["changes"] == 0, "ConformanceAcknowledgedExportRepeated")?;
        }
    } else {
        require(attempts.len() == 2, "ConformanceAcknowledgedExportRepeated")?;
    }
    f.ok(&args, &[("CONTEXTFUL_TOKEN", &token)])?;
    let stable = state.lock().unwrap().attempts == attempts;
    require(stable, "ConformanceAcknowledgedExportRepeated")
}

/// A receiver stages each group's events and activates a group only at its complete barrier.
#[derive(Default)]
struct GroupReceiver {
    bodies: Vec<Vec<u8>>,
    staged: BTreeMap<String, Vec<Value>>,
    active: Option<(Value, Vec<Value>)>,
    activations: usize,
}

impl GroupReceiver {
    fn receive(&mut self, body: &[u8]) -> u16 {
        let Ok(batch) = serde_json::from_slice::<Value>(body) else {
            return 400;
        };
        let (Some(id), Some(events)) = (batch["group"]["id"].as_str(), batch["events"].as_array()) else {
            return 400;
        };
        let mut staged = self.staged.get(id).cloned().unwrap_or_default();
        for event in events {
            let Some(sequence) = event["sequence"].as_u64() else {
                return 400;
            };
            if event["id"] != json!(format!("{id}:{sequence}")) {
                return 409;
            }
            match staged.get(sequence as usize) {
                Some(prior) if prior != event => return 409,
                Some(_) => {}
                None if sequence as usize == staged.len() => staged.push(event.clone()),
                None => return 409,
            }
        }
        let complete = staged.last().is_some_and(|e| e["kind"] == "group_complete");
        if complete && self.active.as_ref().map(|(g, _)| &g["id"]) != Some(&json!(id)) {
            self.active = Some((batch["group"].clone(), staged.clone()));
            self.activations += 1;
        }
        self.staged.insert(id.into(), staged);
        200
    }

    fn upserts(&self) -> usize {
        self.active.as_ref().map_or(0, |(_, events)| {
            events.iter().filter(|e| e["kind"] == "upsert").count()
        })
    }
}

fn group(f: &Fixture) -> Result<()> {
    let state = Arc::new(Mutex::new(GroupReceiver::default()));
    let target = state.clone();
    let sink = Server::start(move |request| {
        let mut receiver = target.lock().unwrap();
        receiver.bodies.push(request.body.clone());
        match receiver.bodies.len() {
            // The sender stops before the receiver commits anything.
            1 => Response::json(503, "{}"),
            // The receiver commits the first batch and its acknowledgement is lost.
            2 => {
                receiver.receive(&request.body);
                Response::json(503, "{}")
            }
            _ => Response::json(receiver.receive(&request.body), "{}"),
        }
    });
    f.write("contextful.toml", &format!("authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"events\"\n[[model]]\nid = \"daily\"\nsql = \"SELECT day, note FROM events\"\nunique_key = [\"day\"]\n[model.contract]\nversion = \"1.0.0\"\ncolumns = [{{ name = \"day\", type = \"utf8\", nullable = false }}, {{ name = \"note\", type = \"utf8\", nullable = false }}]\n[[export]]\nname = \"daily-copy\"\ntable = \"daily\"\nendpoint = \"{}\"\nformat = \"changes-v1\"\nkey = [\"day\"]\nschedule = \"every 30s\"\n[[publication]]\nname = \"projection\"\njob = \"daily\"\nmembers = [\"daily-copy\"]\n", sink.url("/groups")))?;
    // Enough rows that one group spans several delivery requests.
    let note = "n".repeat(300);
    let rows: String = (0..300).map(|i| format!("{{\"day\":\"d{i:03}\",\"note\":\"{note}\"}}\n")).collect();
    f.write("rows.jsonl", &rows)?;
    let produce = |run: &str, now: &str| -> Result<()> {
        f.ok(&["context", "land", "events", "--project", PROJECT, "--rows", "rows.jsonl", "--run-id", run, "--site-id", "conformance", "--now", now], &[])?;
        let built = f.invoke(&["build", "daily", "--project", PROJECT, "--site-id", "conformance", "--now", now], &[])?;
        if !built.status.success() {
            return Err(if built.stderr.contains("PublicationStorageUnsupported") {
                "ConformanceCapabilityMissing:coordinated-group-outbox".into()
            } else {
                "ConformanceCommandFailed:build".into()
            });
        }
        // Generation one crosses empty-disk replacements, so its corpus must reach the bucket.
        if f.remote.is_some() && run == "load-1" {
            f.ok(&["sync", "push", "--project", PROJECT], &[])?;
        }
        Ok(())
    };
    produce("load-1", "2030-01-01T00:01:00Z")?;
    let public = f.ok(&["token", "keygen", "--out", ".contextful/issuer.seed"], &[])?;
    let token = f.ok(&["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "svc://conformance", "--zone", "on-prem:hq", "--table", "daily", "--ttl", "3600"], &[])?;
    let args = ["export", "group", "projection", "--project", PROJECT, "--public-key", &public, "--audience", AUDIENCE];
    let deliver = || f.invoke(&args, &[("CONTEXTFUL_TOKEN", &token)]);
    for _ in 0..2 {
        require(!deliver()?.status.success(), "ConformanceAcknowledgementBeforeCommit")?;
        require(state.lock().unwrap().active.is_none(), "ConformanceReceiverPartialGroup")?;
        f.replace_local_state()?;
    }
    f.ok(&args, &[("CONTEXTFUL_TOKEN", &token)])?;
    let first = {
        let receiver = state.lock().unwrap();
        require(receiver.bodies.len() > 3, "ConformanceGroupSingleRequest")?;
        require(receiver.bodies[0] == receiver.bodies[1] && receiver.bodies[1] == receiver.bodies[2], "ConformanceExportRetryChanged")?;
        require(receiver.activations == 1 && receiver.upserts() == 300, "ConformanceReceiverRowsMissing")?;
        receiver.active.as_ref().expect("activated")
            .0["id"].clone()
    };
    f.ok(&args, &[("CONTEXTFUL_TOKEN", &token)])?;
    let delivered = state.lock().unwrap().bodies.len();
    f.write("rows.jsonl", &format!("{{\"day\":\"d999\",\"note\":\"{note}\"}}\n"))?;
    produce("load-2", "2030-01-01T00:02:00Z")?;
    f.ok(&args, &[("CONTEXTFUL_TOKEN", &token)])?;
    let receiver = state.lock().unwrap();
    let (group, events) = receiver.active.as_ref().expect("activated");
    require(receiver.bodies.len() == delivered + 1, "ConformanceAcknowledgedExportRepeated")?;
    require(
        receiver.activations == 2 && group["base"] == first && group["order"] == 2,
        "ConformanceGroupPredecessorMissing",
    )?;
    require(
        events.iter().filter(|e| e["kind"] == "upsert").count() == 1,
        "ConformanceGroupChangesMissing",
    )
}

const WORKER_KEY: &str = "conformance-worker-key";

/// One request a delegate received: its path, whether its signature verified, and its envelope.
type Received = (String, bool, Value);

/// A loopback delegate behind two bindings: `/screen` holds each request open until the
/// scenario releases it, `/screen-next` answers at once. It logs each request on arrival,
/// before answering, and signs every answer over its request's idempotency key.
struct Delegate {
    server: Server,
    log: Arc<Mutex<Vec<Received>>>,
    gate: Arc<(Mutex<bool>, Condvar)>,
}

impl Delegate {
    fn start(hold: Duration) -> Self {
        let log: Arc<Mutex<Vec<Received>>> = Arc::default();
        let gate: Arc<(Mutex<bool>, Condvar)> = Arc::default();
        let (seen, held) = (log.clone(), gate.clone());
        let server = Server::start(move |request: &Request| {
            let signed = request_signed(WORKER_KEY.as_bytes(), request);
            let envelope = serde_json::from_slice::<Value>(&request.body).unwrap_or(Value::Null);
            seen.lock().unwrap().push((request.path().into(), signed, envelope.clone()));
            if !signed {
                return Response::json(401, "{}");
            }
            if request.path() == "/screen" {
                let (open, wake) = &*held;
                let released = open.lock().unwrap();
                drop(wake.wait_timeout_while(released, hold, |open| !*open).unwrap());
            }
            let row = &envelope["row"];
            let body = json!({
                "outcome": "ok",
                "rows": { "screen_result": [{ "entity_id": row["entity_id"], "pass_id": row["pass_id"], "job": envelope["job"] }] }
            });
            signed_answer(WORKER_KEY.as_bytes(), envelope["idempotency_key"].as_str().unwrap_or_default(), &body.to_string())
        });
        Self { server, log, gate }
    }

    fn release(&self) {
        let (open, wake) = &*self.gate;
        *open.lock().unwrap() = true;
        wake.notify_all();
    }

    fn requests(&self) -> Vec<Received> {
        self.log.lock().unwrap().clone()
    }
}

impl Drop for Delegate {
    fn drop(&mut self) {
        self.release();
    }
}

/// Two store-driven jobs isolate one entity namespace; the second binds a newer delegate
/// version. Each item is one call on its job's binding.
fn delegated(screen: &str, next: &str, timeout: u64) -> String {
    let job = |name: &str, delegate: &str| format!("[[job]]\nname = \"{name}\"\nkind = \"store-driven\"\nbody = {{ delegate = \"{delegate}\" }}\nstatement = \"SELECT entity_id, pass_id FROM due_items ORDER BY pass_id\"\nas_of = \"2030-01-01T00:00:10Z\"\nmax_in_flight = 1\ntables = [\"screen_result\"]\nisolated_items = {{ logical_key = \"pass_id\", entity_key = \"entity_id\", entity_namespace = \"entity\", max_attempts = 3 }}\n");
    format!("authoring_posture = \"per_request\"\n[control.delegates.screen]\nurl = \"{screen}\"\nmode = \"call\"\ntimeout = \"{timeout}s\"\nversion = \"screen-v1\"\n[control.delegates.screen-next]\nurl = \"{next}\"\nmode = \"call\"\ntimeout = \"{timeout}s\"\nversion = \"screen-v2\"\n{}{}", job("screen", "screen"), job("rescreen", "screen-next"))
}

/// The guards `contextful storage guard` lists under the `cloudflare-d1` profile.
fn guards(f: &Fixture) -> Result<Vec<Value>> {
    let state: Value = serde_json::from_str(&f.ok(&["storage", "guard", "--project", PROJECT], &[])?)
        .map_err(|_| "ConformanceGuardReportMalformed")?;
    state["guards"].as_array().cloned().ok_or_else(|| "ConformanceGuardReportMalformed".into())
}

/// Wait for `running` to exit before `deadline`.
fn exited(running: &mut Running, deadline: Instant) -> Result<std::process::ExitStatus> {
    loop {
        if let Some(status) = running.child.try_wait().map_err(|_| "ConformanceProcessUnavailable")? {
            return Ok(status);
        }
        require(Instant::now() < deadline, "ConformanceProcessTimeout:job")?;
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A newer job version fires while an earlier item's delegate call is in flight. On local
/// state it takes the entity lease and the superseded item lands nothing; a remote profile's
/// execution guard holds the takeover until the callback returns. Either way the delegate
/// receives one request per execution across every fire, and each item lands once.
fn fenced(f: &Fixture) -> Result<()> {
    let delegate = Delegate::start(f.timeout);
    f.write("contextful.toml", &delegated(&delegate.server.url("/screen"), &delegate.server.url("/screen-next"), f.timeout.as_secs()))?;
    f.write("due.jsonl", "{\"entity_id\":\"e-1\",\"pass_id\":\"p-1\"}\n")?;
    f.ok(&["context", "land", "due_items", "--project", PROJECT, "--rows", "due.jsonl", "--run-id", "due", "--site-id", "conformance", "--now", "2030-01-01T00:00:00Z"], &[])?;
    let public = f.ok(&["token", "keygen", "--out", ".contextful/issuer.seed"], &[])?;
    let token = f.ok(&["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "svc://conformance", "--zone", "on-prem:hq", "--table", "due_items", "--ttl", "3600"], &[])?;
    let env = [("CONTEXTFUL_TOKEN", token.as_str()), ("CONTEXTFUL_WORKER_KEY", WORKER_KEY)];
    let args = |job: &'static str, run: &'static str, now: &'static str| {
        ["job", "fire", job, "--project", PROJECT, "--run-id", run, "--site-id", "conformance", "--now", now, "--public-key", public.as_str(), "--audience", AUDIENCE]
    };

    // The first fire admits the item under the entity's lease and holds its call open.
    let mut first = Running::spawn(f.command(&args("screen", "fence-1", "2030-01-01T00:01:00Z"), &env))?;
    let deadline = Instant::now() + f.timeout;
    while delegate.requests().is_empty() {
        require(first.child.try_wait().map_err(|_| "ConformanceProcessUnavailable")?.is_none(), "ConformanceDelegateNotCalled")?;
        require(Instant::now() < deadline, "ConformanceDelegateNotCalled")?;
        std::thread::sleep(Duration::from_millis(20));
    }
    let held = delegate.requests()[0].2["idempotency_key"].clone();

    // A newer job version fires past the lease's time-to-live.
    let mut second = Running::spawn(f.command(&args("rescreen", "fence-2", "2030-01-01T00:02:00Z"), &env))?;
    let guarded = f.remote.is_some();
    let refused = if guarded {
        // The in-flight call's guard holds the entity lease: PostgreSQL waits on its row
        // lock, D1 refuses the transfer and lists the guard under the held request's key.
        let settle = Instant::now() + Duration::from_secs(3);
        while Instant::now() < settle && second.child.try_wait().map_err(|_| "ConformanceProcessUnavailable")?.is_none() {
            std::thread::sleep(Duration::from_millis(20));
        }
        let early = second.child.try_wait().map_err(|_| "ConformanceProcessUnavailable")?;
        require(!early.is_some_and(|status| status.success()), "ConformanceGuardTransferAdmitted")?;
        if on_d1(f) {
            require(guards(f)?.iter().any(|guard| guard["idempotency_key"] == held), "ConformanceGuardMissing")?;
        }
        early.is_some()
    } else {
        // Without a guard the newer owner takes the lease and lands while the call is held.
        let status = exited(&mut second, Instant::now() + f.timeout)?;
        if !status.success() {
            return Err(failed("ConformanceNewerOwnerRefused", &["job"], &second.stderr()?));
        }
        require(
            f.query("SELECT job FROM screen_result ORDER BY job")?["rows"] == json!([["rescreen"]]),
            "ConformanceNewerOwnerLost",
        )?;
        false
    };

    // The held call answers.
    delegate.release();
    let landed = exited(&mut first, Instant::now() + f.timeout)?.success();
    require(guarded || !landed, "ConformanceSupersededItemLanded")?;
    if guarded {
        if on_d1(f) {
            // The returned callback deleted its own guard, so the newer owner proceeds.
            require(guards(f)?.is_empty(), "ConformanceGuardRetained")?;
        }
        if refused {
            f.ok(&args("rescreen", "fence-2-retry", "2030-01-01T00:03:00Z"), &env)?;
        } else if !exited(&mut second, Instant::now() + f.timeout)?.success() {
            return Err(failed("ConformanceNewerOwnerRefused", &["job"], &second.stderr()?));
        }
    }

    // A later fire of the first job sends nothing: a superseded item replays its recorded
    // answer under a fresh lease, and a landed one holds its receipt.
    f.ok(&args("screen", "fence-3", "2030-01-01T00:04:00Z"), &env)?;
    let requests = delegate.requests();
    require(requests.len() == 2, "ConformanceDelegateRequestRepeated")?;
    require(requests.iter().all(|(_, signed, _)| *signed), "ConformanceDelegateRequestUnsigned")?;
    let (screen, next) = (&requests[0].2, &requests[1].2);
    require(requests[0].0 == "/screen" && requests[1].0 == "/screen-next", "ConformanceDelegateBindingMismatch")?;
    require(screen["execution_id"] != next["execution_id"] && screen["idempotency_key"] != next["idempotency_key"], "ConformanceExecutionIdentityShared")?;
    require(
        screen["entity"]["fence"].as_u64().zip(next["entity"]["fence"].as_u64()).is_some_and(|(old, new)| new > old),
        "ConformanceEntityFenceNotAdvanced",
    )?;
    require(
        f.query("SELECT job FROM screen_result ORDER BY job")?["rows"] == json!([["rescreen"], ["screen"]]),
        "ConformanceRecordedAnswerLost",
    )?;
    let items = f.query("SELECT job, outcome, run_id FROM _items ORDER BY job")?;
    let items = rows(&items)?;
    require(items.len() == 2 && items.iter().all(|item| item[1] == "succeeded"), "ConformanceItemOutcomeMissing")?;
    // The first item lands under its own attempt only when that attempt succeeded.
    require(
        items[1][0] == "screen" && items[1][2].as_str().is_some_and(|run| run.starts_with(if landed { "fence-1:" } else { "fence-3:" })),
        "ConformanceSupersededItemLanded",
    )?;
    if on_d1(f) {
        require(guards(f)?.is_empty(), "ConformanceGuardRetained")?;
    }
    Ok(())
}
