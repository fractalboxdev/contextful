//! The `s3` object source through the built binary, reading loopback buckets through the S3
//! bucket adapter.
#![cfg(feature = "s3-sync")]

use crate::pipeline::{cf, ok, project, stderr};
use contextful_acceptance::s3::{S3Server, ACCESS_KEY, SECRET_KEY};
use contextful_core::connector::reference::Hydrated;
use contextful_core::store::object::{Condition, ObjectStore};
use contextful_sync::{S3Bucket, S3Credentials};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

/// One fire of pipeline `feed` with `env` in the process environment.
fn fire(dir: &Path, run: &str, now: &str, env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["pipeline", "run", "feed", "--project", "research", "--run-id", run, "--site-id", "site-a", "--now", now])
        .current_dir(dir)
        .env_remove("CONTEXTFUL_NODE_ID")
        .env_remove("CONTEXTFUL_SECRETS_BACKEND")
        .env("CONTEXTFUL_SECRETS_ALLOW_ENV_TEMPLATES", "1")
        .envs(env.iter().copied())
        .output()
        .unwrap()
}

fn object_pipeline(config: &str) -> tempfile::TempDir {
    project(&format!("[[pipeline]]\nid = \"feed\"\ntables = [\"orders\"]\n[pipeline.source]\nname = \"s3\"\nconfig = {{ bucket = \"lake\", format = \"jsonl\"{config} }}\n"))
}

/// `pipeline validate` builds an `s3` source: a key beside a prefix and a literal credential
/// each refuse before any request, and a well-formed source validates.
#[test]
fn validate_builds_an_object_source_and_refuses_its_malformed_declarations() {
    ok(&cf(
        object_pipeline(", prefix = \"orders/\", suffix = \".jsonl.gz\", skip_unchanged = true, access_key_id = \"secret://lake-key-id\", secret_access_key = \"secret://lake-secret\"").path(),
        &["pipeline", "validate"],
    ));
    for (config, error) in [
        (", key = \"orders.jsonl\", prefix = \"orders/\"", "ConnectorObjectAddressRejected"),
        (", key = \"orders.jsonl\", access_key_id = \"AKIAIOSFODNN7EXAMPLE\", secret_access_key = \"secret://lake-secret\"", "SecretMaterialInDeclaration"),
        (", key = \"orders.jsonl\", endpoint = \"http://objects.vendor.example\", access_key_id = \"secret://lake-key-id\", secret_access_key = \"secret://lake-secret\"", "SecretCleartextEndpoint"),
    ] {
        let out = cf(object_pipeline(config).path(), &["pipeline", "validate"]);
        assert!(!out.status.success(), "{config}: {}", String::from_utf8_lossy(&out.stdout));
        assert!(stderr(&out).contains(error), "{config}: {}", stderr(&out));
    }
    // An unsigned read carries no capability to leak, so a cleartext endpoint validates.
    ok(&cf(object_pipeline(", key = \"orders.jsonl\", endpoint = \"http://objects.vendor.example\"").path(), &["pipeline", "validate"]));
}

