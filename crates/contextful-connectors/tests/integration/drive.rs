//! `connector.source` over the Google Drive source, against a recorded fake of the Drive API.
#![cfg(feature = "drive")]

use crate::support::{request, resolver, Never, Request, Response, Server};
use contextful_connectors::drive::{Drive, DriveConfig, DriveSource, PageDecoder};
use contextful_connectors::http::ConfigError;
use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::Source;
use contextful_core::run::{Failure, FailureTag, RunError};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const ACCESS: &str = "ya29.fixture-access-7Qx";
const REFRESH: &str = "1//refresh-fixture-Zq8";
const CLIENT_SECRET: &str = "GOCSPX-fixture-secret";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/drive")
}

/// Percent-decode a query component, `+` as a space.
fn unescape(s: &str) -> String {
    let (bytes, mut out, mut i) = (s.as_bytes(), Vec::new(), 0);
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap_or(b'?'));
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn query(r: &Request) -> Vec<(String, String)> {
    let q = r.target.split_once('?').map(|(_, q)| q).unwrap_or_default();
    q.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (unescape(k), unescape(v))).collect()
}

/// A recorded fake of the Drive API: each exchange answers a request matching its method,
/// path and listed query parameters, a later recording's exchange before an earlier one's.
/// An API request without the minted bearer answers `401`.
struct Fake {
    server: Server,
    recordings: Arc<Mutex<Vec<&'static str>>>,
    /// API requests answering `401` before the bearer is honored.
    reject: Arc<Mutex<usize>>,
}

impl Fake {
    fn start() -> Fake {
        let recordings: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(vec!["recording.json"]));
        let reject = Arc::new(Mutex::new(0usize));
        let (r, j) = (recordings.clone(), reject.clone());
        let server = Server::start(move |req| {
            if req.path() != "/token" {
                let mut left = j.lock().unwrap();
                if req.header("authorization") != Some(&format!("Bearer {ACCESS}")) || *left > 0 {
                    *left = left.saturating_sub(1);
                    return Response::json(401, "{\"error\":{\"code\":401,\"status\":\"UNAUTHENTICATED\"}}");
                }
            }
            let asked = query(req);
            for name in r.lock().unwrap().iter().rev() {
                let recording: Value = serde_json::from_slice(&std::fs::read(fixtures().join(name)).unwrap()).unwrap();
                for x in recording["exchanges"].as_array().unwrap() {
                    let wanted = x["query"].as_object().cloned().unwrap_or_default();
                    let matches = x["method"] == req.method.as_str()
                        && x["path"] == req.path()
                        && wanted.iter().all(|(k, v)| asked.iter().any(|(a, b)| a == k && Some(b.as_str()) == v.as_str()))
                        && (wanted.contains_key("pageToken") || !asked.iter().any(|(a, _)| a == "pageToken"));
                    if matches {
                        let body = match x.get("body_file") {
                            Some(f) => std::fs::read(fixtures().join(f.as_str().unwrap())).unwrap(),
                            None => serde_json::to_vec(&x["body"]).unwrap(),
                        };
                        return Response { status: x["status"].as_u64().unwrap() as u16, headers: vec![], body };
                    }
                }
            }
            Response::json(404, "{\"error\":{\"code\":404}}")
        });
        Fake { server, recordings, reject }
    }

    /// Answer from `name` ahead of the recordings loaded before it.
    fn overlay(&self, name: &'static str) {
        self.recordings.lock().unwrap().push(name);
    }

    fn config(&self, extra: Value) -> Value {
        let mut c = json!({
            "folder_id": "root-f",
            "api_base": self.server.url(""),
            "token_url": self.server.url("/token"),
            "oauth": {"refresh_token": "${secret://drive-refresh}", "client_id": "${secret://drive-client-id}", "client_secret": "${secret://drive-client-secret}"},
        });
        c.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
        c
    }

    /// Requests whose path is `path`.
    fn received(&self, path: &str) -> Vec<Request> {
        self.server.received(path)
    }
}

/// The PDF decoder run in this process: the suite pins decoding to text, not the boundary.
struct InProcess;

