//! The `s3` object source over the object-store port: addressing, listing, gzip, the
//! expansion ceiling, whole-read refusal, ETag skipping and credential references.

use crate::support::{request, Never};
use contextful_connectors::http::ConfigError;
use contextful_connectors::object::{ObjectConfig, ObjectSource, EXPANSION_CEILING};
use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::Source;
use contextful_core::store::object::{Condition, ObjectError, ObjectStore, Put};
use flate2::write::GzEncoder;
use flate2::Compression;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::{Arc, Mutex};

/// A bucket in memory: each put mints the next ETag, and every get is counted.
#[derive(Default)]
pub struct MemBucket {
    objects: Mutex<BTreeMap<String, (Vec<u8>, String)>>,
    minted: Mutex<u64>,
    pub gets: Mutex<Vec<String>>,
}

impl MemBucket {
    pub fn with(objects: &[(&str, &[u8])]) -> Arc<MemBucket> {
        let b = Arc::new(MemBucket::default());
        for (k, v) in objects {
            b.put(k, v, Condition::None).unwrap();
        }
        b
    }
}

impl ObjectStore for MemBucket {
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, String)>, ObjectError> {
        self.gets.lock().unwrap().push(key.to_string());
        Ok(self.objects.lock().unwrap().get(key).cloned())
    }
    fn put(&self, key: &str, bytes: &[u8], _: Condition) -> Result<Put, ObjectError> {
        let mut n = self.minted.lock().unwrap();
        *n += 1;
        let etag = format!("\"e{n}\"");
        self.objects.lock().unwrap().insert(key.to_string(), (bytes.to_vec(), etag.clone()));
        Ok(Put::Applied(etag))
    }
    fn delete(&self, key: &str) -> Result<(), ObjectError> {
        self.objects.lock().unwrap().remove(key);
        Ok(())
    }
    fn list(&self, prefix: &str) -> Result<Vec<String>, ObjectError> {
        Ok(self.objects.lock().unwrap().keys().filter(|k| k.starts_with(prefix)).cloned().collect())
    }
}

fn gz(body: &[u8]) -> Vec<u8> {
    let mut e = GzEncoder::new(Vec::new(), Compression::fast());
    e.write_all(body).unwrap();
    e.finish().unwrap()
}

fn source(config: Value, bucket: Arc<MemBucket>) -> ObjectSource {
    ObjectSource::new(ObjectConfig::parse(&config).unwrap(), bucket)
}

/// Object CSV reads share the declared character decoder used by HTTP CSV reads.
#[test]
fn an_object_csv_decodes_its_declared_encoding_and_refuses_invalid_bytes() {
    let bucket = MemBucket::with(&[("names.csv", b"name\ncaf\xe9\n"), ("bad.csv", b"name\n\x81\n")]);
    let mut good = source(json!({"bucket":"b", "key":"names.csv", "encoding":"windows-1252"}), bucket.clone());
    assert_eq!(read(&mut good, None).0, vec![json!({"name":"café"})]);
    let mut bad = source(json!({"bucket":"b", "key":"bad.csv", "encoding":"shift_jis"}), bucket);
    assert!(bad.pull(&request(None), &Never).unwrap_err().message.contains("ConnectorEncodingInvalid"));
}

/// One read from `position`: its rows and the position after it.
fn read(s: &mut ObjectSource, position: Option<Value>) -> (Vec<Value>, Option<Value>) {
    let pulled: Value = serde_json::from_slice(&s.pull(&request(position), &Never).unwrap()).unwrap();
    assert_eq!(pulled["more"], json!(false));
    (pulled["rows"].as_array().cloned().unwrap_or_default(), pulled.get("cursor").cloned().filter(|c| !c.is_null()))
}

fn refusal(config: Value) -> ConfigError {
    ObjectConfig::parse(&config).unwrap_err()
}

/// Declaring both or neither of `key` and `prefix` raises `ConnectorObjectAddressRejected` at build.
// spec: connector.source.object-address@24520c58
#[test]
fn a_key_and_a_prefix_together_or_neither_are_refused_at_build() {
    for config in [json!({"bucket": "b", "key": "k.jsonl", "prefix": "p/"}), json!({"bucket": "b", "format": "jsonl"})] {
        match refusal(config.clone()) {
            ConfigError::Connector(ConnectorError::ConnectorObjectAddressRejected(m)) => assert!(m.contains("`key`") && m.contains("`prefix`"), "{m}"),
            other => panic!("{config}: {other}"),
        }
    }
    assert!(ObjectConfig::parse(&json!({"bucket": "b", "key": "k.jsonl", "format": "jsonl"})).is_ok());
    assert!(ObjectConfig::parse(&json!({"bucket": "b", "prefix": "p/", "format": "jsonl"})).is_ok());
    assert!(matches!(refusal(json!({"key": "k.jsonl"})), ConfigError::Run(_)), "a bucket is required");
    assert!(matches!(refusal(json!({"bucket": "b", "key": "k", "sheet_name": "x"})), ConfigError::Run(_)), "an unread key is refused");
}