/// `incremental` beside `skip_unchanged` on an `s3` source refuses at validation: the
/// position records each object's ETag.
// spec: connector.package.component-position@a87bf129
#[test]
fn an_incremental_field_beside_an_etag_position_is_refused_at_validation() {
    let manifest = |skip: bool| {
        format!("[[pipeline]]\nid = \"feed\"\nincremental = \"at\"\ntables = [\"orders\"]\n[pipeline.source]\nname = \"s3\"\nconfig = {{ bucket = \"lake\", key = \"orders.jsonl\", skip_unchanged = {skip} }}\n")
    };
    ok(&cf(project(&manifest(false)).path(), &["pipeline", "validate"]));
    let out = cf(project(&manifest(true)).path(), &["pipeline", "validate"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("ConnectorPositionOwned") && stderr(&out).contains("ETag"), "{}", stderr(&out));
}

/// A loopback S3 endpoint over a mutable key map: object reads answer with an `ETag` of
/// the body's length and first byte, and every request target is recorded.
struct Bucket {
    port: u16,
    objects: Arc<Mutex<Vec<(String, String)>>>,
    targets: Arc<Mutex<Vec<(String, String)>>>,
}

impl Bucket {
    fn start(objects: &[(&str, &str)]) -> Bucket {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let objects: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(objects.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()));
        let targets: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let (held, seen) = (objects.clone(), targets.clone());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap() == 0 || h.trim().is_empty() {
                        break;
                    }
                }
                let mut parts = line.split_whitespace();
                let (method, target) = (parts.next().unwrap_or_default().to_string(), parts.next().unwrap_or_default().to_string());
                seen.lock().unwrap().push((method.clone(), target.clone()));
                let path = target.split('?').next().unwrap_or_default();
                let etag = |v: &str| format!("\"{}-{}\"", v.len(), v.bytes().next().unwrap_or(0));
                let (status, headers, body) = match path.strip_prefix("/lake/") {
                    Some(key) => match held.lock().unwrap().iter().find(|(k, _)| k == key) {
                        Some((_, v)) => (200, format!("ETag: {}\r\n", etag(v)), if method == "HEAD" { String::new() } else { v.clone() }),
                        None => (404, String::new(), String::new()),
                    },
                    None => (404, String::new(), String::new()),
                };
                let _ = write!(stream, "HTTP/1.1 {status} X\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n{body}", body.len());
            }
        });
        Bucket { port, objects, targets }
    }

    fn put(&self, key: &str, body: &str) {
        let mut objects = self.objects.lock().unwrap();
        objects.retain(|(k, _)| k != key);
        objects.push((key.to_string(), body.to_string()));
    }

    /// Each request as `<method> <path>`, its query set aside.
    fn lines(&self) -> Vec<String> {
        self.targets.lock().unwrap().iter().map(|(m, t)| format!("{m} {}", t.split('?').next().unwrap_or_default())).collect()
    }
}

fn fixed_key(port: u16, keys: &str) -> tempfile::TempDir {
    project(&format!(
        "[[pipeline]]\nid = \"feed\"\ntables = [\"orders\"]\n[pipeline.source]\nname = \"s3\"\nconfig = {{ bucket = \"lake\", endpoint = \"http://127.0.0.1:{port}\", key = \"feed/orders.jsonl\", skip_unchanged = true{keys} }}\n"
    ))
}

/// A fixed-key JSONL object lands its rows; a second fire over the same ETag lands zero rows,
/// closes a success and keeps the position, and a rewritten object lands again.
#[test]
fn a_fixed_key_object_lands_once_per_etag() {
    let bucket = Bucket::start(&[("feed/orders.jsonl", "{\"id\":\"o1\"}\n{\"id\":\"o2\"}\n")]);
    let dir = fixed_key(bucket.port, ", access_key_id = \"secret://lake-key-id\", secret_access_key = \"secret://lake-secret\"");
    let env = [("LAKE_KEY_ID", "AKIDLOOPBACKEXAMPLE"), ("LAKE_SECRET", "loopback-secret")];
    let fire = |run: &str, now: &str| fire(dir.path(), run, now, &env);
    assert!(ok(&fire("f1", "2030-01-01T00:00:00Z")).contains("2 rows"));
    assert!(ok(&fire("f2", "2030-01-01T00:01:00Z")).contains("f2 success · 0 rows"));
    bucket.put("feed/orders.jsonl", "{\"id\":\"o1\"}\n{\"id\":\"o2\"}\n{\"id\":\"o3\"}\n");
    assert!(ok(&fire("f3", "2030-01-01T00:02:00Z")).contains("3 rows"));
    assert_eq!(bucket.lines(), ["GET /lake/feed/orders.jsonl", "HEAD /lake/feed/orders.jsonl", "HEAD /lake/feed/orders.jsonl", "GET /lake/feed/orders.jsonl"]);
    for (_, t) in bucket.targets.lock().unwrap().iter() {
        assert!(t.contains("X-Amz-Signature=") && t.contains("AKIDLOOPBACKEXAMPLE") && !t.contains("loopback-secret"), "{t}");
    }
}