impl PageDecoder for InProcess {
    fn pages(&self, body: &[u8], input: &str) -> Result<Vec<String>, Failure> {
        contextful_connectors::decode::pdf::pages(body, input)
    }
}

/// A decoder refusing the board report as unreadable and crashing on the deck, decoding the rest in process.
struct Refusing;

impl PageDecoder for Refusing {
    fn pages(&self, body: &[u8], input: &str) -> Result<Vec<String>, Failure> {
        match input {
            "Finance/Board/report.pdf" => Err(Failure::deterministic(FailureTag::Permanent, RunError::PipelineUnreadableInput(format!("`{input}` holds no text layer")).to_string())),
            "Finance/Board/Deck" => Err(Failure::deterministic(FailureTag::Permanent, RunError::PipelineParseCrashed(format!("decoding `{input}`: the decode process died on signal 11")).to_string())),
            _ => InProcess.pages(body, input),
        }
    }
}

fn drive(config: Value) -> Arc<Drive> {
    drive_with(config, Arc::new(InProcess))
}

fn drive_with(config: Value, decoder: Arc<dyn PageDecoder>) -> Arc<Drive> {
    let secrets = vec![("drive-refresh", REFRESH), ("drive-client-id", "client-7.apps.example"), ("drive-client-secret", CLIENT_SECRET)];
    Drive::new(DriveConfig::parse(&config).unwrap(), resolver(secrets), decoder).unwrap()
}

/// Pull `source` from `position`, answering its rows and the position after them.
fn pull(source: &mut DriveSource, position: Option<Value>) -> (Vec<serde_json::Map<String, Value>>, Value, Vec<u8>) {
    let bytes = source.pull(&request(position), &Never).unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let rows = v["rows"].as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect();
    (rows, v["cursor"].clone(), bytes)
}

fn by_id<'a>(rows: &'a [serde_json::Map<String, Value>], id: &str) -> &'a serde_json::Map<String, Value> {
    rows.iter().find(|r| r["file_id"] == id).unwrap_or_else(|| panic!("no row for `{id}`"))
}

fn pages_of(rows: &[serde_json::Map<String, Value>], id: &str) -> Vec<(u64, Value)> {
    rows.iter().filter(|r| r["file_id"] == id).map(|r| (r["page"].as_u64().unwrap(), r["text"].clone())).collect()
}

/// The drive source walks the tree under `folder_id` breadth-first through `files.list`, paging each folder,
/// within `drive_id` when declared. Each folder is listed once, its children sorted by name and id; no shortcut is
/// followed.
// spec: connector.source.drive-walk@60209b54
#[test]
fn a_three_level_tree_lands_one_row_per_file_with_its_path_from_the_root() {
    let fake = Fake::start();
    let d = drive(fake.config(json!({})));
    let (rows, _, _) = pull(&mut d.source("files").unwrap(), None);
    let paths: Vec<(&str, &str)> = rows.iter().map(|r| (r["file_id"].as_str().unwrap(), r["path"].as_str().unwrap())).collect();
    assert_eq!(
        paths,
        [
            ("short-deck", "Deck shortcut"),
            ("slides-deck", "Finance/Board/Deck"),
            ("bin-video", "Finance/Board/all-hands.mp4"),
            ("pdf-report", "Finance/Board/report.pdf"),
            ("sheet-budget", "Finance/Budget"),
            ("form-intake", "Intake"),
            ("doc-plan", "Plan"),
        ]
    );
    // One listing per folder page: the root's two pages, then each subfolder once.
    let listed: Vec<String> = fake.received("/drive/v3/files").iter().map(|r| query(r).into_iter().find(|(k, _)| k == "q").unwrap().1).collect();
    assert_eq!(listed, ["'root-f' in parents and trashed = false", "'root-f' in parents and trashed = false", "'fin-f' in parents and trashed = false", "'board-f' in parents and trashed = false"]);
    let shortcut = by_id(&rows, "short-deck");
    assert!(shortcut["skipped"].as_str().unwrap().contains("shortcut"), "{shortcut:?}");
    assert!(fake.received("/drive/v3/files/slides-deck/export").len() == 1, "the shortcut's target is read once, where it sits in the tree");
    // `drive_id` scopes each listing to that shared drive.
    let shared = Fake::start();
    let d = drive(shared.config(json!({"drive_id": "0AExampleDrive"})));
    let f = d.source("files").unwrap().pull(&request(None), &Never).unwrap_err();
    assert_eq!(f.tag, FailureTag::Config, "the root sits in no declared drive: {f}");
    assert!(f.message.contains("not in drive `0AExampleDrive`"), "{f}");
    assert!(shared.received("/drive/v3/files").is_empty(), "the root check comes ahead of any listing");
}

