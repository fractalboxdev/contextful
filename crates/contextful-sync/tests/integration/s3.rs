//! `store.endpoint`: the S3 adapter against a loopback S3 server that verifies every
//! request's signature and arbitrates each conditional put under one lock.
#![cfg(feature = "s3-sync")]

use crate::support::{at, node};
use contextful_acceptance::s3::{S3Server, ACCESS_KEY, SECRET_KEY};
use contextful_core::connector::reference::Hydrated;
use contextful_core::store::object::{Condition, ObjectError, ObjectStore, Put};
use contextful_core::store::sync::Coordination;
use contextful_core::store::StoreError;
use contextful_sync::{PullScope, S3Bucket, S3Credentials, SyncError};
use serde_json::json;
use std::sync::Arc;

const BUCKET: &str = "context-team";

fn credentials(secret: &str) -> S3Credentials {
    S3Credentials { access_key_id: Hydrated::new(ACCESS_KEY), secret_access_key: Hydrated::new(secret), session_token: None }
}

fn open(server: &S3Server) -> S3Bucket {
    S3Bucket::open(&server.endpoint, "us-east-1", BUCKET, credentials(SECRET_KEY)).unwrap()
}

/// An S3 or R2 bucket addresses each object path-style, `<endpoint>/<bucket>/<key>` with every key segment
/// percent-encoded, and signs every request with AWS Signature Version 4 for service `s3` at the endpoint's region.
// spec: store.endpoint.addressing@ec631d9a
#[test]
fn an_s3_bucket_signs_and_addresses_every_request() {
    let server = S3Server::start(BUCKET);
    let b = open(&server);
    let key = "team/research/tables/filings/data/runs/run 1+x/ingest-a/part-00000.parquet";
    let Put::Applied(etag) = b.put(key, b"rows", Condition::IfNoneMatch).unwrap() else { panic!("the create applies") };
    assert_eq!(server.keys(), [key], "the key lands whole, its segments decoded by the backend");
    assert_eq!(b.get(key).unwrap(), Some((b"rows".to_vec(), etag)));
    assert_eq!(b.get("team/absent.json").unwrap(), None);
    assert_eq!(b.list("team/research/").unwrap(), [key]);
    b.delete(key).unwrap();
    b.delete(key).unwrap();
    assert!(server.keys().is_empty());

    // A request signed with another secret meets the backend's signature check.
    let forged = S3Bucket::open(&server.endpoint, "us-east-1", BUCKET, credentials("not-the-secret-key-0000000000000000000000")).unwrap();
    assert!(matches!(forged.get(key), Err(ObjectError::Forbidden(_))), "a forged signature is refused");
    assert!(matches!(forged.put(key, b"x", Condition::None), Err(ObjectError::Forbidden(_))));
    assert!(server.keys().is_empty(), "no forged write lands");
    assert!(matches!(S3Bucket::open(&server.endpoint, "us-east-1", "Context_Team", credentials(SECRET_KEY)), Err(ObjectError::Unsupported(_))));
}