/// A `session_token` beside the key pair, hydrated per read, signs each request as a
/// temporary credential: every presigned URL carries the token.
// spec: connector.source.object-credentials@1c02d9dc
#[test]
fn a_session_token_signs_every_request_as_a_temporary_credential() {
    let bucket = Bucket::start(&[("feed/orders.jsonl", "{\"id\":\"o1\"}\n")]);
    let dir = fixed_key(bucket.port, ", access_key_id = \"secret://lake-key-id\", secret_access_key = \"secret://lake-secret\", session_token = \"secret://lake-token\"");
    let env = [("LAKE_KEY_ID", "ASIALOOPBACKEXAMPLE"), ("LAKE_SECRET", "loopback-secret"), ("LAKE_TOKEN", "loopback-session-token")];
    assert!(ok(&fire(dir.path(), "f1", "2030-01-01T00:00:00Z", &env)).contains("1 rows"));
    assert!(ok(&fire(dir.path(), "f2", "2030-01-01T00:01:00Z", &env)).contains("f2 success · 0 rows"));
    let targets = bucket.targets.lock().unwrap().clone();
    assert_eq!(targets.len(), 2);
    for (_, t) in &targets {
        assert!(t.contains("X-Amz-Security-Token=loopback-session-token") && t.contains("ASIALOOPBACKEXAMPLE") && !t.contains("loopback-secret"), "{t}");
    }
}

/// A source binding no key pair sends every request unsigned.
#[test]
fn an_unbound_object_source_sends_unsigned() {
    let bucket = Bucket::start(&[("feed/orders.jsonl", "{\"id\":\"o1\"}\n")]);
    let dir = fixed_key(bucket.port, "");
    assert!(ok(&fire(dir.path(), "f1", "2030-01-01T00:00:00Z", &[])).contains("1 rows"));
    let targets = bucket.targets.lock().unwrap().clone();
    assert_eq!(targets.len(), 1);
    assert!(!targets[0].1.contains("X-Amz-"), "{}", targets[0].1);
}

/// A manifest reading bucket `lake` on `server` by `address`, bound to the server's key pair.
fn verified(server: &S3Server, address: &str) -> tempfile::TempDir {
    project(&format!(
        "[[pipeline]]\nid = \"feed\"\ntables = [\"orders\"]\n[pipeline.source]\nname = \"s3\"\nconfig = {{ bucket = \"lake\", endpoint = \"{}\", {address}, format = \"jsonl\", skip_unchanged = true, access_key_id = \"secret://lake-key-id\", secret_access_key = \"secret://lake-secret\" }}\n",
        server.endpoint
    ))
}

