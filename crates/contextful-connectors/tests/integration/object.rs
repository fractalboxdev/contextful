//! The `s3` object source over the object-store port: addressing, listing, gzip, the
//! expansion ceiling, whole-read refusal, ETag skipping and credential references.

use crate::support::{request, Never};
use contextful_connectors::http::ConfigError;
use contextful_connectors::body::EXPANSION_CEILING;
use contextful_connectors::object::{ObjectConfig, ObjectSource};
use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::Source;
use contextful_core::run::RunError;
use contextful_core::store::object::{Condition, ObjectError, ObjectStore, Put};
use contextful_core::store::StoreError;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
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

pub(crate) fn gz(body: &[u8]) -> Vec<u8> {
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

/// An address covers the keys its read lists: a fixed key alone, or a key under the prefix
/// ending in the suffix.
#[test]
fn an_address_covers_exactly_the_keys_its_read_lists() {
    let address = |config: Value| ObjectConfig::parse(&config).unwrap().address;
    let prefix = address(json!({"bucket": "b", "prefix": "feed/", "suffix": ".jsonl", "format": "jsonl"}));
    assert!(prefix.covers("feed/2026-01.jsonl"));
    for key in ["feed/readme.txt", "feedback/2026-03.jsonl", "other/2026-01.jsonl"] {
        assert!(!prefix.covers(key), "{key}");
    }
    assert!(address(json!({"bucket": "b", "prefix": "feed/", "format": "jsonl", "pick": "latest"})).covers("feed/readme.txt"), "no suffix narrows nothing");
    let fixed = address(json!({"bucket": "b", "key": "feed/orders.jsonl", "format": "jsonl"}));
    assert!(fixed.covers("feed/orders.jsonl") && !fixed.covers("feed/orders.jsonl.gz") && !fixed.covers("feed/"));
}

/// `compression = "gzip"`, or a `.gz` key with `compression` undeclared, decompresses before
/// decode; `compression = "none"` reads the object as stored.
// spec: connector.source.object-gzip@255bd183
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

/// One object decompresses to at most 256 MiB, counted as the bytes arrive. Past it the object lands nothing and is
/// unreadable under {{connector.source.object-unreadable}}, named with the ceiling.
// spec: connector.source.object-expansion@61ca643d
#[test]
fn an_object_expanding_past_256_mib_lands_nothing_and_is_named_with_the_ceiling() {
    assert_eq!(EXPANSION_CEILING, 256 * 1024 * 1024);
    let mut body = b"{\"n\":1}\n".to_vec();
    body.resize(EXPANSION_CEILING as usize + 1, b'\n');
    let bucket = MemBucket::with(&[("big/1.jsonl.gz", &gz(b"{\"n\":0}\n")), ("big/2.jsonl.gz", &gz(&body))]);
    let pulled = pull_json(&mut source(json!({"bucket": "lake", "prefix": "big/", "format": "jsonl"}), bucket.clone()), None).unwrap();
    assert_eq!(pulled["rows"], json!([{"n": 0}]), "no byte of the oversize object lands");
    let reason = pulled["dead_letters"][0]["reason"].as_str().unwrap_or_default();
    assert!(reason.contains("s3://lake/big/2.jsonl.gz") && reason.contains(&EXPANSION_CEILING.to_string()), "{pulled}");
    // A read of the oversize object alone refuses, naming it and the ceiling.
    let f = source(json!({"bucket": "lake", "key": "big/2.jsonl.gz", "format": "jsonl"}), bucket).pull(&request(None), &Never).unwrap_err();
    assert!(f.message.contains("s3://lake/big/2.jsonl.gz") && f.message.contains(&EXPANSION_CEILING.to_string()), "{f}");
    assert!(f.deterministic, "a ceiling over static bytes is deterministic");
}

/// One pull as the runner receives it.
fn pull_json(s: &mut ObjectSource, position: Option<Value>) -> Result<Value, contextful_core::run::Failure> {
    Ok(serde_json::from_slice(&s.pull(&request(position), &Never)?).unwrap())
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// An object a decoder cannot read, or one past {{connector.source.object-expansion}}'s ceiling, lands no rows and one
/// {{run.land.dead-letter}} row; the rest of the read lands.
// spec: connector.source.object-unreadable@c5767ace
#[test]
fn an_unreadable_object_lands_a_dead_letter_and_the_rest_of_the_read_lands() {
    let bad = b"{\"n\":2}\n[3]\n";
    let bucket = MemBucket::with(&[("d/1.jsonl", b"{\"n\":1}\n"), ("d/2.jsonl", bad), ("d/3.jsonl", b"{\"n\":4}\n")]);
    let pulled = pull_json(&mut source(json!({"bucket": "lake", "prefix": "d/", "format": "jsonl"}), bucket), None).unwrap();
    assert_eq!(pulled["rows"], json!([{"n": 1}, {"n": 4}]), "no row of the unreadable object lands");
    assert_eq!(pulled["skipped"], json!(1));
    let letters = pulled["dead_letters"].as_array().cloned().unwrap_or_default();
    assert_eq!(letters.len(), 1, "{pulled}");
    assert_eq!((&letters[0]["key"], &letters[0]["sha256"]), (&json!("d/2.jsonl"), &json!(sha256(bad))));
    let reason = letters[0]["reason"].as_str().unwrap();
    assert!(reason.starts_with("PipelineUnreadableInput") && reason.contains("s3://lake/d/2.jsonl") && reason.contains("line 2"), "{reason}");
}

/// A position holds a dead-lettered object at its ETag, so a later read skips it until the ETag changes; with
/// `replay_dead_letters = true` a read decodes it again.
// spec: connector.source.unreadable-position@1e81082c
// spec: connector.source.dead-letter-replay@91122ea9
#[test]
fn a_dead_lettered_object_is_held_at_its_etag_until_it_changes_or_a_replay_reads_it() {
    let bucket = MemBucket::with(&[("d/1.jsonl", b"{\"n\":1}\n"), ("d/2.jsonl", b"[2]\n")]);
    let config = json!({"bucket": "lake", "prefix": "d/", "format": "jsonl", "skip_unchanged": true});
    let first = pull_json(&mut source(config.clone(), bucket.clone()), None).unwrap();
    assert_eq!(first["cursor"], json!({"objects": {"d/1.jsonl": "\"e1\"", "d/2.jsonl": "\"e2\""}, "dead_letters": ["d/2.jsonl"]}));
    let second = pull_json(&mut source(config.clone(), bucket.clone()), Some(first["cursor"].clone())).unwrap();
    assert_eq!((&second["rows"], second.get("dead_letters")), (&json!([]), None), "an unchanged ETag lands nothing, dead-lettered or not");
    assert_eq!(second["cursor"], first["cursor"], "the dead letter stays held");

    let mut replay = config.clone();
    replay["replay_dead_letters"] = json!(true);
    let replayed = pull_json(&mut source(replay.clone(), bucket.clone()), Some(first["cursor"].clone())).unwrap();
    assert_eq!(replayed["rows"], json!([]), "a replay reads the dead-lettered object alone");
    assert_eq!(replayed["dead_letters"][0]["key"], json!("d/2.jsonl"), "a replay of a still unreadable object dead-letters it again");

    bucket.put("d/2.jsonl", b"{\"n\":2}\n", Condition::None).unwrap();
    let fixed = pull_json(&mut source(config, bucket), Some(first["cursor"].clone())).unwrap();
    assert_eq!(fixed["rows"], json!([{"n": 2}]), "a changed ETag reads the object again");
    assert_eq!(fixed["cursor"], json!({"objects": {"d/1.jsonl": "\"e1\"", "d/2.jsonl": "\"e3\""}}), "a readable object leaves the dead letters");
    assert!(ObjectConfig::parse(&json!({"bucket": "b", "key": "k", "replay_dead_letters": "yes"})).is_err());
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
// spec: connector.source.object-credentials@1c02d9dc
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

/// `session_token` binds one `secret://<name>` reference beside the key pair and joins the
/// templates hydrated per read; a literal refuses, and a token without the pair names no key.
#[test]
fn a_session_token_reference_joins_the_key_pair() {
    let pair = json!({"bucket": "b", "key": "k.jsonl", "access_key_id": "secret://id", "secret_access_key": "secret://s", "session_token": "secret://lake-token"});
    let ok = ObjectConfig::parse(&pair).unwrap();
    let names: Vec<String> = ok.credentials().flat_map(|t| t.names().map(|n| n.as_str().to_string()).collect::<Vec<_>>()).collect();
    assert_eq!(names, ["id", "s", "lake-token"]);
    assert!(ok.session_token.is_some());
    let mut literal = pair.clone();
    literal["session_token"] = json!("IQoJb3JpZ2luX2VjEXAMPLETOKEN");
    match refusal(literal) {
        ConfigError::Connector(ConnectorError::SecretMaterialInDeclaration(m)) => assert!(m.contains("session_token"), "{m}"),
        other => panic!("{other}"),
    }
    let alone = json!({"bucket": "b", "key": "k.jsonl", "session_token": "secret://lake-token"});
    assert!(matches!(refusal(alone), ConfigError::Run(_)), "a token signs only beside the key pair");
}

/// `credentials = "role"` selects workload-role signing, alone: beside any key it refuses as
/// `SyncCredentialConflict`, and a cleartext endpoint off loopback refuses as for a key pair.
#[test]
fn role_credentials_replace_the_key_pair_and_refuse_beside_a_key() {
    let role = ObjectConfig::parse(&json!({"bucket": "b", "key": "k.jsonl", "credentials": "role"})).unwrap();
    assert!(role.role && role.access_key_id.is_none());
    assert_eq!(role.credentials().count(), 0, "a role read hydrates no reference");
    for (k, v) in [("access_key_id", "secret://id"), ("secret_access_key", "secret://s"), ("session_token", "secret://t")] {
        let mut config = json!({"bucket": "b", "key": "k.jsonl", "credentials": "role", "access_key_id": "secret://id", "secret_access_key": "secret://s"});
        if k == "session_token" {
            config[k] = json!(v);
        }
        match refusal(config) {
            ConfigError::Store(StoreError::SyncCredentialConflict(m)) => assert!(m.contains("access_key_id") && m.contains("s3.amazonaws.com"), "{k}: {m}"),
            other => panic!("{k}: {other}"),
        }
    }
    assert!(matches!(refusal(json!({"bucket": "b", "key": "k", "credentials": "instance"})), ConfigError::Run(_)));
    match refusal(json!({"bucket": "b", "key": "k.jsonl", "credentials": "role", "endpoint": "http://objects.vendor.example"})) {
        ConfigError::Connector(ConnectorError::SecretCleartextEndpoint(m)) => assert!(m.contains("objects.vendor.example"), "{m}"),
        other => panic!("{other}"),
    }
}

/// A signing source never addresses a cleartext endpoint off loopback; an unsigned one may,
/// carrying no capability to leak.
// spec: connector.source.object-cleartext@80d4cece
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


#[test]
fn an_object_source_explicitly_reads_one_json_object() {
    let bucket = MemBucket::with(&[("snapshot.json", br#"{"data":{"items":[]},"next":"ignored"}"#)]);
    let mut selected = source(json!({"bucket":"b","key":"snapshot.json","format":"json-object","records":"/data"}), bucket.clone());
    assert_eq!(read(&mut selected, None).0, vec![json!({"items":[]})]);
    let mut root = source(json!({"bucket":"b","key":"snapshot.json","format":"json-object"}), bucket.clone());
    assert_eq!(read(&mut root, None).0, vec![json!({"data":{"items":[]},"next":"ignored"})]);
    let mut inferred = source(json!({"bucket":"b","key":"snapshot.json"}), bucket);
    assert!(inferred.pull(&request(None), &Never).unwrap_err().message.contains("PipelineUnreadableInput"));
}

/// A bucket that leaves every request unanswered.
struct UnansweredBucket;

impl ObjectStore for UnansweredBucket {
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, String)>, ObjectError> {
        Err(ObjectError::Unavailable(format!("GET `{key}`: io: Connection refused")))
    }
    fn put(&self, key: &str, _: &[u8], _: Condition) -> Result<Put, ObjectError> {
        Err(ObjectError::Unavailable(format!("PUT `{key}`: io: Connection refused")))
    }
    fn delete(&self, key: &str) -> Result<(), ObjectError> {
        Err(ObjectError::Unavailable(format!("DELETE `{key}`: io: Connection refused")))
    }
    fn list(&self, prefix: &str) -> Result<Vec<String>, ObjectError> {
        Err(ObjectError::Unavailable(format!("LIST `{prefix}`: io: Connection refused")))
    }
}

/// An unanswered bucket is a transient read failure, retried like any transport loss.
#[test]
fn an_unanswered_bucket_fails_the_read_as_transient() {
    let config = ObjectConfig::parse(&json!({"bucket": "lake", "prefix": "d/", "format": "jsonl"})).unwrap();
    let mut s = ObjectSource::new(config, Arc::new(UnansweredBucket));
    let f = s.pull(&request(None), &Never).unwrap_err();
    assert_eq!(f.tag, contextful_core::run::failure::FailureTag::Transient, "{f}");
    assert!(f.message.contains("s3://lake/") && f.message.contains("Connection refused"), "{f}");
}

const GDELT: &[u8] = include_bytes!("../fixtures/gdelt/20250101000000.export.CSV");
const GDELT_COLUMNS: [&str; 8] = ["GlobalEventID", "Day", "MonthYear", "Year", "FractionDate", "Actor1Code", "Actor1Name", "Actor1CountryCode"];

pub(crate) fn zipped(members: &[(&str, &[u8])]) -> Vec<u8> {
    use zip::write::SimpleFileOptions;
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, body) in members {
        w.start_file(*name, options).unwrap();
        w.write_all(body).unwrap();
    }
    w.finish().unwrap().into_inner()
}

/// `compression = "zip"`, or a key ending `.zip` with `compression` undeclared, decodes the
/// archive's member: a GDELT 2.0 export lands one row per line, every cell a string.
// spec: connector.source.object-gzip@255bd183
#[test]
fn a_declared_or_zip_keyed_archive_lands_its_tab_delimited_headerless_member() {
    let archive = zipped(&[("20250101000000.export.CSV", GDELT)]);
    let bucket = MemBucket::with(&[
        ("gdelt/20250101000000.export.CSV.zip", &archive),
        ("gdelt/20250101000000.export.CSV.ZIP", &archive),
        ("gdelt/export.bin", &archive),
    ]);
    let declared = json!({"bucket": "gdelt", "key": "gdelt/export.bin", "format": "csv", "compression": "zip", "delimiter": "\t", "columns": GDELT_COLUMNS});
    let keyed = json!({"bucket": "gdelt", "key": "gdelt/20250101000000.export.CSV.zip", "delimiter": "\t", "columns": GDELT_COLUMNS});
    let upper = json!({"bucket": "gdelt", "key": "gdelt/20250101000000.export.CSV.ZIP", "delimiter": "\t", "columns": GDELT_COLUMNS});
    for config in [declared, keyed, upper] {
        let (rows, _) = read(&mut source(config, bucket.clone()), None);
        assert_eq!(rows.len(), 3, "one row per line, the first line no header");
        assert_eq!(rows[0]["GlobalEventID"], json!("1210000001"));
        assert_eq!(rows[2]["Actor1Name"], json!("LONDON, UK"), "a comma is no delimiter under a declared tab");
        assert_eq!(rows[1]["Actor1Code"], Value::Null);
        assert!(rows.iter().flat_map(|r| r.as_object().unwrap().values()).all(|v| v.is_string() || v.is_null()), "every cell a string");
        assert!(rows.iter().all(|r| r.as_object().unwrap().len() == GDELT_COLUMNS.len()));
    }
}

/// An object holding a row with more or fewer fields than `columns` lands none of its rows and dead-letters, naming
/// the row; the other objects land.
#[test]
fn a_row_off_the_declared_column_count_dead_letters_its_object_naming_the_row() {
    let bucket = MemBucket::with(&[("e/1.CSV.zip", &zipped(&[("1.CSV", b"1\t2\t3\n")])), ("e/2.CSV.zip", &zipped(&[("2.CSV", b"4\t5\t6\n7\t8\n")]))]);
    let mut s = source(json!({"bucket": "lake", "prefix": "e/", "suffix": ".CSV.zip", "delimiter": "\t", "columns": ["a", "b", "c"]}), bucket);
    let pulled = pull_json(&mut s, None).unwrap();
    assert_eq!(pulled["rows"], json!([{"a": "1", "b": "2", "c": "3"}]));
    let reason = pulled["dead_letters"][0]["reason"].as_str().unwrap_or_default();
    assert!(reason.starts_with("PipelineUnreadableInput") && reason.contains("s3://lake/e/2.CSV.zip") && reason.contains("line 2"), "{pulled}");
}

/// A two-member archive without `member` refuses; with `member` it lands that member alone.
#[test]
fn a_two_member_archive_refuses_without_member_and_lands_the_named_one() {
    let bucket = MemBucket::with(&[("filing.zip", &zipped(&[("a.csv", b"id\n1\n"), ("b.csv", b"id\n2\n")]))]);
    let mut s = source(json!({"bucket": "b", "key": "filing.zip", "format": "csv"}), bucket.clone());
    let f = s.pull(&request(None), &Never).unwrap_err();
    assert!(f.message.starts_with("PipelineUnreadableInput") && f.message.contains("s3://b/filing.zip") && f.message.contains("`member`"), "{f}");
    let (rows, _) = read(&mut source(json!({"bucket": "b", "key": "filing.zip", "format": "csv", "member": "b.csv"}), bucket), None);
    assert_eq!(rows, vec![json!({"id": "2"})]);
}

/// `delimiter` names one field delimiter character, `,` by default, and a tab is admitted;
/// against a non-delimited format it raises `ConnectorFormatKeyRejected` at build, and a
/// quote, a line break or a longer value refuses as invalid configuration.
// spec: connector.source.delimiter@6321ed6c
#[test]
fn delimited_keys_refuse_against_another_format_and_a_malformed_delimiter_refuses() {
    let bucket = MemBucket::with(&[("k.csv", b"a\tb,c\n1\t2,3\n")]);
    let (rows, _) = read(&mut source(json!({"bucket": "b", "key": "k.csv", "delimiter": "\t"}), bucket.clone()), None);
    assert_eq!(rows, vec![json!({"a": "1", "b,c": "2,3"})]);
    let (rows, _) = read(&mut source(json!({"bucket": "b", "key": "k.csv"}), bucket), None);
    assert_eq!(rows, vec![json!({"a\tb": "1\t2", "c": "3"})], "`,` by default");
    for config in [
        json!({"bucket": "b", "key": "k.jsonl", "delimiter": "\t"}),
        json!({"bucket": "b", "key": "k.json", "columns": ["a"]}),
        json!({"bucket": "b", "key": "k.xlsx", "delimiter": ";"}),
    ] {
        assert!(matches!(refusal(config.clone()), ConfigError::Connector(ConnectorError::ConnectorFormatKeyRejected(_))), "{config}");
    }
    for config in [
        json!({"bucket": "b", "key": "k.csv", "delimiter": "\"\""}),
        json!({"bucket": "b", "key": "k.csv", "delimiter": "\""}),
        json!({"bucket": "b", "key": "k.csv", "delimiter": "\n"}),
        json!({"bucket": "b", "key": "k.csv", "delimiter": "\r"}),
        json!({"bucket": "b", "key": "k.csv", "delimiter": ""}),
        json!({"bucket": "b", "key": "k.csv", "columns": []}),
        json!({"bucket": "b", "key": "k.csv", "columns": ["a", "a"]}),
        json!({"bucket": "b", "key": "k.csv", "member": "m.csv", "compression": "gzip"}),
        json!({"bucket": "b", "key": "a.csv.gz", "member": "x.csv"}),
        json!({"bucket": "b", "key": "a.csv", "member": "x.csv"}),
        json!({"bucket": "b", "prefix": "d/", "suffix": ".csv", "member": "x.csv"}),
    ] {
        assert!(matches!(refusal(config.clone()), ConfigError::Run(RunError::Invalid(_))), "{config}");
    }
    for config in [json!({"bucket": "b", "prefix": "d/", "member": "x.csv"}), json!({"bucket": "b", "prefix": "d/", "suffix": ".ZIP", "member": "x.csv"})] {
        assert!(ObjectConfig::parse(&config).is_ok(), "{config}: a zip suffix or no suffix admits `member`");
    }
}

/// Objects in each quarantine prefix below.
const PREFIX_OBJECTS: usize = 1000;

/// One pull from `position`: its rows, its position and whether it claims a complete snapshot.
fn pulled_rows(s: &mut ObjectSource, position: Option<Value>) -> Result<(Vec<Value>, Option<Value>, bool), contextful_core::run::Failure> {
    let pulled: Value = serde_json::from_slice(&s.pull(&request(position), &Never)?).unwrap();
    let rows = pulled["rows"].as_array().cloned().unwrap_or_default();
    Ok((rows, pulled.get("cursor").cloned().filter(|c| !c.is_null()), pulled["snapshot_complete"] == json!(true)))
}

/// The readable objects whose row landed, each readable object `i` holding the one row `{"n": i}`.
fn objects_landed(rows: &[Value], bad: usize) -> usize {
    let mut landed: Vec<usize> = rows
        .iter()
        .filter_map(|r| r.get("n"))
        .filter_map(|n| n.as_u64().map(|i| i as usize).or_else(|| n.as_str().and_then(|s| s.parse().ok())))
        .filter(|i| *i != bad && *i < PREFIX_OBJECTS)
        .collect();
    landed.sort_unstable();
    landed.dedup();
    landed.len()
}

/// A prefix of 1000 objects with one unreadable among them, in three shapes: a JSON Lines line that is no object, a
/// truncated gzip stream, and a truncated zip archive. The figure is the fewest readable objects
/// any shape lands.
#[test]
fn a_prefix_of_1000_objects_with_one_unreadable_lands_the_rest() {
    let bad = PREFIX_OBJECTS / 2;
    let (jsonl, csv_gz, zips) = (MemBucket::default(), MemBucket::default(), MemBucket::default());
    for i in 0..PREFIX_OBJECTS {
        let csv = format!("n\n{i}\n");
        if i == bad {
            jsonl.put(&format!("d/{i:04}.jsonl"), b"{\"n\":\"unreadable\"}\n[3]\n", Condition::None).unwrap();
            let whole = gz(b"n\nunreadable\n");
            csv_gz.put(&format!("g/{i:04}.csv.gz"), &whole[..whole.len() / 2], Condition::None).unwrap();
            let whole = zipped(&[("a.csv", b"n\nunreadable\n")]);
            zips.put(&format!("z/{i:04}.csv.zip"), &whole[..whole.len() / 2], Condition::None).unwrap();
        } else {
            jsonl.put(&format!("d/{i:04}.jsonl"), format!("{{\"n\":{i}}}\n").as_bytes(), Condition::None).unwrap();
            csv_gz.put(&format!("g/{i:04}.csv.gz"), &gz(csv.as_bytes()), Condition::None).unwrap();
            zips.put(&format!("z/{i:04}.csv.zip"), &zipped(&[("a.csv", csv.as_bytes())]), Condition::None).unwrap();
        }
    }
    let cases = [
        (json!({"bucket": "lake", "prefix": "d/", "format": "jsonl"}), Arc::new(jsonl)),
        (json!({"bucket": "lake", "prefix": "g/", "format": "csv"}), Arc::new(csv_gz)),
        (json!({"bucket": "lake", "prefix": "z/", "format": "csv"}), Arc::new(zips)),
    ];
    let mut fewest = PREFIX_OBJECTS;
    for (config, bucket) in &cases {
        let rows = match pulled_rows(&mut source(config.clone(), bucket.clone()), None) {
            Ok((rows, _, _)) => rows,
            Err(f) => {
                assert!(f.message.contains(&format!("{bad:04}")), "{config}: a refusal names the unreadable object: {f}");
                Vec::new()
            }
        };
        assert_eq!(rows.iter().filter(|r| r["n"] == json!("unreadable")).count(), 0, "{config}: no row of the unreadable object lands");
        fewest = fewest.min(objects_landed(&rows, bad));
    }
    contextful_eval::record::emit("ingest-prefix-one-corrupt", fewest as f64, cases.len() as u64, 0);
    assert_eq!(fewest, PREFIX_OBJECTS - 1, "every readable object lands in each shape");
}

/// Under `skip_unchanged`, a read of 1000 objects whose last is unreadable: whether the first read's position holds
/// every readable object, and how many readable objects the second read decodes and downloads again.
#[test]
fn a_read_with_one_unreadable_object_commits_the_position_of_the_rest() {
    let unreadable = format!("d/{:04}.jsonl", PREFIX_OBJECTS - 1);
    let bucket = Arc::new(MemBucket::default());
    for i in 0..PREFIX_OBJECTS - 1 {
        bucket.put(&format!("d/{i:04}.jsonl"), format!("{{\"n\":{i}}}\n").as_bytes(), Condition::None).unwrap();
    }
    bucket.put(&unreadable, b"{\"n\":\"unreadable\"}\n[3]\n", Condition::None).unwrap();
    let mut s = source(json!({"bucket": "lake", "prefix": "d/", "format": "jsonl", "skip_unchanged": true}), bucket.clone());
    let position = match pulled_rows(&mut s, None) {
        Ok((_, at, _)) => at,
        Err(f) => {
            assert!(f.message.contains(&unreadable), "a refusal names the unreadable object: {f}");
            None
        }
    };
    let held = position.as_ref().and_then(|p| p["objects"].as_object()).map_or(0, |o| o.keys().filter(|k| **k != unreadable).count());
    let advanced = u64::from(held == PREFIX_OBJECTS - 1);

    // A refused first read holds no position, so every readable object decodes again.
    bucket.gets.lock().unwrap().clear();
    let redecoded = pulled_rows(&mut s, position).map_or(PREFIX_OBJECTS - 1, |(again, _, _)| objects_landed(&again, PREFIX_OBJECTS - 1));
    let redownloaded = bucket.gets.lock().unwrap().iter().filter(|k| **k != unreadable).count();
    contextful_eval::record::emit("ingest-prefix-position-advances", advanced as f64, 1, 0);
    contextful_eval::record::emit("ingest-prefix-redecodes", redecoded as f64, 1, 0);
    contextful_eval::record::emit("ingest-prefix-redownloads", redownloaded as f64, 1, 0);
    assert_eq!((advanced, redecoded), (1, 0), "the position holds every readable object, and none decodes again");
}

/// A prefix of 100 objects declared JSON Lines, every one holding CSV, refuses the read.
// spec: connector.source.unreadable-all@d5df385c
#[test]
fn a_prefix_where_every_object_is_unreadable_still_refuses() {
    let bucket = Arc::new(MemBucket::default());
    for i in 0..100 {
        bucket.put(&format!("d/{i:03}.jsonl"), format!("a,b\n{i},x\n").as_bytes(), Condition::None).unwrap();
    }
    let refused = match pulled_rows(&mut source(json!({"bucket": "lake", "prefix": "d/", "format": "jsonl"}), bucket), None) {
        Err(f) => u64::from(f.message.starts_with("PipelineUnreadableInput")),
        Ok(_) => 0,
    };
    assert_eq!(refused, 1, "a read whose every object is unreadable refuses");
    contextful_eval::record::emit("ingest-prefix-all-corrupt-refused", refused as f64, 1, 0);
}

/// Two prefixes of 100 objects, one holding a single unreadable object and one holding two. The figure counts the dead
/// letters that name their object's key, reason and digest, while every readable object of both prefixes lands.
#[test]
fn each_unreadable_object_in_a_prefix_lands_one_dead_letter_naming_it() {
    let prefix = |bad: &[usize]| {
        let bucket = Arc::new(MemBucket::default());
        let mut bodies = BTreeMap::new();
        for i in 0..100 {
            let body = if bad.contains(&i) { format!("[{i}]\n").into_bytes() } else { format!("{{\"n\":{i}}}\n").into_bytes() };
            bucket.put(&format!("d/{i:03}.jsonl"), &body, Condition::None).unwrap();
            bodies.insert(format!("d/{i:03}.jsonl"), body);
        }
        let pulled = pull_json(&mut source(json!({"bucket": "lake", "prefix": "d/", "format": "jsonl"}), bucket), None).unwrap();
        assert_eq!(pulled["rows"].as_array().map_or(0, Vec::len), 100 - bad.len(), "every readable object lands");
        let named = pulled["dead_letters"].as_array().cloned().unwrap_or_default().iter().filter(|l| {
            let key = l["key"].as_str().unwrap_or_default();
            bodies.get(key).is_some_and(|b| l["sha256"] == json!(sha256(b))) && l["reason"].as_str().is_some_and(|r| r.contains(&format!("s3://lake/{key}")))
        }).count();
        named
    };
    let named = prefix(&[50]) + prefix(&[25, 75]);
    contextful_eval::record::emit("ingest-prefix-dead-letter-rows", named as f64, 2, 0);
    assert_eq!(named, 3, "one dead letter per unreadable object, naming it");
}

/// A prefix read whose declaration no object can satisfy refuses whole, naming the error, and lands neither rows nor
/// dead letters: an archive lacking the declared `member`, archives of several members and no `member`, and a first
/// record off `columns`. A corrupt object beside them still dead-letters.
// spec: connector.source.configuration-fault@d2488000
#[test]
fn a_prefix_read_refuses_whole_on_a_configuration_fault_and_dead_letters_a_corrupt_object() {
    let one = zipped(&[("a.csv", b"1\t2\t3\n")]);
    let two = zipped(&[("a.csv", b"1\t2\t3\n"), ("b.csv", b"4\t5\t6\n")]);
    type Objects<'a> = &'a [(&'a str, &'a [u8])];
    let cases: [(Objects, Value, &str); 3] = [
        (&[("e/1.zip", &two), ("e/2.zip", &one)], json!({"member": "b.csv", "columns": ["a", "b", "c"]}), "no such member"),
        (&[("e/1.zip", &one), ("e/2.zip", &two)], json!({"columns": ["a", "b", "c"]}), "declares no `member`"),
        (&[("e/1.zip", &one), ("e/2.zip", &zipped(&[("a.csv", b"1\t2\n")]))], json!({"columns": ["a", "b", "c"]}), "line 1"),
    ];
    for (objects, keys, why) in cases {
        let mut config = json!({"bucket": "lake", "prefix": "e/", "format": "csv", "delimiter": "\t"});
        config.as_object_mut().unwrap().extend(keys.as_object().unwrap().clone());
        let f = pull_json(&mut source(config, MemBucket::with(objects)), None).unwrap_err();
        assert_eq!(f.tag, contextful_core::run::FailureTag::Config, "{f}");
        assert!(f.message.starts_with("PipelineUnreadableInput") && f.message.contains("s3://lake/e/") && f.message.contains(why), "{why}: {f}");
    }
    let corrupt = MemBucket::with(&[("e/1.zip", &one), ("e/2.zip", b"not a zip")]);
    let pulled = pull_json(&mut source(json!({"bucket": "lake", "prefix": "e/", "format": "csv", "delimiter": "\t", "columns": ["a", "b", "c"]}), corrupt), None).unwrap();
    assert_eq!(pulled["rows"], json!([{"a": "1", "b": "2", "c": "3"}]));
    assert_eq!(pulled["dead_letters"][0]["key"], json!("e/2.zip"), "{pulled}");
}

/// A prefix read in full twice, one object turned unreadable before the second read: the rows of that object the
/// second read would retire, every one of them when it lands without the object and claims a complete snapshot.
// spec: connector.source.unreadable-incomplete@3cf5e19f
#[test]
fn a_skipped_object_tombstones_none_of_its_earlier_rows() {
    let bucket = MemBucket::with(&[("d/1.jsonl", b"{\"n\":1}\n"), ("d/2.jsonl", b"{\"n\":2}\n{\"n\":3}\n"), ("d/3.jsonl", b"{\"n\":4}\n")]);
    let mut s = source(json!({"bucket": "lake", "prefix": "d/", "format": "jsonl"}), bucket.clone());
    let (first, _, complete) = pulled_rows(&mut s, None).unwrap();
    assert_eq!((first.len(), complete), (4, true));
    bucket.put("d/2.jsonl", b"{\"n\":2}\n[3]\n", Condition::None).unwrap();
    let second = pull_json(&mut s, None).unwrap();
    let retired = match second["snapshot_complete"] == json!(true) {
        true => [json!({"n": 2}), json!({"n": 3})].iter().filter(|r| !second["rows"].as_array().is_some_and(|rows| rows.contains(r))).count(),
        false => 0,
    };
    contextful_eval::record::emit("ingest-skip-prior-rows-tombstoned", retired as f64, 1, 0);
    assert_eq!(retired, 0, "a read leaving an object out never claims the snapshot whole");
    assert_eq!(second["rows"], json!([{"n": 1}, {"n": 4}]), "the readable objects land");
    assert_eq!(second["dead_letters"][0]["key"], json!("d/2.jsonl"), "the unreadable object dead-letters");
}

/// A prefix of 1000 gzip objects whose last expands past the 256 MiB ceiling: the readable objects that land.
#[test]
fn a_prefix_with_one_object_expanding_past_the_ceiling_lands_the_rest() {
    let bad = PREFIX_OBJECTS - 1;
    let mut big = GzEncoder::new(Vec::new(), Compression::fast());
    let chunk = vec![0u8; 1024 * 1024];
    for _ in 0..=EXPANSION_CEILING / chunk.len() as u64 {
        big.write_all(&chunk).unwrap();
    }
    let big = big.finish().unwrap();
    let bucket = Arc::new(MemBucket::default());
    for i in 0..bad {
        bucket.put(&format!("big/{i:04}.jsonl.gz"), &gz(format!("{{\"n\":{i}}}\n").as_bytes()), Condition::None).unwrap();
    }
    bucket.put(&format!("big/{bad:04}.jsonl.gz"), &big, Condition::None).unwrap();
    let rows = match pulled_rows(&mut source(json!({"bucket": "lake", "prefix": "big/", "format": "jsonl"}), bucket), None) {
        Ok((rows, _, _)) => rows,
        Err(f) => {
            let named = f.message.contains(&format!("big/{bad:04}.jsonl.gz")) && f.message.contains(&EXPANSION_CEILING.to_string());
            assert!(named, "a refusal names the oversize object and the ceiling: {f}");
            Vec::new()
        }
    };
    let landed = objects_landed(&rows, bad);
    contextful_eval::record::emit("ingest-prefix-one-oversize", landed as f64, 1, 0);
    assert_eq!(landed, PREFIX_OBJECTS - 1, "every object under the ceiling lands");
}
