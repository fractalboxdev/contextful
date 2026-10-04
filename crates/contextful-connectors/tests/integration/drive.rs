//! `connector.source` over the Google Drive source, against a recorded fake of the Drive API.
#![cfg(feature = "drive")]

use crate::support::{request, resolver, Never, Request, Response, Server};
use contextful_connectors::drive::{BodyStore, Drive, DriveConfig, DriveSource, PageDecoder};
use contextful_connectors::http::ConfigError;
use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::Source;
use contextful_core::run::{Failure, FailureTag, RunError};
use serde_json::{json, Value};
use std::collections::BTreeMap;
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
    incomplete_search_override: Arc<Mutex<Option<Value>>>,
    /// API requests answering `401` before the bearer is honored.
    reject: Arc<Mutex<usize>>,
}

impl Fake {
    fn start() -> Fake {
        let recordings: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(vec!["recording.json"]));
        let incomplete_search_override = Arc::new(Mutex::new(None));
        let reject = Arc::new(Mutex::new(0usize));
        let (r, j) = (recordings.clone(), reject.clone());
        let marker = incomplete_search_override.clone();
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
                            None => {
                                let mut body = x["body"].clone();
                                if body.get("incompleteSearch").is_some() {
                                    if let Some(value) = marker.lock().unwrap().clone() {
                                        body["incompleteSearch"] = value;
                                    }
                                }
                                serde_json::to_vec(&body).unwrap()
                            }
                        };
                        return Response { status: x["status"].as_u64().unwrap() as u16, headers: vec![], body };
                    }
                }
            }
            Response::json(404, "{\"error\":{\"code\":404}}")
        });
        Fake { server, recordings, incomplete_search_override, reject }
    }

    /// Answer from `name` ahead of the recordings loaded before it.
    fn overlay(&self, name: &'static str) {
        self.recordings.lock().unwrap().push(name);
    }

    fn override_incomplete_search(&self, value: Value) {
        *self.incomplete_search_override.lock().unwrap() = Some(value);
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

/// The bodies a drive lands, held in memory by digest, each put counted.
#[derive(Default)]
struct Held {
    blobs: Mutex<BTreeMap<String, Vec<u8>>>,
    puts: Mutex<usize>,
}

impl BodyStore for Held {
    fn put(&self, sha256: &str, bytes: &[u8]) -> Result<(), Failure> {
        *self.puts.lock().unwrap() += 1;
        self.blobs.lock().unwrap().insert(sha256.to_string(), bytes.to_vec());
        Ok(())
    }
}

fn drive(config: Value) -> Arc<Drive> {
    drive_with(config, Arc::new(InProcess))
}

fn drive_with(config: Value, decoder: Arc<dyn PageDecoder>) -> Arc<Drive> {
    drive_into(config, decoder, Arc::new(Held::default()))
}

fn drive_into(config: Value, decoder: Arc<dyn PageDecoder>, bodies: Arc<dyn BodyStore>) -> Arc<Drive> {
    let secrets = vec![("drive-refresh", REFRESH), ("drive-client-id", "client-7.apps.example"), ("drive-client-secret", CLIENT_SECRET)];
    Drive::new(DriveConfig::parse(&config).unwrap(), resolver(secrets), decoder, bodies).unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
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

fn selected_roots(fake: &Fake, ids: &[&str]) -> Value {
    let mut config = fake.config(json!({"folder_ids": ids, "drive_id": "0AExampleDrive"}));
    config.as_object_mut().unwrap().remove("folder_id");
    config
}

/// A full empty walk announces a replacement-capable snapshot; unchanged input does not.
#[test]
fn complete_empty_drive_walk_reports_snapshot_completion_but_unchanged_input_does_not() {
    let empty = Fake::start();
    empty.overlay("empty.json");
    let selected = drive(selected_roots(&empty, &["empty-f"]));
    for table in ["files", "pages"] {
        let (rows, _, bytes) = pull(&mut selected.source(table).unwrap(), None);
        assert!(rows.is_empty());
        let response: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(response["snapshot_complete"], true, "{table}: {response}");
    }

    let populated = Fake::start();
    let source = drive(populated.config(json!({})));
    let (_, cursor, _) = pull(&mut source.source("files").unwrap(), None);
    let (rows, _, bytes) = pull(&mut source.source("files").unwrap(), Some(cursor));
    assert!(rows.is_empty());
    let response: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(response["snapshot_complete"], false, "{response}");
}

/// A bounded root set is validated before any listing.
// spec: connector.source.drive-root-set@3c8b13be
#[test]
fn selected_drive_roots_reject_ambiguous_or_unbounded_configuration() {
    let fake = Fake::start();
    for ids in [Vec::<&str>::new(), vec!["root-f", "root-f"], vec!["root-f"; 17]] {
        let config = selected_roots(&fake, &ids);
        assert!(DriveConfig::parse(&config).unwrap_err().to_string().starts_with("ConnectorDriveRootsInvalid:"), "{config:?}");
    }
    let mut combined = selected_roots(&fake, &["root-f"]);
    combined["folder_id"] = json!("root-f");
    assert!(DriveConfig::parse(&combined).unwrap_err().to_string().starts_with("ConnectorDriveRootsInvalid:"));
    let mut no_drive = selected_roots(&fake, &["root-f"]);
    no_drive.as_object_mut().unwrap().remove("drive_id");
    assert!(DriveConfig::parse(&no_drive).unwrap_err().to_string().starts_with("ConnectorDriveRootsInvalid:"));
    let valid = (0..16).map(|i| format!("root{i}")).collect::<Vec<_>>();
    let mut upper_bound = selected_roots(&fake, &["root-f"]);
    upper_bound["folder_ids"] = json!(valid);
    assert!(DriveConfig::parse(&upper_bound).is_ok());
    assert!(fake.received("/drive/v3/files").is_empty());
}

/// An outside-drive selected root refuses before any selected folder is listed.
// spec: connector.source.drive-root-validation@8a0c59bf
#[test]
fn every_selected_root_is_checked_before_listing() {
    let fake = Fake::start();
    fake.overlay("shared.json");
    for rejected in ["outside-f", "missing-f"] {
        let d = drive(selected_roots(&fake, &["root-f", rejected]));
        let failure = d.source("files").unwrap().pull(&request(None), &Never).unwrap_err();
        assert_eq!(failure.tag, FailureTag::Config, "{failure}");
        assert!(failure.message.contains("ConnectorDriveRootRejected") && failure.message.contains(rejected), "{failure}");
    }
    assert!(fake.received("/drive/v3/files").is_empty());
}

/// Overlapping selected roots land each file once and resolve it to the least selected root id.
// spec: connector.source.drive-root-walk@a69f82be
// spec: connector.source.drive-overlap@ec0c9d7d
#[test]
fn overlapping_selected_roots_deduplicate_files_and_resolve_paths() {
    let fake = Fake::start();
    fake.overlay("shared.json");
    let d = drive(selected_roots(&fake, &["root-f", "fin-f"]));
    let (rows, _, _) = pull(&mut d.source("files").unwrap(), None);
    assert_eq!(rows.len(), 7);
    assert_eq!(rows.iter().filter(|r| r["file_id"] == "sheet-budget").count(), 1);
    assert_eq!((by_id(&rows, "doc-plan")["resolved_root"].clone(), by_id(&rows, "doc-plan")["path"].clone()), (json!("root-f"), json!("Plan")));
    assert_eq!((by_id(&rows, "sheet-budget")["resolved_root"].clone(), by_id(&rows, "sheet-budget")["path"].clone()), (json!("fin-f"), json!("Budget")));
    assert_eq!((by_id(&rows, "pdf-report")["resolved_root"].clone(), by_id(&rows, "pdf-report")["path"].clone()), (json!("fin-f"), json!("Board/report.pdf")));
    let listed = fake.received("/drive/v3/files");
    assert_eq!(listed.len(), 4, "the overlapping Finance folder is listed once");
    assert_eq!(listed.iter().filter(|r| query(r).iter().any(|(k, v)| k == "q" && v.contains("'fin-f'"))).count(), 1);
    for request in listed {
        assert!(query(&request).contains(&("driveId".into(), "0AExampleDrive".into())));
    }
}

/// A pending next-page token at the shared listing cap refuses without committing removals.
// spec: connector.source.drive-list-bound@48660c8e
#[test]
fn a_selected_root_page_token_at_the_listing_cap_refuses_by_name() {
    let fake = Fake::start();
    fake.overlay("shared.json");
    fake.overlay("capped.json");
    let d = drive(selected_roots(&fake, &["root-f"]));
    let prior = json!({"files": {"gone": {"modified": "2031-01-01T00:00:00Z", "path": "gone", "name": "gone", "pages": 1}}});
    let failure = d.source("files").unwrap().pull(&request(Some(prior)), &Never).unwrap_err();
    assert_eq!(failure.tag, FailureTag::Permanent, "{failure}");
    assert!(failure.message.contains("ConnectorDriveListingExceeded") && failure.message.contains("root-f"), "{failure}");
    assert_eq!(fake.received("/drive/v3/files").len(), contextful_connectors::drive::LISTING_CAP);
}

/// A malformed listing cannot prove that previously captured files left the selection.
#[test]
fn malformed_selected_root_listings_refuse_before_removals() {
    for (fixture, malformed_marker) in [
        ("malformed-files.json", false),
        ("malformed-token.json", false),
        ("malformed-child.json", false),
        ("malformed-version.json", false),
        ("malformed-modified.json", false),
        ("incomplete-search.json", false),
        ("incomplete-search.json", true),
    ] {
        let fake = Fake::start();
        fake.overlay(fixture);
        if malformed_marker {
            fake.override_incomplete_search(json!("true"));
        }
        let d = drive(fake.config(json!({})));
        let prior = json!({"files": {"retained": {"modified": "2031-01-01T00:00:00Z", "path": "retained", "name": "retained", "pages": 1}}});
        let failure = d.source("files").unwrap().pull(&request(Some(prior)), &Never).unwrap_err();
        assert_eq!(failure.tag, FailureTag::Permanent, "{fixture}: {failure}");
        assert!(failure.message.contains("listing") && failure.message.contains("root-f"), "{fixture}: {failure}");
        assert!(fake.received("/drive/v3/files").iter().all(|request| query(request).iter().any(|(key, value)| key == "fields" && value.contains("incompleteSearch"))));
    }
}

/// `metadata-only` records exact bytes without retaining a body or decoding pages.
// spec: connector.source.drive-capture-record@b9187adf
// spec: connector.source.drive-metadata-only@6c032057
// spec: connector.source.drive-page-grain@9029df45
// spec: connector.source.drive-bytes@2f79acc3
#[test]
fn metadata_only_records_digests_without_blobs_or_page_content() {
    struct NoDecode;
    impl PageDecoder for NoDecode {
        fn pages(&self, _: &[u8], _: &str) -> Result<Vec<String>, Failure> {
            panic!("metadata-only must not decode")
        }
    }
    let fake = Fake::start();
    let bodies = Arc::new(Held::default());
    let d = drive_into(fake.config(json!({"mode": "metadata-only"})), Arc::new(NoDecode), bodies.clone());
    let (rows, _, _) = pull(&mut d.source("files").unwrap(), None);
    let report = by_id(&rows, "pdf-report");
    let report_bytes = std::fs::read(fixtures().join("report.pdf")).unwrap();
    assert_eq!(report["sha256"], json!(sha256(&report_bytes)));
    assert_eq!(report["bytes"], json!(report_bytes.len()));
    assert_eq!(report["capture_status"], "captured");
    assert_eq!(report["pages"], Value::Null);
    let plan = by_id(&rows, "doc-plan");
    let plan_bytes = std::fs::read(fixtures().join("plan.pdf")).unwrap();
    assert_eq!(plan["sha256"], json!(sha256(&plan_bytes)));
    assert_eq!(plan["export_mime_type"], "application/pdf");
    assert_eq!(by_id(&rows, "short-deck")["capture_status"], "skipped");
    assert!(by_id(&rows, "short-deck")["sha256"].is_null());
    assert_eq!(report["resolved_root"], "root-f");
    assert_eq!(*bodies.puts.lock().unwrap(), 0);
    let (pages, _, _) = pull(&mut d.source("pages").unwrap(), None);
    assert!(pages.is_empty(), "fresh metadata capture has no page rows: {pages:?}");
    assert_eq!(*bodies.puts.lock().unwrap(), 0);
}

/// A capture mode change rereads retained files and removes prior page content.
// spec: connector.source.drive-selection-removals@d263dd24
#[test]
fn metadata_mode_switch_rereads_files_and_tombstones_pages() {
    let fake = Fake::start();
    let full = drive(fake.config(json!({})));
    let (_, file_position, _) = pull(&mut full.source("files").unwrap(), None);
    let (old_pages, page_position, _) = pull(&mut full.source("pages").unwrap(), None);
    assert!(!old_pages.is_empty());
    let bodies = Arc::new(Held::default());
    let metadata = drive_into(fake.config(json!({"mode": "metadata-only"})), Arc::new(InProcess), bodies.clone());
    let (files, new_position, _) = pull(&mut metadata.source("files").unwrap(), Some(file_position));
    assert_eq!(files.len(), 7, "mode change rereads retained files");
    assert_eq!(new_position["mode"], "metadata-only");
    let (pages, _, _) = pull(&mut metadata.source("pages").unwrap(), Some(page_position));
    assert_eq!(pages.len(), old_pages.len());
    assert!(pages.iter().all(|r| r["removed"] == true && r["text"].is_null()));
    assert_eq!(*bodies.puts.lock().unwrap(), 0);
}

/// An unknown mode refuses before any provider request.
// spec: connector.source.drive-mode@f6f7b4a5
#[test]
fn unknown_drive_capture_mode_refuses_before_requests() {
    let fake = Fake::start();
    let error = DriveConfig::parse(&fake.config(json!({"mode": "digest-ish"}))).unwrap_err();
    assert!(error.to_string().contains("ConnectorDriveModeUnknown"), "{error}");
    assert!(fake.received("/drive/v3/files").is_empty());
}

/// A selected-root or drive change binds a new cursor and removes deselected files.
// spec: connector.source.drive-selection-position@00a84230
#[test]
fn selection_change_replays_retained_files_and_tombstones_deselected_files() {
    let fake = Fake::start();
    fake.overlay("shared.json");
    let first = drive(selected_roots(&fake, &["root-f", "fin-f"]));
    let (_, before, _) = pull(&mut first.source("files").unwrap(), None);
    assert_eq!(before["drive_id"], "0AExampleDrive");
    assert_eq!(before["roots"], json!(["fin-f", "root-f"]));
    let second = drive(selected_roots(&fake, &["fin-f"]));
    let (rows, after, _) = pull(&mut second.source("files").unwrap(), Some(before));
    assert_eq!(after["roots"], json!(["fin-f"]));
    assert_eq!(by_id(&rows, "doc-plan")["capture_status"], "removed");
    assert_eq!(by_id(&rows, "doc-plan")["resolved_root"], "root-f");
    assert_eq!(by_id(&rows, "pdf-report")["capture_status"], "captured");
    assert_eq!(by_id(&rows, "pdf-report")["resolved_root"], "fin-f");
}

/// A changed exported version in metadata mode publishes the digest of its new exact bytes.
#[test]
fn metadata_only_changed_version_updates_digest_and_removed_file() {
    let fake = Fake::start();
    let config = fake.config(json!({"mode": "metadata-only"}));
    let (first, before, _) = pull(&mut drive(config.clone()).source("files").unwrap(), None);
    fake.overlay("second.json");
    let (rows, _, _) = pull(&mut drive(config).source("files").unwrap(), Some(before));
    assert_eq!(rows.len(), 2);
    let plan = by_id(&rows, "doc-plan");
    let bytes = std::fs::read(fixtures().join("plan-v2.pdf")).unwrap();
    assert_ne!(plan["sha256"], by_id(&first, "doc-plan")["sha256"]);
    assert_eq!(plan["sha256"], json!(sha256(&bytes)));
    assert_eq!(plan["version"], 6);
    let removed = by_id(&rows, "pdf-report");
    assert_eq!(removed["capture_status"], "removed");
    assert!(removed["sha256"].is_null());
}

/// A download crossing a version change refuses the whole metadata capture.
// spec: connector.source.drive-version-consistency@058faf68
#[test]
fn metadata_only_refuses_a_version_moving_during_capture() {
    let fake = Fake::start();
    fake.overlay("version-race.json");
    let bodies = Arc::new(Held::default());
    let d = drive_into(fake.config(json!({"mode": "metadata-only"})), Arc::new(InProcess), bodies.clone());
    let failure = d.source("files").unwrap().pull(&request(None), &Never).unwrap_err();
    assert!(failure.message.contains("ConnectorDriveVersionMoved") && failure.message.contains("doc-plan"), "{failure}");
    assert_eq!(*bodies.puts.lock().unwrap(), 0);
}

/// A version or resolved-root change re-reads a file despite an unchanged modification time and path.
// spec: connector.source.drive-root-reassignment@ab410702
#[test]
fn held_version_and_resolved_root_trigger_reread() {
    let fake = Fake::start();
    fake.overlay("shared.json");
    let config = selected_roots(&fake, &["root-f"]);
    let (_, position, _) = pull(&mut drive(config.clone()).source("files").unwrap(), None);
    let mut prior_version = position.clone();
    prior_version["files"]["doc-plan"]["version"] = json!(0);
    let (rows, _, _) = pull(&mut drive(config.clone()).source("files").unwrap(), Some(prior_version));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["file_id"], "doc-plan");
    let mut prior_root = position;
    prior_root["files"]["doc-plan"]["resolved_root"] = json!("fin-f");
    let (rows, _, _) = pull(&mut drive(config).source("files").unwrap(), Some(prior_root));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["resolved_root"], "root-f");
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
    // A root inside the declared drive walks the whole tree, each listing scoped to that drive.
    shared.overlay("shared.json");
    let (rows, _, _) = pull(&mut d.source("files").unwrap(), None);
    assert_eq!(rows.len(), 7);
    let listings = shared.received("/drive/v3/files");
    assert_eq!(listings.len(), 4, "the root's two pages, then each subfolder once");
    for r in &listings {
        let q = query(r);
        for pair in [("corpora", "drive"), ("driveId", "0AExampleDrive"), ("supportsAllDrives", "true"), ("includeItemsFromAllDrives", "true")] {
            assert!(q.contains(&(pair.0.to_string(), pair.1.to_string())), "{pair:?} in {q:?}");
        }
    }
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

/// In the default `bytes-and-pages` mode, each PDF page lands one `pages` row under
/// {{connector.source.document-grain}}, decoded behind {{run.land.parse-boundary}};
/// bytes land in no column.
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

/// In the default `bytes-and-pages` mode, every whole exported or downloaded body lands
/// as {{store.lay-out.landed-blob}}, named by its file row's `sha256`.
#[test]
fn every_body_read_lands_once_as_the_blob_its_row_names() {
    let fake = Fake::start();
    let held = Arc::new(Held::default());
    let d = drive_into(fake.config(json!({})), Arc::new(Refusing), held.clone());
    let (files, _, _) = pull(&mut d.source("files").unwrap(), None);
    pull(&mut d.source("pages").unwrap(), None);
    let blobs = held.blobs.lock().unwrap().clone();
    for (id, fixture) in [("doc-plan", "plan.pdf"), ("sheet-budget", "budget.pdf"), ("slides-deck", "deck.pdf"), ("pdf-report", "report.pdf")] {
        let bytes = std::fs::read(fixtures().join(fixture)).unwrap();
        let named = by_id(&files, id)["sha256"].as_str().unwrap_or_else(|| panic!("{id} names no blob")).to_string();
        assert_eq!(named, sha256(&bytes), "{id}");
        assert_eq!(blobs.get(&named), Some(&bytes), "{id}: the row names a stored blob holding the bytes read");
    }
    assert_eq!(blobs.len(), 4, "the video over the cap, the form and the shortcut land no blob");
    assert_eq!(*held.puts.lock().unwrap(), 4, "both tables of one fire share one put per body");
}

/// A Doc, Sheet or Slides deck whose export Drive answers `403 exportSizeLimitExceeded` lands its file row with
/// a `skipped` reason naming Drive's export limit and no pages, and the read continues.
// spec: connector.source.drive-export-limit@5926bd84
#[test]
fn an_export_over_drives_limit_is_skipped_by_name_and_the_read_succeeds() {
    let fake = Fake::start();
    fake.overlay("export-limit.json");
    let d = drive(fake.config(json!({})));
    let (files, at, _) = pull(&mut d.source("files").unwrap(), None);
    assert_eq!(files.len(), 7, "every file lands its row");
    let sheet = by_id(&files, "sheet-budget");
    assert!(sheet["skipped"].as_str().unwrap().contains("exportSizeLimitExceeded"), "{sheet:?}");
    assert_eq!((sheet["sha256"].clone(), sheet["pages"].clone(), sheet["export_mime_type"].clone()), (Value::Null, Value::Null, Value::Null));
    let (pages, pat, _) = pull(&mut d.source("pages").unwrap(), None);
    assert!(pages_of(&pages, "sheet-budget").is_empty());
    assert_eq!(pages_of(&pages, "doc-plan"), [(1, json!("Quarterly plan")), (2, json!("Hiring targets"))]);
    assert_eq!(serde_json::from_slice::<Value>(&d.source("files").unwrap().pull(&request(None), &Never).unwrap()).unwrap()["skipped"], json!(4));
    // The position advances past the refused export: an unchanged tree lands nothing next.
    let again = drive(fake.config(json!({})));
    assert!(pull(&mut again.source("files").unwrap(), Some(at)).0.is_empty());
    assert!(pull(&mut again.source("pages").unwrap(), Some(pat)).0.is_empty());
    // A `403` for another reason still fails the read.
    let denied = Fake::start();
    denied.overlay("export-denied.json");
    let f = drive(denied.config(json!({}))).source("files").unwrap().pull(&request(None), &Never).unwrap_err();
    assert!(f.message.contains("sheet-budget/export") && f.message.contains("403"), "{f}");
}