/// The `s3` source presigns each request through the S3 bucket adapter and sends it through the
/// mediated client; a skip compares the listing's ETags or one head request's and downloads no
/// object.
// spec: connector.source.object-transport@36989e47
#[test]
fn a_signed_read_passes_the_backends_signature_check_and_a_skip_downloads_nothing() {
    let server = S3Server::start("lake");
    let keys = S3Credentials { access_key_id: Hydrated::new(ACCESS_KEY), secret_access_key: Hydrated::new(SECRET_KEY), session_token: None };
    let writer = S3Bucket::open(&server.endpoint, "us-east-1", "lake", keys).unwrap();
    for (k, v) in [("feed/01.jsonl", "{\"n\":1}\n"), ("feed/02.jsonl", "{\"n\":2}\n{\"n\":3}\n")] {
        writer.put(k, v.as_bytes(), Condition::None).unwrap();
    }
    let env = [("LAKE_KEY_ID", ACCESS_KEY), ("LAKE_SECRET", SECRET_KEY)];

    let prefix = verified(&server, "prefix = \"feed/\"");
    assert!(ok(&fire(prefix.path(), "f1", "2030-01-01T00:00:00Z", &env)).contains("3 rows"));
    assert_eq!(server.gets(), 2, "one download per listed object");
    assert!(ok(&fire(prefix.path(), "f2", "2030-01-01T00:01:00Z", &env)).contains("f2 success · 0 rows"));
    assert_eq!(server.gets(), 2, "the listing's ETags answer the skip");

    let key = verified(&server, "key = \"feed/02.jsonl\"");
    assert!(ok(&fire(key.path(), "f1", "2030-01-01T00:00:00Z", &env)).contains("2 rows"));
    assert!(ok(&fire(key.path(), "f2", "2030-01-01T00:01:00Z", &env)).contains("f2 success · 0 rows"));
    assert_eq!((server.gets(), server.heads()), (3, 1), "a fixed key's skip costs one head request");

    // A signature the backend cannot verify lands nothing.
    let forged = verified(&server, "key = \"feed/01.jsonl\"");
    let out = fire(forged.path(), "f1", "2030-01-01T00:00:00Z", &[("LAKE_KEY_ID", ACCESS_KEY), ("LAKE_SECRET", "not-the-secret-key-0000000000000000000000")]);
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), stderr(&out));
    assert!(said.contains("answered 403") && !said.contains("1 rows"), "{said}");
    assert_eq!(server.gets(), 3);
}

/// `credentials = "role"` on the `s3` source signs each request with the workload role's
/// credential in place of the key pair, which the backend verifies; beside a key it refuses at
/// validation as a credential conflict.
// spec: connector.source.object-role@21954bf3
#[test]
fn a_role_signed_object_source_reads_and_refuses_beside_a_key() {
    use crate::sync::s3::{container_endpoint, NO_ROLE};
    let server = S3Server::start("lake");
    let keys = S3Credentials { access_key_id: Hydrated::new(ACCESS_KEY), secret_access_key: Hydrated::new(SECRET_KEY), session_token: None };
    S3Bucket::open(&server.endpoint, "us-east-1", "lake", keys).unwrap().put("feed/01.jsonl", b"{\"n\":1}\n{\"n\":2}\n", Condition::None).unwrap();
    let (url, served) = container_endpoint();
    let mut env = NO_ROLE.to_vec();
    env[1].1 = &url;
    let manifest = |extra: &str| {
        project(&format!(
            "[[pipeline]]\nid = \"feed\"\ntables = [\"orders\"]\n[pipeline.source]\nname = \"s3\"\nconfig = {{ bucket = \"lake\", endpoint = \"{}\", prefix = \"feed/\", format = \"jsonl\", credentials = \"role\"{extra} }}\n",
            server.endpoint
        ))
    };
    let role = manifest("");
    assert!(ok(&fire(role.path(), "f1", "2030-01-01T00:00:00Z", &env)).contains("2 rows"));
    assert_eq!(server.gets(), 1);
    assert_eq!(served.load(std::sync::atomic::Ordering::SeqCst), 1);

    // No role source answering lands nothing and names the error.
    let out = fire(role.path(), "f2", "2030-01-01T00:01:00Z", &NO_ROLE);
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), stderr(&out));
    assert!(said.contains("SyncRoleUnavailable") && !said.contains("2 rows"), "{said}");
    assert_eq!(server.gets(), 1);

    let out = cf(manifest(", access_key_id = \"secret://lake-key-id\", secret_access_key = \"secret://lake-secret\"").path(), &["pipeline", "validate"]);
    assert!(!out.status.success() && stderr(&out).contains("SyncCredentialConflict"), "{}", stderr(&out));
}