/// The drive source serves `files`, one row per file keyed by `file_id` with its version, name, path from the
/// root, MIME type, `modifiedTime`, Drive's `md5Checksum` and the SHA-256 of the bytes read, and `pages`.
// spec: connector.source.drive-tables@b99cb589
#[test]
fn a_file_row_carries_its_metadata_and_names_its_bytes_by_digest() {
    let fake = Fake::start();
    let d = drive(fake.config(json!({})));
    let (rows, _, _) = pull(&mut d.source("files").unwrap(), None);
    let report = by_id(&rows, "pdf-report");
    let bytes = std::fs::read(fixtures().join("report.pdf")).unwrap();
    use sha2::Digest;
    let digest: String = sha2::Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(report["sha256"], json!(digest));
    assert_eq!(report["md5_checksum"], json!("22359917bddc62856915e06ca6b50811"));
    assert_eq!((report["version"].clone(), report["name"].clone(), report["mime_type"].clone()), (json!(1), json!("report.pdf"), json!("application/pdf")));
    assert_eq!((report["modified_time"].clone(), report["bytes"].clone(), report["pages"].clone()), (json!("2031-01-06T14:00:00.000Z"), json!(bytes.len()), json!(1)));
    assert_eq!(report["removed"], json!(false));
    assert!(report.values().all(|v| v.as_str().is_none_or(|s| !s.starts_with("%PDF"))), "bytes land in no column");
    let plan = by_id(&rows, "doc-plan");
    assert_eq!((plan["export_mime_type"].clone(), plan["md5_checksum"].clone()), (json!("application/pdf"), Value::Null), "Drive supplies no checksum for a native file");
}

/// A drive table other than `files` or `pages` refuses as {{connector.source.table-unmatched}}, ahead of any
/// request.
// spec: connector.source.drive-table-unmatched@f63a2d97
#[test]
fn a_table_the_source_does_not_serve_refuses_before_any_request() {
    let fake = Fake::start();
    let d = drive(fake.config(json!({})));
    match d.source("revisions") {
        Err(ConnectorError::ConnectorTableUnmatched(m)) => assert!(m.contains("files and pages") && m.contains("revisions"), "{m}"),
        Ok(_) => panic!("`revisions` is served"),
        Err(other) => panic!("{other}"),
    }
    assert!(fake.server.requests.lock().unwrap().is_empty());
}

/// A Google Doc, Sheet or Slides deck lands as its `files.export` PDF and any other file as its `alt=media`
/// bytes; another Google-native type lands a `skipped` reason and no bytes.
// spec: connector.source.drive-export@74bd1c5c
#[test]
fn docs_sheets_and_slides_export_as_pdf_and_other_files_download() {
    let fake = Fake::start();
    let d = drive(fake.config(json!({})));
    let (rows, _, _) = pull(&mut d.source("files").unwrap(), None);
    for id in ["doc-plan", "sheet-budget", "slides-deck"] {
        let exported = fake.received(&format!("/drive/v3/files/{id}/export"));
        assert_eq!(exported.len(), 1, "{id}");
        assert_eq!(query(&exported[0]), [("mimeType".to_string(), "application/pdf".to_string())], "{id}");
    }
    let media = fake.received("/drive/v3/files/pdf-report");
    assert_eq!(media.len(), 1);
    assert!(query(&media[0]).contains(&("alt".into(), "media".into())));
    let form = by_id(&rows, "form-intake");
    assert!(form["skipped"].as_str().unwrap().contains("application/vnd.google-apps.form"), "{form:?}");
    assert_eq!((form["sha256"].clone(), form["bytes"].clone()), (Value::Null, Value::Null));
    assert!(fake.received("/drive/v3/files/form-intake/export").is_empty());
}