/// A `412` or `409`, or a `404` naming `NoSuchKey` to an `If-Match` put, answers a failed condition; a `501` answers an unsupported
/// method and a `403` a forbidden credential, both read by `store.probe.inconclusive`.
// spec: store.endpoint.conditional-answers@4c5e3b70
#[test]
fn backend_answers_map_to_failed_conditions_and_refusals() {
    let server = S3Server::start(BUCKET);
    let b = open(&server);
    let key = "team/manifest.json";
    assert_eq!(b.put(key, b"{}", Condition::IfMatch("\"0123\"".into())).unwrap(), Put::ConditionFailed, "a 404 to If-Match");
    let Put::Applied(first) = b.put(key, b"{}", Condition::IfNoneMatch).unwrap() else { panic!() };
    assert_eq!(b.put(key, b"{}", Condition::IfNoneMatch).unwrap(), Put::ConditionFailed, "a 412 to If-None-Match");
    assert_eq!(b.put(key, b"{}", Condition::IfMatch("stale".into())).unwrap(), Put::ConditionFailed, "a 412 to a stale If-Match");
    server.fail_puts(&["ConditionalRequestConflict"]);
    assert_eq!(b.put(key, b"{}", Condition::IfMatch(first.clone())).unwrap(), Put::ConditionFailed, "a 409");
    assert!(matches!(b.put(key, b"{\"v\":2}", Condition::IfMatch(first)).unwrap(), Put::Applied(_)), "the current ETag replaces");
    assert_eq!(server.object(key).unwrap(), b"{\"v\":2}");

    server.fail_puts(&["NotImplemented"]);
    assert!(matches!(b.put(key, b"{}", Condition::IfNoneMatch), Err(ObjectError::Unsupported(_))));
    server.fail_puts(&["AccessDenied"]);
    assert!(matches!(b.put(key, b"{}", Condition::IfNoneMatch), Err(ObjectError::Forbidden(_))));

    // The probe reads either refusal as capability not demonstrated.
    for code in ["NotImplemented", "AccessDenied"] {
        let a = node("ingest-a", Arc::new(open(&server)), "");
        server.fail_puts(&[code]);
        assert!(matches!(a.syncer.probe(), Err(SyncError::Store(StoreError::SyncProbeInconclusive(_)))), "{code}");
    }
}

/// A `404` naming any code but `NoSuchKey`, `NoSuchBucket` among them, answers no absent object and no failed
/// condition: the get, put, delete or list fails as transport, naming the code.
// spec: store.endpoint.missing-bucket@7e8b0846
#[test]
fn a_missing_bucket_answers_no_absent_object() {
    let server = S3Server::start(BUCKET);
    let b = S3Bucket::open(&server.endpoint, "us-east-1", "context-typo", credentials(SECRET_KEY)).unwrap();
    let key = "team/manifest.json";
    let transport = |r: Result<(), ObjectError>, what: &str| match r {
        Err(ObjectError::Transport(m)) => assert!(m.contains("NoSuchBucket"), "{what}: {m}"),
        other => panic!("{what}: expected a transport failure naming NoSuchBucket, got {other:?}"),
    };
    transport(b.get(key).map(|_| ()), "get");
    transport(b.put(key, b"{}", Condition::IfMatch("\"0123\"".into())).map(|_| ()), "If-Match put");
    transport(b.put(key, b"{}", Condition::IfNoneMatch).map(|_| ()), "If-None-Match put");
    transport(b.delete(key), "delete");
    transport(b.list("team/").map(|_| ()), "list");
    transport(b.head(key).map(|_| ()), "head");
    // The bucket that exists still answers a missing key as absent.
    assert_eq!(open(&server).get(key).unwrap(), None);
    assert!(server.keys().is_empty());
}

/// A list follows each continuation token until the backend reports the listing complete, and returns every key
/// under the prefix sorted.
// spec: store.endpoint.list-pages@4b694af7
#[test]
fn a_list_follows_every_continuation_token() {
    let server = S3Server::start_paged(BUCKET, 2);
    let b = open(&server);
    for k in ["team/e", "team/a", "team/d", "other/z", "team/c", "team/b"] {
        b.put(k, k.as_bytes(), Condition::IfNoneMatch).unwrap();
    }
    assert_eq!(b.list("team/").unwrap(), ["team/a", "team/b", "team/c", "team/d", "team/e"]);
    assert_eq!(b.list("").unwrap().len(), 6);
    assert!(b.list("none/").unwrap().is_empty());
}

/// A head request answers an object's ETag and a tagged listing answers each key's ETag, both
/// without transferring an object's bytes.
#[test]
fn a_head_and_a_tagged_listing_answer_etags_without_bytes() {
    let server = S3Server::start_paged(BUCKET, 2);
    let b = open(&server);
    let mut tags = Vec::new();
    for k in ["feed/03.jsonl", "feed/01.jsonl", "feed/02.jsonl", "other/01.jsonl"] {
        let Put::Applied(tag) = b.put(k, k.as_bytes(), Condition::IfNoneMatch).unwrap() else { panic!("{k}") };
        tags.push((k, tag));
    }
    let tag = |k: &str| tags.iter().find(|(key, _)| *key == k).unwrap().1.clone();
    let before = server.gets();
    assert_eq!(b.head("feed/01.jsonl").unwrap(), Some(tag("feed/01.jsonl")));
    let listed = b.list_tagged("feed/").unwrap();
    assert_eq!(listed, ["feed/01.jsonl", "feed/02.jsonl", "feed/03.jsonl"].map(|k| (k.to_string(), tag(k))));
    assert_eq!(server.gets(), before, "neither transfers an object");
    assert_eq!(b.head("feed/absent.jsonl").unwrap(), None, "a missing key in a bucket that exists answers absent");

    let missing = S3Bucket::open(&server.endpoint, "us-east-1", "context-typo", credentials(SECRET_KEY)).unwrap();
    assert!(matches!(missing.list_tagged("feed/"), Err(ObjectError::Transport(m)) if m.contains("NoSuchBucket")));
}