/// An object-arrival key only wakes its prefix read, which lists from its held position as a scheduled fire does; a
/// duplicate or reordered key lands nothing twice, and the source's cadence still fires to catch a missed one.
// spec: run.land.object-arrival-wake@3d0668f8
#[test]
fn an_arrival_key_wakes_the_prefix_read_covering_it_and_lands_nothing_twice() {
    use contextful_core::run::queue::{Queue, QueueMessage};
    use contextful_engine::queue::LocalSpool;

    let server = S3Server::start("lake");
    let keys = S3Credentials { access_key_id: Hydrated::new(ACCESS_KEY), secret_access_key: Hydrated::new(SECRET_KEY), session_token: None };
    let writer = S3Bucket::open(&server.endpoint, "us-east-1", "lake", keys).unwrap();
    writer.put("feed/01.jsonl", b"{\"n\":1}\n", Condition::None).unwrap();
    let env = [("LAKE_KEY_ID", ACCESS_KEY), ("LAKE_SECRET", SECRET_KEY), ("CONTEXTFUL_SECRETS_ALLOW_ENV_TEMPLATES", "1")];
    let dir = project(&format!(
        "site_id = \"site-a\"\n[[pipeline]]\nid = \"feed\"\ntables = [\"orders\"]\n[pipeline.source]\nname = \"s3\"\nconfig = {{ bucket = \"lake\", endpoint = \"{}\", prefix = \"feed/\", suffix = \".jsonl\", format = \"jsonl\", skip_unchanged = true, access_key_id = \"secret://lake-key-id\", secret_access_key = \"secret://lake-secret\" }}\n",
        server.endpoint
    ));
    let p = dir.path();
    let spool = LocalSpool::open(&p.join(".contextful/queue/research")).unwrap();
    let arrive = |bucket: &str, key: &str| spool.send(&QueueMessage::ObjectArrival { bucket: bucket.into(), key: key.into() }).unwrap();
    let drain = |now: &str| {
        let out = Command::new(env!("CARGO_BIN_EXE_contextful"))
            .args(["queue", "drain", "--project", "research", "--now", now])
            .current_dir(p)
            .env_remove("CONTEXTFUL_NODE_ID")
            .env_remove("CONTEXTFUL_SECRETS_BACKEND")
            .envs(env.iter().copied())
            .output()
            .unwrap();
        ok(&out)
    };
    assert!(ok(&fire(p, "f1", "2030-01-01T00:00:00Z", &env)).contains("1 rows"));

    // A key outside every read's address, or in another bucket, wakes nothing and is acknowledged.
    writer.put("other/09.jsonl", b"{\"n\":9}\n", Condition::None).unwrap();
    arrive("lake", "other/09.jsonl");
    arrive("lake", "feed/readme.txt");
    arrive("archive", "feed/02.jsonl");
    let none = drain("2030-01-01T00:01:00Z");
    assert!(none.contains("and 3 arrivals") && !none.contains("rows"), "{none}");
    assert_eq!(server.gets(), 1);

    // A new key, repeated and arriving beside an older one, wakes one read from the held position.
    writer.put("feed/02.jsonl", b"{\"n\":2}\n{\"n\":3}\n", Condition::None).unwrap();
    arrive("lake", "feed/02.jsonl");
    arrive("lake", "feed/01.jsonl");
    arrive("lake", "feed/02.jsonl");
    let woke = drain("2030-01-01T00:02:00Z");
    assert!(woke.contains("· 2 rows") && woke.contains("2 arrivals"), "{woke}");
    assert_eq!(server.gets(), 2, "the held ETags skip the landed object");

    // A duplicate after the landing lands nothing.
    arrive("lake", "feed/02.jsonl");
    let again = drain("2030-01-01T00:03:00Z");
    assert!(again.contains("· 0 rows"), "{again}");
    assert_eq!(server.gets(), 2);

    // An object whose notification never arrives lands on the source's next scheduled fire.
    writer.put("feed/03.jsonl", b"{\"n\":4}\n", Condition::None).unwrap();
    assert!(ok(&fire(p, "f9", "2030-01-01T00:09:00Z", &env)).contains("f9 success · 1 rows"));
}