/// A PDF body lands one `pages` row per page under {{connector.source.document-grain}}, decoded behind
/// {{run.land.parse-boundary}}; bytes land in no column.
// spec: connector.source.drive-page-grain@35ff9f32
#[test]
fn a_doc_a_sheet_and_a_deck_land_as_their_pdf_pages() {
    let fake = Fake::start();
    let d = drive(fake.config(json!({})));
    let (rows, _, _) = pull(&mut d.source("pages").unwrap(), None);
    assert_eq!(pages_of(&rows, "doc-plan"), [(1, json!("Quarterly plan")), (2, json!("Hiring targets"))]);
    assert_eq!(pages_of(&rows, "sheet-budget"), [(1, json!("Budget 2031"))]);
    assert_eq!(pages_of(&rows, "slides-deck"), [(1, json!("Roadmap")), (2, json!("Launch")), (3, json!("Risks"))]);
    assert_eq!(pages_of(&rows, "pdf-report"), [(1, json!("Board report"))]);
    assert!(pages_of(&rows, "bin-video").is_empty() && pages_of(&rows, "form-intake").is_empty());
    let deck = rows.iter().find(|r| r["file_id"] == "slides-deck").unwrap();
    assert_eq!((deck["path"].clone(), deck["version"].clone(), deck["removed"].clone()), (json!("Finance/Board/Deck"), json!(4), json!(false)));
    // Both tables of one fire share one walk and one download per file.
    let (files, _, _) = pull(&mut d.source("files").unwrap(), None);
    assert_eq!(files.len(), 7);
    assert_eq!(fake.received("/drive/v3/files/doc-plan/export").len(), 1);
    assert_eq!(fake.received("/drive/v3/files").len(), 4);
}

/// A file over `max_file_bytes`, 64 MiB by default, lands its file row with a `skipped` reason naming the cap
/// and no pages. No byte past the cap is read, and the read continues.
// spec: connector.source.drive-file-cap@358a2c15
#[test]
fn a_file_over_the_cap_is_skipped_by_name_and_the_read_succeeds() {
    let fake = Fake::start();
    let d = drive(fake.config(json!({})));
    let (rows, _, _) = pull(&mut d.source("files").unwrap(), None);
    let video = by_id(&rows, "bin-video");
    assert_eq!(video["path"], json!("Finance/Board/all-hands.mp4"));
    assert!(video["skipped"].as_str().unwrap().contains(&format!("over max_file_bytes of {}", 64 * 1024 * 1024)), "{video:?}");
    assert!(fake.received("/drive/v3/files/bin-video").is_empty(), "a reported size over the cap downloads nothing");
    assert_eq!(rows.iter().filter(|r| !r["skipped"].is_null()).count(), 3, "the video, the form and the shortcut");
    // An export reports no size: its body stops at the cap.
    let small = Fake::start();
    let d = drive(small.config(json!({"max_file_bytes": 700})));
    let (rows, _, _) = pull(&mut d.source("pages").unwrap(), None);
    assert!(pages_of(&rows, "doc-plan").is_empty() && pages_of(&rows, "slides-deck").is_empty(), "the 883- and 1128-byte exports pass 700 bytes");
    assert_eq!(pages_of(&rows, "sheet-budget"), [(1, json!("Budget 2031"))]);
    let (files, _, _) = pull(&mut d.source("files").unwrap(), None);
    assert!(by_id(&files, "doc-plan")["skipped"].as_str().unwrap().contains("over max_file_bytes"));
}