/// Two nodes sharing one S3 bucket probe `cas`, push at once, and converge on each other's runs.
#[test]
fn two_nodes_converge_through_one_s3_bucket() {
    let server = S3Server::start(BUCKET);
    let a = node("ingest-a", Arc::new(open(&server)), "");
    let b = node("ingest-b", Arc::new(open(&server)), "");
    assert_eq!(a.syncer.probe().unwrap(), Coordination::Cas);
    assert_eq!(server.keys().iter().filter(|k| k.contains("cas-probe")).count(), 0, "the sentinel the probe wrote leaves");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    b.land("run-1", json!([{"id": 2}]), "2030-01-01T00:00:01Z");
    let now = at("2030-01-01T00:01:00Z");
    std::thread::scope(|s| {
        let pa = s.spawn(|| a.syncer.push(now));
        let pb = s.spawn(|| b.syncer.push(now));
        pa.join().unwrap().unwrap();
        pb.join().unwrap().unwrap();
    });
    a.syncer.pull(&PullScope::default()).unwrap();
    b.syncer.pull(&PullScope::default()).unwrap();
    let runs = |n: &crate::support::Node| -> Vec<String> {
        let dir = n.root().join("tables/filings/data/runs/run-1");
        let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    };
    assert_eq!(runs(&a), ["ingest-a", "ingest-b"]);
    assert_eq!(runs(&b), ["ingest-a", "ingest-b"]);
    let manifest: serde_json::Value = serde_json::from_slice(&server.object("team/manifest.json").unwrap()).unwrap();
    assert!(manifest["entries"].as_object().unwrap().keys().any(|k| k.contains("ingest-a")) && manifest["entries"].as_object().unwrap().keys().any(|k| k.contains("ingest-b")));
}

/// An S3 endpoint's outbound path can drop a request or hold it open without an answer.
mod transport {
    use contextful_core::connector::reference::Hydrated;
    use contextful_core::store::object::{Condition, ObjectStore, Put};
    use contextful_sync::{S3Bucket, S3Credentials};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    #[derive(Clone)]
    enum Answer {
        /// Close the connection unanswered.
        Drop,
        /// Hold the connection open and never answer.
        Stall,
        /// Answer with this status and body.
        Status(u16, &'static str),
    }

    /// An endpoint stand-in answering requests in arrival order and logging each request line.
    struct Endpoint {
        url: String,
        log: Arc<Mutex<Vec<String>>>,
    }