/// A prefix read lists every key under the prefix in key order, narrowed by `suffix`;
/// `pick = "latest"` lands the greatest key alone.
// spec: connector.source.prefix-listing@60f5022d
#[test]
fn a_prefix_lands_each_listed_object_in_key_order_and_latest_lands_one() {
    let bucket = MemBucket::with(&[
        ("feed/2026-02.jsonl", b"{\"n\":3}\n"),
        ("feed/2026-01.jsonl", b"{\"n\":1}\n{\"n\":2}\n"),
        ("feed/readme.txt", b"not a record"),
        ("feedback/2026-03.jsonl", b"{\"n\":9}\n"),
        ("other/2026-01.jsonl", b"{\"n\":8}\n"),
    ]);
    let mut all = source(json!({"bucket": "b", "prefix": "feed/", "suffix": ".jsonl", "format": "jsonl"}), bucket.clone());
    let (rows, _) = read(&mut all, None);
    assert_eq!(rows, vec![json!({"n": 1}), json!({"n": 2}), json!({"n": 3})]);

    let mut latest = source(json!({"bucket": "b", "prefix": "feed/", "suffix": ".jsonl", "format": "jsonl", "pick": "latest"}), bucket.clone());
    let (rows, _) = read(&mut latest, None);
    assert_eq!(rows, vec![json!({"n": 3})]);

    assert!(matches!(refusal(json!({"bucket": "b", "prefix": "p/", "pick": "oldest"})), ConfigError::Run(_)));
    assert!(matches!(refusal(json!({"bucket": "b", "key": "k", "pick": "all"})), ConfigError::Run(_)), "`pick` narrows a listing alone");
}

/// `compression = "gzip"`, or a `.gz` key with `compression` undeclared, decompresses before
/// decode; `compression = "none"` reads the object as stored.
// spec: connector.source.object-gzip@1d996fa5
#[test]
fn a_gz_key_or_declared_gzip_decompresses_and_none_reads_as_stored() {
    let bucket = MemBucket::with(&[
        ("a.csv.gz", &gz(b"id,v\n1,x\n")),
        ("b.csv", &gz(b"id,v\n2,y\n")),
        ("c.csv.gz", b"id,v\n3,z\n"),
    ]);
    let (rows, _) = read(&mut source(json!({"bucket": "b", "key": "a.csv.gz", "format": "csv"}), bucket.clone()), None);
    assert_eq!(rows, vec![json!({"id": "1", "v": "x"})]);
    let (rows, _) = read(&mut source(json!({"bucket": "b", "key": "b.csv", "format": "csv", "compression": "gzip"}), bucket.clone()), None);
    assert_eq!(rows, vec![json!({"id": "2", "v": "y"})]);
    let (rows, _) = read(&mut source(json!({"bucket": "b", "key": "c.csv.gz", "format": "csv", "compression": "none"}), bucket.clone()), None);
    assert_eq!(rows, vec![json!({"id": "3", "v": "z"})]);
    assert!(matches!(refusal(json!({"bucket": "b", "key": "k", "compression": "zstd"})), ConfigError::Run(_)));
}

/// One object decompresses to at most 256 MiB; past it the read refuses whole, naming the
/// object, and lands nothing.
// spec: connector.source.object-expansion@12b9ca72
#[test]
fn an_object_expanding_past_256_mib_refuses_naming_it_and_lands_nothing() {
    assert_eq!(EXPANSION_CEILING, 256 * 1024 * 1024);
    let mut body = b"{\"n\":1}\n".to_vec();
    body.resize(EXPANSION_CEILING as usize + 1, b'\n');
    let bucket = MemBucket::with(&[("big/1.jsonl.gz", &gz(b"{\"n\":0}\n")), ("big/2.jsonl.gz", &gz(&body))]);
    let mut s = source(json!({"bucket": "lake", "prefix": "big/", "format": "jsonl"}), bucket);
    let f = s.pull(&request(None), &Never).unwrap_err();
    assert!(f.message.contains("s3://lake/big/2.jsonl.gz") && f.message.contains(&EXPANSION_CEILING.to_string()), "{f}");
    assert!(f.deterministic, "a ceiling over static bytes is deterministic");
}

/// An object a decoder cannot read refuses the whole read, naming `s3://<bucket>/<key>`,
/// and no other object of the read lands.
// spec: connector.source.object-unreadable@a4fe1b2d
#[test]
fn one_unreadable_object_refuses_the_whole_read_naming_it() {
    let bucket = MemBucket::with(&[("d/1.jsonl", b"{\"n\":1}\n"), ("d/2.jsonl", b"{\"n\":2}\n[3]\n"), ("d/3.jsonl", b"{\"n\":4}\n")]);
    let mut s = source(json!({"bucket": "lake", "prefix": "d/", "format": "jsonl"}), bucket);
    let f = s.pull(&request(None), &Never).unwrap_err();
    assert!(f.message.starts_with("PipelineUnreadableInput") && f.message.contains("s3://lake/d/2.jsonl") && f.message.contains("line 2"), "{f}");
}