/// The drive position holds each file's `modifiedTime`, path and page count. A read re-lands a file whose time or
/// path changed, and lands a tombstone for a file gone from the tree and for each page past its new count.
// spec: connector.source.drive-incremental@cd599d0d
#[test]
fn a_second_read_relands_the_changed_file_alone_and_tombstones_what_left() {
    let fake = Fake::start();
    let (files1, at1, _) = pull(&mut drive(fake.config(json!({}))).source("files").unwrap(), None);
    let (pages1, pat1, _) = pull(&mut drive(fake.config(json!({}))).source("pages").unwrap(), None);
    assert_eq!((files1.len(), pages1.len()), (7, 7));
    fake.overlay("second.json");
    let d = drive(fake.config(json!({})));
    let (files2, _, _) = pull(&mut d.source("files").unwrap(), Some(at1));
    let changed: Vec<(&str, bool)> = files2.iter().map(|r| (r["file_id"].as_str().unwrap(), r["removed"].as_bool().unwrap())).collect();
    assert_eq!(changed, [("doc-plan", false), ("pdf-report", true)]);
    assert_eq!(by_id(&files2, "doc-plan")["version"], json!(6));
    let gone = by_id(&files2, "pdf-report");
    assert_eq!((gone["path"].clone(), gone["sha256"].clone()), (json!("Finance/Board/report.pdf"), Value::Null));
    let (pages2, at2, _) = pull(&mut d.source("pages").unwrap(), Some(pat1));
    assert_eq!(pages_of(&pages2, "doc-plan"), [(1, json!("Quarterly plan revised")), (2, Value::Null)]);
    assert_eq!(pages_of(&pages2, "pdf-report"), [(1, Value::Null)]);
    assert_eq!(pages2.len(), 3, "unchanged files land nothing");
    assert!(pages2.iter().filter(|r| r["text"].is_null()).all(|r| r["removed"] == json!(true)));
    assert_eq!(fake.received("/drive/v3/files/doc-plan/export").len(), 3, "two first reads and one re-land");
    assert_eq!(fake.received("/drive/v3/files/sheet-budget/export").len(), 2, "the unchanged sheet exports on first reads alone");
    // A third read over an unchanged tree lands nothing.
    let (pages3, _, _) = pull(&mut drive(fake.config(json!({}))).source("pages").unwrap(), Some(at2));
    assert!(pages3.is_empty(), "{pages3:?}");
}

/// The drive source mints its access token from a refresh token and client credentials bound as `secret://`
/// references, once per fire and again on a `401`, holding it in memory and writing it to no store, journal or
/// record.
// spec: connector.source.drive-oauth@df353f08
#[test]
fn the_access_token_is_minted_from_references_and_lands_in_no_row_or_position() {
    let fake = Fake::start();
    let d = drive(fake.config(json!({})));
    let (_, _, files) = pull(&mut d.source("files").unwrap(), None);
    let (_, _, pages) = pull(&mut d.source("pages").unwrap(), None);
    let minted = fake.received("/token");
    assert_eq!(minted.len(), 1, "one mint serves the fire");
    let form: Vec<(String, String)> = String::from_utf8(minted[0].body.clone()).unwrap().split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (unescape(k), unescape(v))).collect();
    for pair in [("grant_type", "refresh_token"), ("refresh_token", REFRESH), ("client_id", "client-7.apps.example"), ("client_secret", CLIENT_SECRET)] {
        assert!(form.contains(&(pair.0.to_string(), pair.1.to_string())), "{pair:?} in {form:?}");
    }
    assert_eq!(minted[0].method, "POST");
    assert!(fake.received("/drive/v3/files").iter().all(|r| r.header("authorization") == Some(&format!("Bearer {ACCESS}"))));
    assert_eq!(d.sensitive_headers(), ["Authorization"]);
    for landed in [&files, &pages] {
        let text = String::from_utf8_lossy(landed);
        for secret in [ACCESS, REFRESH, CLIENT_SECRET] {
            assert!(!text.contains(secret), "`{secret}` in a pull");
        }
    }
    // A `401` mints once more and repeats the request.
    let again = Fake::start();
    *again.reject.lock().unwrap() = 1;
    let (rows, _, _) = pull(&mut drive(again.config(json!({}))).source("files").unwrap(), None);
    assert_eq!((rows.len(), again.received("/token").len()), (7, 2));
    // A second `401` is an expired credential.
    let refused = Fake::start();
    *refused.reject.lock().unwrap() = 2;
    let f = drive(refused.config(json!({}))).source("files").unwrap().pull(&request(None), &Never).unwrap_err();
    assert_eq!(f.tag, FailureTag::AuthExpired, "{f}");
    assert!(!f.message.contains(ACCESS) && !f.message.contains(REFRESH), "{f}");
}