/// The env a cycle's fires read the loopback bucket's key pair from.
const LAKE_ENV: [(&str, &str); 3] = [("LAKE_KEY_ID", ACCESS_KEY), ("LAKE_SECRET", SECRET_KEY), ("CONTEXTFUL_SECRETS_ALLOW_ENV_TEMPLATES", "1")];

/// `pipeline serve --cycle` at `now`: its exit status and printed answer.
fn cycle(dir: &Path, now: &str) -> (bool, serde_json::Value, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["pipeline", "serve", "--cycle", "--project", "research", "--now", now])
        .current_dir(dir)
        .env_remove("CONTEXTFUL_NODE_ID")
        .env_remove("CONTEXTFUL_SECRETS_BACKEND")
        .envs(LAKE_ENV.iter().copied())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let answer = serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("{e}: {text}\n{}", stderr(&out)));
    (out.status.success(), answer, stderr(&out))
}

fn rows(dir: &Path) -> String {
    ok(&cf(dir, &["query", "--json", "--project", "research", "SELECT count(*) FROM feed_orders"]))
}

/// A cycle drains the queue under the cadence lease once its units end: an arrival key wakes its prefix read before the
/// source's cadence comes due, and an object whose key never arrives lands on that cadence.
// spec: surface.fire.cycle-drain@9362d1e0
#[test]
fn a_cycle_drains_arrival_keys_between_cadence_fires() {
    use contextful_core::run::queue::{Queue, QueueMessage};
    use contextful_engine::queue::LocalSpool;

    let server = S3Server::start("lake");
    let keys = S3Credentials { access_key_id: Hydrated::new(ACCESS_KEY), secret_access_key: Hydrated::new(SECRET_KEY), session_token: None };
    let writer = S3Bucket::open(&server.endpoint, "us-east-1", "lake", keys).unwrap();
    writer.put("feed/01.jsonl", b"{\"n\":1}\n", Condition::None).unwrap();
    let dir = project(&format!(
        "site_id = \"site-a\"\n[[pipeline]]\nid = \"feed\"\ntables = [\"orders\"]\nschedule = \"every 1h\"\n[pipeline.source]\nname = \"s3\"\nconfig = {{ bucket = \"lake\", endpoint = \"{}\", prefix = \"feed/\", suffix = \".jsonl\", format = \"jsonl\", skip_unchanged = true, access_key_id = \"secret://lake-key-id\", secret_access_key = \"secret://lake-secret\" }}\n",
        server.endpoint
    ));
    let p = dir.path();
    ok(&cf(p, &["pipeline", "import", "--project", "research"]));
    let (passed, first, said) = cycle(p, "2030-01-01T00:00:00Z");
    assert!(passed, "{first}\n{said}");
    assert_eq!(first["fired"], serde_json::json!(["feed"]), "{first}");
    assert_eq!(first["drained"]["arrivals"], 0, "{first}");

    writer.put("feed/02.jsonl", b"{\"n\":2}\n{\"n\":3}\n", Condition::None).unwrap();
    LocalSpool::open(&p.join(".contextful/queue/research")).unwrap().send(&QueueMessage::ObjectArrival { bucket: "lake".into(), key: "feed/02.jsonl".into() }).unwrap();
    let (passed, woke, said) = cycle(p, "2030-01-01T00:05:00Z");
    assert!(passed, "{woke}\n{said}");
    assert_eq!((woke["fired"].clone(), woke["drained"]["arrivals"].clone(), woke["drained"]["left"].clone()), (serde_json::json!([]), serde_json::json!(1), serde_json::json!(0)), "nothing is due; the arrival wakes the read: {woke}");
    assert!(rows(p).contains("\"3\""), "{}", rows(p));
    let spool = LocalSpool::open(&p.join(".contextful/queue/research")).unwrap();
    let waiting: Vec<QueueMessage> = spool.receive(10).unwrap().into_iter().map(|d| d.message).collect();
    assert_eq!(woke["drained"]["wake_ups"], 0, "{woke}");
    assert!(matches!(waiting.as_slice(), [QueueMessage::Committed { table, .. }] if table == "feed_orders"), "the wake-up waits for `queue drain`: {waiting:?}");

    // An object whose key never arrives waits for the cadence, which counts from the woken fire.
    writer.put("feed/03.jsonl", b"{\"n\":4}\n", Condition::None).unwrap();
    let (_, quiet, _) = cycle(p, "2030-01-01T00:10:00Z");
    assert_eq!((quiet["drained"]["arrivals"].clone(), quiet["next_due"].clone()), (serde_json::json!(0), serde_json::json!("2030-01-01T01:05:00Z")), "{quiet}");
    assert!(rows(p).contains("\"3\""), "{}", rows(p));
    let (passed, due, said) = cycle(p, "2030-01-01T01:05:00Z");
    assert!(passed, "{due}\n{said}");
    assert_eq!(due["fired"], serde_json::json!(["feed"]), "{due}");
    assert!(rows(p).contains("\"4\""), "{}", rows(p));
}