/// With `skip_unchanged = true`, the position maps each key to its ETag; a matching ETag
/// lands nothing, and a read matching every object keeps the position.
// spec: connector.source.etag-skip@d98d2508
#[test]
fn an_unchanged_etag_lands_nothing_and_a_new_object_lands_alone() {
    let bucket = MemBucket::with(&[("snap.jsonl", b"{\"id\":1}\n{\"id\":2}\n")]);
    let mut s = source(json!({"bucket": "b", "key": "snap.jsonl", "format": "jsonl", "skip_unchanged": true}), bucket.clone());
    let (rows, first) = read(&mut s, None);
    assert_eq!(rows.len(), 2);
    assert_eq!(first, Some(json!({"objects": {"snap.jsonl": "\"e1\""}})));
    let (rows, second) = read(&mut s, first.clone());
    assert!(rows.is_empty(), "an unchanged ETag lands nothing");
    assert_eq!(second, first, "a skip holds the position");
    bucket.put("snap.jsonl", b"{\"id\":1}\n{\"id\":2}\n{\"id\":3}\n", Condition::None).unwrap();
    let (rows, third) = read(&mut s, second);
    assert_eq!(rows.len(), 3, "a rewritten object lands whole");
    assert_eq!(third, Some(json!({"objects": {"snap.jsonl": "\"e2\""}})));

    // A prefix of three gzip objects lands every row in key order; a fourth lands alone.
    let series = MemBucket::with(&[
        ("s/01.csv.gz", &gz(b"id\n1\n")),
        ("s/02.csv.gz", &gz(b"id\n2\n3\n")),
        ("s/03.csv.gz", &gz(b"id\n4\n")),
    ]);
    let mut s = source(json!({"bucket": "b", "prefix": "s/", "format": "csv", "skip_unchanged": true}), series.clone());
    let (rows, at) = read(&mut s, None);
    assert_eq!(rows, vec![json!({"id": "1"}), json!({"id": "2"}), json!({"id": "3"}), json!({"id": "4"})]);
    series.put("s/04.csv.gz", &gz(b"id\n5\n"), Condition::None).unwrap();
    let (rows, at) = read(&mut s, at);
    assert_eq!(rows, vec![json!({"id": "5"})]);
    assert_eq!(at.unwrap()["objects"].as_object().unwrap().len(), 4);

    // Undeclared, every read lands every object and carries no position.
    let mut plain = source(json!({"bucket": "b", "key": "snap.jsonl", "format": "jsonl"}), bucket);
    let (rows, at) = read(&mut plain, None);
    assert_eq!((rows.len(), at), (3, None));
}

/// `access_key_id` and `secret_access_key` each hold one `secret://<name>` reference,
/// declared together; a literal in either is refused at parse.
// spec: connector.source.object-credentials@12d6f5bc
#[test]
fn a_literal_credential_is_refused_and_a_reference_parses() {
    let ok = ObjectConfig::parse(&json!({
        "bucket": "b", "key": "k.jsonl",
        "access_key_id": "secret://bucket-key-id", "secret_access_key": "secret://bucket-secret"
    }))
    .unwrap();
    let names: Vec<String> = ok.credentials().flat_map(|t| t.names().map(|n| n.as_str().to_string()).collect::<Vec<_>>()).collect();
    assert_eq!(names, ["bucket-key-id", "bucket-secret"]);
    for (k, v) in [("access_key_id", "AKIAIOSFODNN7EXAMPLE"), ("secret_access_key", "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"), ("secret_access_key", "plain")] {
        let mut config = json!({"bucket": "b", "key": "k.jsonl", "access_key_id": "secret://id", "secret_access_key": "secret://s"});
        config[k] = json!(v);
        match refusal(config) {
            ConfigError::Connector(ConnectorError::SecretMaterialInDeclaration(m)) => assert!(m.contains(k), "{m}"),
            other => panic!("{k} = {v}: {other}"),
        }
    }
    assert!(matches!(refusal(json!({"bucket": "b", "key": "k", "access_key_id": "secret://id"})), ConfigError::Run(_)), "a key pair binds both");
}

/// A signing source never addresses a cleartext endpoint off loopback; an unsigned one may,
/// carrying no capability to leak.
// spec: connector.source.object-cleartext@31d9b13c
#[test]
fn a_signing_source_refuses_a_cleartext_endpoint_off_loopback() {
    let signed = json!({
        "bucket": "lake", "endpoint": "http://objects.vendor.example", "key": "k.jsonl",
        "access_key_id": "secret://lake-id", "secret_access_key": "secret://lake-secret"
    });
    match refusal(signed.clone()) {
        ConfigError::Connector(ConnectorError::SecretCleartextEndpoint(m)) => assert!(m.contains("objects.vendor.example"), "{m}"),
        other => panic!("{other}"),
    }
    let mut loopback = signed;
    loopback["endpoint"] = json!("http://127.0.0.1:9000");
    assert!(ObjectConfig::parse(&loopback).is_ok());
    assert!(ObjectConfig::parse(&json!({"bucket": "lake", "key": "k.jsonl", "endpoint": "http://objects.vendor.example"})).is_ok());
}