/// `oauth.refresh_token`, `oauth.client_id` and `oauth.client_secret` each hold one `${secret://<name>}`
/// reference and nothing else, checked before any request.
// spec: connector.source.drive-oauth-shape@a500c256
#[test]
fn each_oauth_value_is_one_reference() {
    let fake = Fake::start();
    let with = |oauth: Value| DriveConfig::parse(&fake.config(json!({ "oauth": oauth })));
    let refs = json!({"refresh_token": "${secret://r}", "client_id": "${secret://i}", "client_secret": "${secret://s}"});
    assert!(with(refs.clone()).is_ok());
    for (key, value) in [("client_id", "client-7.apps.example"), ("refresh_token", "Bearer ${secret://r}"), ("client_secret", "${secret://s}${secret://t}")] {
        let mut o = refs.clone();
        o[key] = json!(value);
        let e = with(o).unwrap_err();
        assert!(e.to_string().contains(&format!("oauth.{key}")), "{key}: {e}");
    }
    let mut o = refs.clone();
    o["client_secret"] = json!("ghp_0123456789abcdefghijABCDEFGHIJ012345");
    assert!(matches!(with(o), Err(ConfigError::Connector(ConnectorError::SecretMaterialInDeclaration(_)))), "material refuses as material");
    let mut o = refs.clone();
    o["scope"] = json!("drive");
    assert!(matches!(with(o), Err(ConfigError::Run(RunError::PipelineUnknownConfigKey(_)))));
    assert!(matches!(DriveConfig::parse(&json!({"folder_id": "root-f", "oauth": refs, "folders": []})), Err(ConfigError::Run(RunError::PipelineUnknownConfigKey(_)))));
    assert!(fake.server.requests.lock().unwrap().is_empty());
}

/// The drive API and token endpoints are `www.googleapis.com` and `oauth2.googleapis.com` over TLS on the default
/// port; another host, port or scheme refuses as {{connector.source.provider-origin}}, loopback excepted.
// spec: connector.source.drive-origin@e3de3af5
#[test]
fn a_google_credential_goes_to_google_hosts_alone() {
    let refs = json!({"refresh_token": "${secret://r}", "client_id": "${secret://i}", "client_secret": "${secret://s}"});
    let base = |extra: Value| {
        let mut c = json!({"folder_id": "root-f", "oauth": refs.clone()});
        c.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap());
        DriveConfig::parse(&c)
    };
    let d = base(json!({})).unwrap();
    assert_eq!((d.api_base.as_str(), d.token_url.as_str()), ("https://www.googleapis.com/", "https://oauth2.googleapis.com/token"));
    for (key, url) in [("api_base", "https://drive.example.com"), ("api_base", "http://www.googleapis.com"), ("token_url", "https://oauth2.example.com/token"), ("token_url", "https://oauth2.googleapis.com:8443/token")] {
        match base(json!({ key: url })) {
            Err(ConfigError::Connector(ConnectorError::ConnectorProviderOriginRejected(m))) => assert!(m.contains(key), "{m}"),
            other => panic!("{key} = {url}: {other:?}"),
        }
    }
    assert!(base(json!({"api_base": "http://127.0.0.1:9", "token_url": "http://[::1]:9/token"})).is_ok(), "loopback is a recorded fake");
}

