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
// spec: connector.source.object-position-owned@4ed72fb9
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