/// A cycle whose drain fails prints its answer, exits non-zero and leaves the delivery queued.
// spec: surface.fire.cycle-drain-exit@c1cbfb90
#[test]
fn a_cycle_whose_drain_fails_exits_non_zero_and_keeps_the_delivery() {
    use contextful_core::run::queue::{Queue, QueueMessage};
    use contextful_engine::queue::LocalSpool;

    // `dead` reads an endpoint nothing listens on and declares no schedule, so only its arrival fires it.
    let dir = project(
        "site_id = \"site-a\"\n[[pipeline]]\nid = \"dead\"\ntables = [\"orders\"]\n[pipeline.source]\nname = \"s3\"\nconfig = { bucket = \"lake\", endpoint = \"http://127.0.0.1:9\", prefix = \"feed/\", format = \"jsonl\", access_key_id = \"secret://lake-key-id\", secret_access_key = \"secret://lake-secret\" }\n",
    );
    let p = dir.path();
    ok(&cf(p, &["pipeline", "import", "--project", "research"]));
    let spool = LocalSpool::open(&p.join(".contextful/queue/research")).unwrap();
    spool.send(&QueueMessage::ObjectArrival { bucket: "lake".into(), key: "feed/01.jsonl".into() }).unwrap();
    let (passed, answer, said) = cycle(p, "2030-01-01T00:00:00Z");
    assert!(!passed, "a failed drain exits non-zero: {answer}");
    assert_eq!(answer["failed"], serde_json::json!([]), "{answer}");
    assert!(answer["drained"]["failed"].as_str().is_some_and(|f| f.contains("stay queued")), "{answer}");
    assert!(said.contains("queue drain failed") && said.contains("`dead`"), "{said}");
    assert_eq!(spool.receive(10).unwrap().len(), 1, "the arrival stays queued for the next cycle");
}

/// The queue messages pending in the project's local spool.
fn spooled(dir: &Path) -> Vec<contextful_core::run::queue::QueueMessage> {
    use contextful_core::run::queue::Queue;
    let spool = contextful_engine::queue::LocalSpool::open(&dir.join(".contextful/queue/research")).unwrap();
    spool.receive(10).unwrap().into_iter().map(|d| d.message).collect()
}