    impl Endpoint {
        fn start(script: Vec<Answer>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let log = Arc::new(Mutex::new(Vec::new()));
            let script = Arc::new(Mutex::new(script.into_iter()));
            let shared = log.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let (log, script) = (shared.clone(), script.clone());
                    std::thread::spawn(move || serve(stream.unwrap(), &log, &script));
                }
            });
            Self { url, log }
        }

        fn requests(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }

        fn bucket(&self) -> S3Bucket {
            let credentials = S3Credentials { access_key_id: Hydrated::new("synthetic"), secret_access_key: Hydrated::new("synthetic"), session_token: None };
            S3Bucket::open(&self.url, "auto", "corpus", credentials).unwrap()
        }
    }

    fn serve(stream: TcpStream, log: &Mutex<Vec<String>>, script: &Mutex<std::vec::IntoIter<Answer>>) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let mut length = 0;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header == "\r\n" {
                    break;
                }
                if let Some((name, value)) = header.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse().unwrap();
                    }
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let method = line.split_whitespace().next().unwrap_or_default().to_string();
            log.lock().unwrap().push(method.clone());
            let answer = script.lock().unwrap().next().unwrap_or(Answer::Status(200, ""));
            match answer {
                Answer::Drop => return,
                Answer::Stall => {
                    std::thread::sleep(Duration::from_secs(120));
                    return;
                }
                Answer::Status(status, body) => {
                    let body = if method == "HEAD" { "" } else { body };
                    write!(writer, "HTTP/1.1 {status} X\r\netag: \"e1\"\r\ncontent-length: {}\r\n\r\n{body}", body.len()).unwrap();
                }
            }
        }
    }

    const LISTING: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Name>corpus</Name><Prefix></Prefix><KeyCount>1</KeyCount><MaxKeys>1000</MaxKeys><IsTruncated>false</IsTruncated><Contents><Key>a</Key><LastModified>2030-01-01T00:00:00.000Z</LastModified><ETag>\"e1\"</ETag><Size>1</Size><StorageClass>STANDARD</StorageClass></Contents></ListBucketResult>";

    #[test]
    fn a_stalled_get_gives_up_its_attempt_within_the_answer_bound_and_resends() {
        let endpoint = Endpoint::start(vec![Answer::Stall, Answer::Status(200, "x")]);
        let started = Instant::now();
        let got = endpoint.bucket().get("a").unwrap();
        assert_eq!(got, Some((b"x".to_vec(), "\"e1\"".into())));
        // 3 attempts at this bound, with their backoff, fit inside a 60 s process deadline.
        assert!(started.elapsed() < Duration::from_secs(18), "{:?}", started.elapsed());
        assert_eq!(endpoint.requests(), ["GET", "GET"]);
    }

    #[test]
    fn a_dropped_read_resends_within_three_attempts() {
        let endpoint = Endpoint::start(vec![Answer::Drop, Answer::Drop, Answer::Status(200, "")]);
        endpoint.bucket().head("a").unwrap();
        assert_eq!(endpoint.requests(), ["HEAD", "HEAD", "HEAD"]);
        let endpoint = Endpoint::start(vec![Answer::Drop, Answer::Status(200, LISTING)]);
        assert_eq!(endpoint.bucket().list("").unwrap(), ["a"]);
        assert_eq!(endpoint.requests(), ["GET", "GET"]);
        let endpoint = Endpoint::start(vec![Answer::Drop; 8]);
        assert!(endpoint.bucket().get("a").is_err());
        assert_eq!(endpoint.requests().len(), 3);
    }

    #[test]
    fn an_unanswered_or_server_failed_request_fails_as_unavailable() {
        use contextful_core::store::object::ObjectError;
        let endpoint = Endpoint::start(vec![Answer::Drop; 3]);
        assert!(matches!(endpoint.bucket().list("corpus/"), Err(ObjectError::Unavailable(m)) if m.contains("LIST")));
        let endpoint = Endpoint::start(vec![Answer::Status(503, "<Error><Code>SlowDown</Code></Error>")]);
        assert!(matches!(endpoint.bucket().get("a"), Err(ObjectError::Unavailable(m)) if m.contains("503 SlowDown")));
        assert_eq!(endpoint.requests(), ["GET"], "an answered status never resends");
    }

    #[test]
    fn an_unconditional_put_resends_and_a_conditional_put_goes_once() {
        let endpoint = Endpoint::start(vec![Answer::Drop, Answer::Status(200, "")]);
        assert_eq!(endpoint.bucket().put("a", b"x", Condition::None).unwrap(), Put::Applied("\"e1\"".into()));
        assert_eq!(endpoint.requests(), ["PUT", "PUT"]);
        for condition in [Condition::IfNoneMatch, Condition::IfMatch("\"e0\"".into())] {
            let endpoint = Endpoint::start(vec![Answer::Drop, Answer::Status(200, "")]);
            assert!(endpoint.bucket().put("a", b"x", condition).is_err());
            assert_eq!(endpoint.requests(), ["PUT"], "a conditional put whose answer is lost may have applied");
        }
    }
}