/// A `folder_id` naming no folder, or one outside a declared `drive_id`, fails the read as a configuration fault
/// before any listing.
// spec: connector.source.drive-root@a4b29f8d
#[test]
fn a_root_that_is_no_folder_fails_before_any_listing() {
    let fake = Fake::start();
    for id in ["doc-plan", "missing-f"] {
        let f = drive(fake.config(json!({"folder_id": id}))).source("files").unwrap().pull(&request(None), &Never).unwrap_err();
        assert_eq!(f.tag, FailureTag::Config, "{id}: {f}");
        assert!(f.deterministic && f.message.contains(id), "{id}: {f}");
    }
    assert!(fake.received("/drive/v3/files").is_empty());
}

/// A PDF body failing to decode behind {{run.land.parse-boundary}} lands its file row with a `skipped` reason naming
/// the failure and no pages, and the read continues.
// spec: connector.source.drive-unreadable@eb615de8
#[test]
fn an_unreadable_or_crashing_pdf_is_skipped_and_every_other_file_lands() {
    let fake = Fake::start();
    let d = drive_with(fake.config(json!({})), Arc::new(Refusing));
    let (files, at, _) = pull(&mut d.source("files").unwrap(), None);
    assert_eq!(files.len(), 7, "every file lands its row");
    let report = by_id(&files, "pdf-report");
    assert!(report["skipped"].as_str().unwrap().contains("PipelineUnreadableInput"), "{report:?}");
    assert_eq!((report["pages"].clone(), report["removed"].clone()), (Value::Null, json!(false)));
    assert!(report["sha256"].is_string(), "the bytes were read whole, so the digest names them");
    let deck = by_id(&files, "slides-deck");
    assert!(deck["skipped"].as_str().unwrap().contains("PipelineParseCrashed"), "{deck:?}");
    assert_eq!(deck["export_mime_type"], json!("application/pdf"), "the export was read");
    assert!(by_id(&files, "doc-plan")["skipped"].is_null());
    let (pages, pat, _) = pull(&mut d.source("pages").unwrap(), None);
    assert!(pages_of(&pages, "pdf-report").is_empty() && pages_of(&pages, "slides-deck").is_empty());
    assert_eq!(pages_of(&pages, "doc-plan"), [(1, json!("Quarterly plan")), (2, json!("Hiring targets"))]);
    assert_eq!(pages_of(&pages, "sheet-budget"), [(1, json!("Budget 2031"))]);
    assert_eq!(fake.received("/drive/v3/files/pdf-report").len(), 1, "both tables share the one refused read");
    // The position advances past the refused files: an unchanged tree lands nothing on the next read.
    assert_eq!(at["files"]["pdf-report"]["pages"], json!(0));
    let again = drive_with(fake.config(json!({})), Arc::new(Refusing));
    assert!(pull(&mut again.source("files").unwrap(), Some(at)).0.is_empty());
    assert!(pull(&mut again.source("pages").unwrap(), Some(pat)).0.is_empty());
}

/// Each drive pull reports the files it lands with a `skipped` reason as its {{run.record.skipped-count}}, so a
/// fire's `files` run and `pages` run each carry the tally.
// spec: connector.source.drive-skip-count@3994398e
#[test]
fn each_pull_counts_the_files_it_skipped() {
    let skipped = |bytes: &[u8]| serde_json::from_slice::<Value>(bytes).unwrap().get("skipped").and_then(Value::as_u64).unwrap_or(0);
    let fake = Fake::start();
    let d = drive(fake.config(json!({})));
    let (_, at, files) = pull(&mut d.source("files").unwrap(), None);
    let (_, _, pages) = pull(&mut d.source("pages").unwrap(), None);
    assert_eq!((skipped(&files), skipped(&pages)), (3, 3), "the video over the cap, the form and the shortcut");
    // A refused decode counts beside them.
    let refused = drive_with(fake.config(json!({})), Arc::new(Refusing));
    let (_, _, files) = pull(&mut refused.source("files").unwrap(), None);
    assert_eq!(skipped(&files), 5);
    // A read landing no skipped file counts none.
    fake.overlay("second.json");
    let (_, _, files) = pull(&mut drive(fake.config(json!({}))).source("files").unwrap(), Some(at));
    assert_eq!(skipped(&files), 0);
}