/// The daemon drains the queue under the cadence lease between cadence fires: an arrival key wakes its prefix read
/// while the source's hourly cadence is far from due, and the woken read's post-commit wake-up stays spooled.
// spec: surface.fire.daemon-drain@a2bc94c8
#[test]
fn the_daemon_drains_arrival_keys_between_cadence_fires() {
    use contextful_core::run::queue::{Queue, QueueMessage};
    use contextful_engine::queue::LocalSpool;

    let server = S3Server::start("lake");
    let keys = S3Credentials { access_key_id: Hydrated::new(ACCESS_KEY), secret_access_key: Hydrated::new(SECRET_KEY), session_token: None };
    let writer = S3Bucket::open(&server.endpoint, "us-east-1", "lake", keys).unwrap();
    writer.put("feed/01.jsonl", b"{\"n\":1}\n", Condition::None).unwrap();
    let dir = project(&format!(
        "site_id = \"site-a\"\n[[pipeline]]\nid = \"feed\"\ntables = [\"orders\"]\nschedule = \"every 1h\"\n[pipeline.source]\nname = \"s3\"\nconfig = {{ bucket = \"lake\", endpoint = \"{}\", prefix = \"feed/\", suffix = \".jsonl\", format = \"jsonl\", skip_unchanged = true, access_key_id = \"secret://lake-key-id\", secret_access_key = \"secret://lake-secret\" }}\n",
        server.endpoint
    ));
    let p = dir.path();
    ok(&cf(p, &["pipeline", "import", "--project", "research"]));
    let mut daemon = crate::pipeline::Daemon::start_env(p, &LAKE_ENV);
    let fired = daemon.wait_for("fire feed: done", 0);

    writer.put("feed/02.jsonl", b"{\"n\":2}\n{\"n\":3}\n", Condition::None).unwrap();
    LocalSpool::open(&p.join(".contextful/queue/research")).unwrap().send(&QueueMessage::ObjectArrival { bucket: "lake".into(), key: "feed/02.jsonl".into() }).unwrap();
    daemon.wait_for("\"arrivals\":1", fired);
    assert!(daemon.running(), "{}", daemon.lines().join("\n"));
    assert!(daemon.terminate().success(), "{}", daemon.lines().join("\n"));
    assert_eq!(daemon.lines().iter().filter(|l| l.contains("fire feed: started")).count(), 1, "the hourly cadence fired once:\n{}", daemon.lines().join("\n"));
    assert!(rows(p).contains("\"3\""), "{}", rows(p));
    let waiting = spooled(p);
    assert!(!waiting.is_empty() && waiting.iter().all(|m| matches!(m, QueueMessage::Committed { .. })), "only wake-ups wait for `queue drain`: {waiting:?}");
}

/// A daemon drain that fails logs its error and leaves the delivery queued; the daemon keeps serving and drains again
/// on its next pass.
// spec: surface.fire.daemon-drain-retry@d169b8df
#[test]
fn a_failed_daemon_drain_logs_keeps_serving_and_retries() {
    use contextful_core::run::queue::{Queue, QueueMessage};
    use contextful_engine::queue::LocalSpool;

    // `dead` reads an endpoint nothing listens on and declares no schedule, so only its arrival fires it.
    let dir = project(
        "site_id = \"site-a\"\n[[pipeline]]\nid = \"dead\"\ntables = [\"orders\"]\n[pipeline.source]\nname = \"s3\"\nconfig = { bucket = \"lake\", endpoint = \"http://127.0.0.1:9\", prefix = \"feed/\", format = \"jsonl\", access_key_id = \"secret://lake-key-id\", secret_access_key = \"secret://lake-secret\" }\n",
    );
    let p = dir.path();
    ok(&cf(p, &["pipeline", "import", "--project", "research"]));
    LocalSpool::open(&p.join(".contextful/queue/research")).unwrap().send(&QueueMessage::ObjectArrival { bucket: "lake".into(), key: "feed/01.jsonl".into() }).unwrap();
    let mut daemon = crate::pipeline::Daemon::start_env(p, &LAKE_ENV);
    let first = daemon.wait_for("queue drain failed", 0);
    daemon.wait_for("queue drain failed", first + 1);
    assert!(daemon.running(), "a failed drain stops nothing:\n{}", daemon.lines().join("\n"));
    assert!(daemon.terminate().success(), "{}", daemon.lines().join("\n"));
    assert!(matches!(spooled(p).as_slice(), [QueueMessage::ObjectArrival { key, .. }] if key == "feed/01.jsonl"), "the arrival stays queued");
}
