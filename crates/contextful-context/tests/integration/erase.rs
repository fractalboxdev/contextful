use crate::support::{decl, Fixture};
use contextful_core::store::bound_time::Bounds;
use serde_json::json;

#[test]
#[cfg(feature = "read")]
fn the_project_audit_key_has_one_stable_private_creation_and_refuses_bad_files() {
    let fixture = Fixture::new();
    let project = contextful_context::project::Project { dir: fixture.store.root().ancestors().nth(3).unwrap().to_path_buf(), name: "research".to_string() };
    let store = fixture.store.clone();
    let other = project.clone();
    let creator = std::thread::spawn(move || contextful_context::project::audit_key(&store, &other).unwrap());
    let first = contextful_context::project::audit_key(&fixture.store, &project).unwrap();
    assert!(first == creator.join().unwrap());
    assert!(first == contextful_context::project::audit_key(&fixture.store, &project).unwrap());
    let reopened = contextful_context::Store::open(&project.dir, &project.name).unwrap();
    let original_hash = contextful_policy::audit::query_digest(&first, "contextful.erasure.subject\nalice");
    assert_eq!(original_hash, contextful_policy::audit::query_digest(&contextful_context::project::audit_key(&reopened, &project).unwrap(), "contextful.erasure.subject\nalice"));
    let path = project.dir.join(".contextful/audit.key");
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    std::fs::write(&path, b"truncated").unwrap();
    assert!(contextful_context::project::audit_key(&fixture.store, &project).is_err());
    std::fs::remove_file(&path).unwrap();
    #[cfg(unix)] {
        let target = project.dir.join("external-key");
        std::fs::write(&target, first).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(contextful_context::project::audit_key(&fixture.store, &project).is_err());
    }
}

#[test]
#[cfg(feature = "read")]
fn recovery_discards_uncommitted_owned_replacements_without_touching_live_tables() {
    let fixture = Fixture::new();
    let table = decl("name = \"notes\"\nprimary_key = [\"id\"]");
    fixture.land(&table, "run-1", json!([{ "id":"kept", "text":"live" }]), "2030-01-01T00:00:00Z").unwrap();
    let staged = fixture.store.root().join("_erasure/staging").join("a".repeat(64));
    std::fs::create_dir_all(staged.join("tables/notes")).unwrap();
    std::fs::write(staged.join("tables/notes/partial"), b"unfinished owned replacement").unwrap();
    contextful_context::erase::recover_committed_erasure(&fixture.store).unwrap();
    assert!(!staged.exists(), "an uncommitted replacement survives recovery");
    assert_eq!(contextful_context::rows::table_rows(&fixture.store, &table, &["id"]).unwrap()[0]["id"], "kept");
    contextful_context::erase::recover_committed_erasure(&fixture.store).unwrap();
}

#[test]
#[cfg(feature = "read")]
fn a_published_erasure_never_regenerates_a_missing_project_audit_key() {
    let fixture = Fixture::new();
    let project = contextful_context::project::Project { dir:fixture.store.root().ancestors().nth(3).unwrap().to_path_buf(), name:"research".into() };
    contextful_context::project::audit_key(&fixture.store, &project).unwrap();
    std::fs::write(fixture.store.root().join("_erasure_frontier.json"), b"present publication").unwrap();
    let key = project.dir.join(".contextful/audit.key");
    std::fs::remove_file(&key).unwrap();
    assert!(contextful_context::project::audit_key(&fixture.store, &project).is_err(), "published erasure regenerates its missing pseudonym key");
    assert!(!key.exists());
}

#[test]
fn a_malformed_erasure_frontier_refuses_store_paths_before_content_access() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.store.root()).unwrap();
    std::fs::write(fixture.store.root().join("_erasure_frontier.json"), b"{truncated").unwrap();
    let result = fixture.store.table_dir("notes");
    assert!(result.is_err(), "malformed erasure publication was ignored: {result:?}");
    assert!(result.unwrap_err().to_string().starts_with("ErasureTransactionIncomplete"));
}

#[test]
fn a_disagreeing_erasure_frontier_exposes_no_pre_erasure_scan() {
    let fixture = Fixture::new();
    let table = decl("name = \"notes\"\nprimary_key = [\"id\"]");
    fixture.land(&table, "run-0001", json!([{ "id":"victim", "text":"private" }]), "2030-01-01T00:00:00Z").unwrap();
    std::fs::write(fixture.store.root().join("_erasure_frontier.json"), json!({
        "version":1, "transaction_id":"a".repeat(64), "audit_hash":"b".repeat(64),
        "tables":{"notes":{"directory":"../../untrusted", "certificate_sha256":"c".repeat(64)}}
    }).to_string()).unwrap();
    let result = fixture.scan(&table, Bounds::default());
    assert!(result.is_err(), "disagreeing erasure frontier exposes the original relation: {result:?}");
    assert!(result.unwrap_err().to_string().starts_with("ErasureTransactionIncomplete"));
}

fn select_replacement(fixture: &Fixture, table: &str) -> std::path::PathBuf {
    select_store_replacement(&fixture.store, table)
}

pub(crate) fn select_store_replacement(store: &contextful_context::Store, table: &str) -> std::path::PathBuf {
    use sha2::{Digest, Sha256};
    let transaction = "a".repeat(64);
    let audit = "b".repeat(64);
    let relative = format!("_erasure/committed/{transaction}/tables/{table}");
    let directory = store.root().join(&relative);
    std::fs::create_dir_all(directory.parent().unwrap()).unwrap();
    fn copy(source: &std::path::Path, target: &std::path::Path) {
        std::fs::create_dir_all(target).unwrap();
        for entry in std::fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let destination = target.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() { copy(&entry.path(), &destination); }
            else { std::fs::copy(entry.path(), destination).unwrap(); }
        }
    }
    copy(&store.table_dir(table).unwrap(), &directory);
    let certificate = serde_json::to_vec(&json!({"version":1,"transaction_id":transaction,"table":table,"audit_hash":audit})).unwrap();
    let hash = Sha256::digest(&certificate).iter().map(|b| format!("{b:02x}")).collect::<String>();
    std::fs::write(directory.join("_erasure_certificate.json"), certificate).unwrap();
    std::fs::write(store.root().join("_erasure_frontier.json"), json!({
        "version":1,"transaction_id":transaction,"audit_hash":audit,
        "tables":{table:{"directory":relative,"certificate_sha256":hash}}
    }).to_string()).unwrap();
    directory
}

#[test]
#[cfg(feature = "read")]
fn a_selected_replacement_keeps_logical_file_names_and_reads_its_physical_files() {
    let mut fixture = Fixture::new();
    let table = decl("name = \"research/notes\"\nprimary_key = [\"id\"]");
    fixture.land(&table, "run-0001", json!([{ "id":"survivor", "text":"retained" }]), "2030-01-01T00:00:00Z").unwrap();
    let (store, selected) = select_signed_store_replacement(&fixture.store, "research/notes");
    fixture.store = store;
    assert_eq!(fixture.store.table_dir("research/notes").unwrap(), selected);
    let scan = fixture.scan(&table, Bounds::default()).unwrap();
    assert!(scan.files.iter().all(|p| p.starts_with("tables/research/notes/")));
    assert!(scan.relation.contains("_erasure/working/"), "the relation bypasses the selected frontier: {}", scan.relation);
    assert_eq!(fixture.store.tables().unwrap(), vec!["research/notes"]);
    let rows = contextful_context::rows::table_rows(&fixture.store, &table, &["id", "text"]).unwrap();
    assert_eq!(rows[0]["id"], json!("survivor"));
}

#[cfg(feature = "read")]
pub(crate) fn select_signed_store_replacement(store: &contextful_context::Store, table: &str) -> (contextful_context::Store, std::path::PathBuf) {
    let (store, directory, _) = signed_fixture(store, table);
    (store, directory)
}

#[cfg(feature = "read")]
fn signed_fixture(store: &contextful_context::Store, table: &str) -> (contextful_context::Store, std::path::PathBuf, contextful_policy::issue::SignerKey) {
    signed_inventory_fixture(store, table, true)
}

#[cfg(feature = "read")]
fn signed_inventory_fixture(store: &contextful_context::Store, table: &str, file_inventory: bool) -> (contextful_context::Store, std::path::PathBuf, contextful_policy::issue::SignerKey) {
    signed_retirement_fixture(store, table, |retirement| {
        if !file_inventory {
            retirement.as_object_mut().unwrap().remove("version");
            retirement.as_object_mut().unwrap().remove("files");
        }
    })
}

#[cfg(feature = "read")]
fn signed_retirement_fixture(store: &contextful_context::Store, table: &str, change: impl FnOnce(&mut serde_json::Value)) -> (contextful_context::Store, std::path::PathBuf, contextful_policy::issue::SignerKey) {
    use contextful_policy::audit::AuditLog;
    use contextful_policy::issue::{SeedSigner, SignerKey};
    use contextful_core::issue::SignatureAlgorithm;
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    fn hash(bytes: &[u8]) -> String { Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect() }
    fn visit(root: &std::path::Path, path: &std::path::Path, files: &mut BTreeMap<String,String>) {
        for item in std::fs::read_dir(path).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() { visit(root, &path, files); }
            else if path != root.join("_erasure_certificate.json") {
                files.insert(path.strip_prefix(root).unwrap().to_str().unwrap().replace('\\', "/"), hash(&std::fs::read(path).unwrap()));
            }
        }
    }
    let directory = select_store_replacement(store, table);
    let store_id = contextful_context::project::ensure_store_id(store.root()).unwrap();
    let mut files = BTreeMap::new();
    visit(&directory, &directory, &mut files);
    let mut retired_files = BTreeMap::new();
    let retired = store.root().join("tables").join(table);
    visit(&retired, &retired, &mut retired_files);
    let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let key = SignerKey::of(&signer);
    let audit_dir = store.root().parent().unwrap().join("fixture-audit");
    let log = AuditLog::anchor(&audit_dir, signer).unwrap();
    let transaction = "a".repeat(64);
    let mut retirement = json!({"version":2,"directory":format!("tables/{table}"),"inventory_sha256":hash(&serde_json::to_vec(&retired_files).unwrap()),"files":retired_files});
    change(&mut retirement);
    let entry = log.append(json!({"operation":"erasure", "transaction_id":transaction,"store_id":store_id,
        "subject_hash":contextful_policy::audit::query_digest(b"fixture-audit-key", "erasure-subject:alice"),
        "replacement_hashes":{table:hash(&serde_json::to_vec(&files).unwrap())},
        "retired_directories":{table:[retirement]},
        "affected_counts":{table:1}})).unwrap();
    log.export().unwrap();
    drop(log);
    let certificate = serde_json::to_vec(&json!({"version":1,"transaction_id":transaction,"table":table,"audit_hash":entry.entry_hash,"files":files})).unwrap();
    std::fs::write(directory.join("_erasure_certificate.json"), &certificate).unwrap();
    let relative = format!("_erasure/committed/{transaction}/tables/{table}");
    let working_relative = format!("_erasure/working/{transaction}/tables/{table}");
    let working = store.root().join(&working_relative);
    for name in files.keys() {
        let destination = working.join(name);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::copy(directory.join(name), destination).unwrap();
    }
    std::fs::write(store.root().join("_erasure_frontier.json"), json!({"version":1,"transaction_id":transaction,
        "audit_hash":entry.entry_hash,"audit_seq":entry.seq,"tables":{table:{"directory":working_relative,"baseline_directory":relative,"certificate_sha256":hash(&certificate)}}}).to_string()).unwrap();
    (store.with_erasure_verifier(audit_dir, vec![key.clone()]).unwrap(), working, key)
}

#[test]
#[cfg(feature = "read")]
fn a_live_erasure_verifier_refuses_a_key_retired_after_opening() {
    use contextful_core::AuthorityError;
    use contextful_policy::keyset::{KeySet, KeySource, StaticPins};
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
    struct LiveKeys { pins: StaticPins, retired: Arc<AtomicBool> }
    impl KeySource for LiveKeys {
        fn keys(&self) -> Result<Arc<KeySet>, AuthorityError> {
            if self.retired.load(Ordering::SeqCst) { Err(AuthorityError::KeySetUnavailable("the fixture issuer is retired".into())) }
            else { self.pins.keys() }
        }
        fn keys_after_signature_failure(&self) -> Result<Arc<KeySet>, AuthorityError> { self.keys() }
    }
    let fixture = Fixture::new();
    let table = decl("name = \"notes\"\nprimary_key = [\"id\"]");
    fixture.land(&table, "run-0001", json!([{ "id":"survivor" }]), "2030-01-01T00:00:00Z").unwrap();
    let (store, directory, key) = signed_fixture(&fixture.store, "notes");
    let retired = Arc::new(AtomicBool::new(false));
    let source = Arc::new(LiveKeys { pins:StaticPins::parse(&format!("ed25519/{}", key.public_key.iter().map(|byte| format!("{byte:02x}")).collect::<String>())).unwrap(), retired:retired.clone() });
    let bound = store.with_erasure_key_source(fixture.store.root().parent().unwrap().join("fixture-audit"), source).unwrap();
    assert_eq!(bound.table_dir("notes").unwrap(), directory);
    assert!(bound.frontier_stamp().unwrap().is_some());
    retired.store(true, Ordering::SeqCst);
    let result = bound.table_dir("notes");
    assert!(result.is_err(), "an opened Store retains a retired erasure key: {result:?}");
    assert!(bound.frontier_stamp().is_err(), "cached reads retain a retired verifier frontier");
}

#[test]
#[cfg(feature = "read")]
fn a_signed_erasure_frontier_refuses_a_substituted_replacement_file() {
    let fixture = Fixture::new();
    let table = decl("name = \"notes\"\nprimary_key = [\"id\"]");
    fixture.land(&table, "run-0001", json!([{ "id":"survivor" }]), "2030-01-01T00:00:00Z").unwrap();
    let (store, directory) = select_signed_store_replacement(&fixture.store, "notes");
    assert_eq!(store.table_dir("notes").unwrap(), directory);
    let baseline = store.root().join(format!("_erasure/committed/{}/tables/notes", "a".repeat(64)));
    std::fs::write(baseline.join("schema.json"), b"substituted").unwrap();
    assert!(store.table_dir("notes").unwrap_err().to_string().starts_with("ErasureTransactionIncomplete"));
}

#[test]
#[cfg(feature = "read")]
fn a_frontier_cannot_choose_its_own_signer_or_an_unconfigured_verifier() {
    use contextful_policy::issue::{SeedSigner, SignerKey};
    use contextful_core::issue::SignatureAlgorithm;
    let fixture = Fixture::new();
    let table = decl("name = \"notes\"\nprimary_key = [\"id\"]");
    fixture.land(&table, "run-0001", json!([{ "id":"survivor" }]), "2030-01-01T00:00:00Z").unwrap();
    let (trusted, _) = select_signed_store_replacement(&fixture.store, "notes");
    assert!(trusted.table_dir("notes").is_ok());
    assert!(fixture.store.table_dir("notes").unwrap_err().to_string().starts_with("ErasureTransactionIncomplete"));
    let other = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let wrong = fixture.store.with_erasure_verifier(fixture.store.root().parent().unwrap().join("fixture-audit"), vec![SignerKey::of(&other)]).unwrap();
    assert!(wrong.table_dir("notes").unwrap_err().to_string().starts_with("ErasureTransactionIncomplete"));
}

#[test]
#[cfg(feature = "read")]
fn a_signed_frontier_cannot_replay_into_a_recreated_store_identity() {
    let fixture = Fixture::new();
    let table = decl("name = \"notes\"\nprimary_key = [\"id\"]");
    fixture.land(&table, "run-0001", json!([{ "id":"survivor" }]), "2030-01-01T00:00:00Z").unwrap();
    let (store, _) = select_signed_store_replacement(&fixture.store, "notes");
    assert!(store.table_dir("notes").is_ok());
    std::fs::write(store.root().join(contextful_core::store::lay_out::STORE_ID_FILE), b"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb\n").unwrap();
    let result = store.table_dir("notes");
    assert!(result.is_err(), "a signed transaction replays across store identity: {result:?}");
    assert!(result.unwrap_err().to_string().starts_with("ErasureTransactionIncomplete"));
}

#[test]
#[cfg(feature = "read")]
fn committed_erasure_recovery_collects_only_the_signed_retired_inventory() {
    let fixture = Fixture::new();
    let table = decl("name = \"notes\"\nprimary_key = [\"id\"]");
    fixture.land(&table, "run-0001", json!([{ "id":"erased", "text":"private" }]), "2030-01-01T00:00:00Z").unwrap();
    let (bound, replacement) = select_signed_store_replacement(&fixture.store, "notes");
    let original = fixture.store.root().join("tables/notes");
    assert!(original.is_dir());
    contextful_context::erase::recover_committed_erasure(&bound).unwrap();
    assert!(!original.exists());
    assert!(replacement.is_dir());
    contextful_context::erase::recover_committed_erasure(&bound).unwrap();
    assert_eq!(bound.table_dir("notes").unwrap(), replacement);
}

#[test]
#[cfg(feature = "read")]
fn committed_erasure_recovery_resumes_after_a_partial_retired_directory_unlink() {
    let fixture = Fixture::new();
    let table = decl("name = \"notes\"\nprimary_key = [\"id\"]");
    fixture.land(&table, "run-0001", json!([{ "id":"retired", "text":"remaining-retired-file" }]), "2030-01-01T00:00:00Z").unwrap();
    let (bound, replacement) = select_signed_store_replacement(&fixture.store, "notes");
    let original = fixture.store.root().join("tables/notes");
    let baseline = bound.root().join(format!("_erasure/committed/{}/tables/notes", "a".repeat(64)));
    let baseline_schema = std::fs::read(baseline.join("schema.json")).unwrap();
    let remaining = original.join(contextful_core::store::lay_out::RUNS_DIR).join("run-0001");
    assert!(remaining.is_dir(), "the fixture has no retained run to collect");
    assert!(original.join("schema.json").is_file());
    // A terminated recursive collection can remove one admitted file before its
    // directory disappears. Recovery must validate and collect the remainder.
    std::fs::remove_file(original.join("schema.json")).unwrap();
    let result = contextful_context::erase::recover_committed_erasure(&bound);
    assert!(result.is_ok(), "partial admitted collection cannot resume: {result:?}");
    assert!(!original.exists());
    assert!(replacement.is_dir());
    assert_eq!(std::fs::read(baseline.join("schema.json")).unwrap(), baseline_schema);
    contextful_context::erase::recover_committed_erasure(&bound).unwrap();
}

#[test]
#[cfg(feature = "read")]
fn legacy_retirement_requires_the_complete_original_inventory() {
    for partial in [false, true] {
        let fixture = Fixture::new();
        let table = decl("name = \"notes\"\nprimary_key = [\"id\"]");
        fixture.land(&table, "run-0001", json!([{ "id":"retired" }]), "2030-01-01T00:00:00Z").unwrap();
        let (bound, replacement, _) = signed_inventory_fixture(&fixture.store, "notes", false);
        let original = bound.root().join("tables/notes");
        if partial { std::fs::remove_file(original.join("schema.json")).unwrap(); }
        let result = contextful_context::erase::recover_committed_erasure(&bound);
        if partial {
            assert!(result.unwrap_err().to_string().starts_with("ErasureTransactionIncomplete"));
            assert!(original.is_dir());
        } else {
            result.unwrap();
            assert!(!original.exists());
        }
        assert!(replacement.is_dir());
    }
}

#[test]
#[cfg(feature = "read")]
fn signed_retirement_refuses_malformed_maps_and_changed_remaining_files() {
    use sha2::{Digest, Sha256};
    for case in ["aggregate", "traversal", "version", "changed", "added"] {
        let fixture = Fixture::new();
        let table = decl("name = \"notes\"\nprimary_key = [\"id\"]");
        fixture.land(&table, "run-0001", json!([{ "id":"retired" }]), "2030-01-01T00:00:00Z").unwrap();
        let (bound, replacement, _) = signed_retirement_fixture(&fixture.store, "notes", |retirement| match case {
            "aggregate" => retirement["inventory_sha256"] = json!("0".repeat(64)),
            "traversal" => {
                retirement["files"]["../outside"] = json!("a".repeat(64));
                let files: std::collections::BTreeMap<String,String> = serde_json::from_value(retirement["files"].clone()).unwrap();
                let hash = Sha256::digest(serde_json::to_vec(&files).unwrap()).iter().map(|byte| format!("{byte:02x}")).collect::<String>();
                retirement["inventory_sha256"] = json!(hash);
            },
            "version" => retirement["version"] = json!(3),
            _ => (),
        });
        let original = bound.root().join("tables/notes");
        if case == "changed" { std::fs::write(original.join("schema.json"), b"different admitted file").unwrap(); }
        if case == "added" { std::fs::write(original.join("new-file"), b"unadmitted bytes").unwrap(); }
        let result = contextful_context::erase::recover_committed_erasure(&bound);
        assert!(result.unwrap_err().to_string().starts_with("ErasureTransactionIncomplete"), "{case}");
        assert!(original.join("schema.json").is_file(), "{case}: recovery deleted before validating");
        assert!(replacement.is_dir());
    }
}

#[test]
#[cfg(all(feature = "read", unix))]
fn partial_retirement_refuses_a_substituted_remaining_symlink() {
    let fixture = Fixture::new();
    let table = decl("name = \"notes\"\nprimary_key = [\"id\"]");
    fixture.land(&table, "run-0001", json!([{ "id":"retired" }]), "2030-01-01T00:00:00Z").unwrap();
    let (bound, replacement) = select_signed_store_replacement(&fixture.store, "notes");
    let original = bound.root().join("tables/notes");
    let foreign = fixture.store.root().join("foreign-schema");
    std::fs::copy(original.join("schema.json"), &foreign).unwrap();
    std::fs::remove_file(original.join("schema.json")).unwrap();
    std::os::unix::fs::symlink(&foreign, original.join("schema.json")).unwrap();
    assert!(contextful_context::erase::recover_committed_erasure(&bound).is_err());
    assert!(std::fs::symlink_metadata(original.join("schema.json")).unwrap().file_type().is_symlink());
    assert!(foreign.is_file());
    assert!(replacement.is_dir());
}

#[test]
#[cfg(feature = "read")]
fn recovery_refuses_changed_retired_content_without_deleting_it() {
    let fixture = Fixture::new();
    let table = decl("name = \"notes\"\nprimary_key = [\"id\"]");
    fixture.land(&table, "run-0001", json!([{ "id":"erased", "text":"private" }]), "2030-01-01T00:00:00Z").unwrap();
    let (bound, replacement) = select_signed_store_replacement(&fixture.store, "notes");
    let original = fixture.store.root().join("tables/notes");
    std::fs::write(original.join("unadmitted-file"), "new owned content").unwrap();
    let outcome = contextful_context::erase::recover_committed_erasure(&bound);
    assert!(outcome.is_err(), "recovery deleted an inventory absent from the signed erasure: {outcome:?}");
    assert!(original.join("unadmitted-file").is_file());
    assert!(replacement.is_dir());
}

#[test]
#[cfg(feature = "read")]
fn an_erased_store_accepts_normal_append_fold_and_recovery_without_a_signer() {
    let mut fixture = Fixture::new();
    let table = decl("name = \"notes\"\nprimary_key = [\"id\"]");
    fixture.land(&table, "run-0001", json!([{ "id":"survivor", "text":"retained" }]), "2030-01-01T00:00:00Z").unwrap();
    let (bound, _) = select_signed_store_replacement(&fixture.store, "notes");
    fixture.store = bound;
    let baseline = fixture.store.root().join(format!("_erasure/committed/{}/tables/notes", "a".repeat(64)));
    let schema = std::fs::read(baseline.join("schema.json")).unwrap();
    let before = fixture.store.frontier_stamp().unwrap();
    assert_ne!(fixture.store.table_dir("notes").unwrap(), baseline);
    contextful_context::erase::recover_committed_erasure(&fixture.store).unwrap();
    let landed = fixture.land(&table, "run-0002", json!([{ "id":"next", "text":"ordinary write" }]), "2030-01-01T00:02:00Z");
    assert!(landed.is_ok(), "an erasure certificate disables ordinary append: {landed:?}");
    assert_ne!(fixture.store.frontier_stamp().unwrap(), before);
    contextful_context::fold::fold(&fixture.store, &table, crate::support::at("2030-01-01T00:03:00Z")).unwrap();
    let rows = contextful_context::rows::table_rows(&fixture.store, &table, &["id"]).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|row| row["id"] == "next"));
    assert_eq!(std::fs::read(baseline.join("schema.json")).unwrap(), schema);
    contextful_context::erase::recover_committed_erasure(&fixture.store).unwrap();
    assert_eq!(contextful_context::rows::table_rows(&fixture.store, &table, &["id"]).unwrap(), rows);
}

#[test]
#[cfg(feature = "read")]
fn normal_publication_waits_for_the_erasure_frontier_before_mutating_working_files() {
    let mut fixture = Fixture::new();
    let table = decl("name = \"notes\"\nprimary_key = [\"id\"]");
    fixture.land(&table, "run-0001", json!([{ "id":"survivor" }]), "2030-01-01T00:00:00Z").unwrap();
    fixture.store = select_signed_store_replacement(&fixture.store, "notes").0;
    contextful_context::erase::recover_committed_erasure(&fixture.store).unwrap();
    let lease = fixture.store.lock_frontier().unwrap();
    let before = fixture.store.frontier_stamp().unwrap();
    let (started, ready) = std::sync::mpsc::channel();
    let (finished, done) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            started.send(()).unwrap();
            finished.send(fixture.land(&table, "run-0002", json!([{ "id":"next" }]), "2030-01-01T00:02:00Z")).unwrap();
        });
        ready.recv().unwrap();
        assert!(matches!(done.recv_timeout(std::time::Duration::from_millis(100)), Err(std::sync::mpsc::RecvTimeoutError::Timeout)));
        assert_eq!(fixture.store.frontier_stamp().unwrap(), before);
        drop(lease);
        done.recv_timeout(std::time::Duration::from_secs(5)).unwrap().unwrap();
    });
    assert_ne!(fixture.store.frontier_stamp().unwrap(), before);
    assert_eq!(contextful_context::rows::table_rows(&fixture.store, &table, &["id"]).unwrap().len(), 2);
}

#[test]
#[cfg(feature = "read")]
fn subject_erasure_publishes_survivors_and_collects_original_retained_parts() {
    use contextful_context::erase::{erase_subject, EraseRequest, EraseSelector};
    use contextful_core::grant::Action;
    use contextful_core::issue::SignatureAlgorithm;
    use contextful_core::ports::FixedClock;
    use contextful_policy::enforce::erase::ForgetAdmission;
    use contextful_policy::issue::SeedSigner;
    use contextful_policy::revoke::RevocationState;
    use contextful_policy::verify::{effect_boundary, Admission};
    let fixture = Fixture::new();
    let table = decl("name = \"notes\"\nprimary_key = [\"id\"]\nsubject_id = \"subject\"\nerasure_key = \"id\"\nretain_runs = \"90d\"\n[[pipeline.tables.indexes]]\nkind = \"fulltext\"\ncolumn = \"text\"");
    fixture.land(&table, "run-0001", json!([{ "id":"a", "subject":"alice", "text":"private" }, {"id":"b", "subject":"bob", "text":"retained"}]), "2030-01-01T00:00:00Z").unwrap();
    contextful_context::project::ensure_store_id(fixture.store.root()).unwrap();
    contextful_context::fold::fold(&fixture.store, &table, crate::support::at("2030-01-01T00:01:00Z")).unwrap();
    fixture.land(&table, "run-0002", json!([{ "id":"b", "subject":"bob", "text":"retained updated"}]), "2030-01-01T00:02:00Z").unwrap();
    contextful_context::fold::fold(&fixture.store, &table, crate::support::at("2030-01-01T00:03:00Z")).unwrap();
    let reads = crate::read::Reads::new();
    let mut grant = crate::read::read(&["*"], None);
    grant.actions = vec![Action::Forget];
    let authority = reads.authority(crate::read::loop_subject("agent://eraser"), vec![grant]);
    let admission = ForgetAdmission::admit(&authority, &["notes"]).unwrap();
    let now = crate::read::at("2030-01-01T00:05:00Z");
    let revocation = RevocationState::default();
    let boundary = |authority: &contextful_policy::verify::AdmittedAuthority| effect_boundary(authority, &Admission::new(now, &revocation));
    let declarations = [table.clone()];
    let tables = ["notes".to_string()];
    let audit_dir = fixture.store.root().parent().unwrap().join("fixture-audit");
    let signer = std::sync::Arc::new(SeedSigner::generate(SignatureAlgorithm::Ed25519));
    let request = EraseRequest { declarations: &declarations, tables: &tables, selector: EraseSelector::Subject("alice"), admission: &admission,
        signer: Some(signer.clone()), audit_dir: &audit_dir, audit_key: b"fixture-project-audit-key", boundary: &boundary, clock: &FixedClock(now) };
    let configured = fixture.store.with_erasure_verifier(audit_dir.clone(), vec![contextful_policy::issue::SignerKey::of(signer.as_ref())]).unwrap();
    let erased = erase_subject(&configured, request).unwrap();
    let rows = contextful_context::rows::table_rows(&erased.store, &table, &["id", "subject", "text"]).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "b");
    let face = contextful_context::read::Face::open(erased.store.clone(), "[[pipeline.tables]]\nname = \"notes\"\nprimary_key = [\"id\"]", crate::read::pepper()).unwrap();
    let reader = reads.authority(crate::read::loop_subject("agent://reference"), vec![crate::read::read(&["notes"], None)]);
    let session = face.session(&reader, &contextful_policy::enforce::session::Request { zone:None }, Default::default()).unwrap();
    let reference = face.reference(&session, "notes", "run-0001", 0, Default::default()).unwrap();
    assert_eq!(serde_json::to_value(reference).unwrap()["rows"], json!([[false, "erased"]]));
    let bounded = face.reference(&session, "notes", "run-0001", 0, contextful_context::read::ReadOptions { limit:Some(0), ..Default::default() }).unwrap();
    assert!(bounded.rows.is_empty() && bounded.truncated, "a zero row ceiling still releases a reference verdict");
    assert_eq!(serde_json::to_value(face.reference(&session, "notes", "never-landed", 0, Default::default()).unwrap()).unwrap()["rows"], json!([[false, "missing"]]));
    assert!(!fixture.store.root().join("tables/notes").exists(), "the original retained table remains physically readable");
    fn check_bytes(directory: &std::path::Path) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() { check_bytes(&path); }
            else {
                if path.extension().is_some_and(|extension| extension == "parquet") {
                    for batch in contextful_context::parquet_io::read(&path).unwrap() {
                        let schema = batch.schema();
                        let columns = schema.fields().iter().map(|field| field.name().as_str()).collect::<Vec<_>>();
                        let rows = contextful_context::rows::batch_rows(&batch, &columns).unwrap();
                        let text = serde_json::to_string(&rows).unwrap();
                        assert!(!text.contains("alice") && !text.contains("private"), "erased rows remain in a retained Parquet version");
                    }
                }
                assert!(!std::fs::read(path).unwrap().windows(b"private".len()).any(|value| value == b"private"));
            }
        }
    }
    check_bytes(erased.store.root());
    let historical = contextful_context::rows::table_rows(&erased.store, &table, &["subject"]).unwrap();
    assert!(historical.iter().all(|row| row["subject"] == "bob"));
    let audit = contextful_policy::audit::entries(&audit_dir).unwrap();
    assert_eq!(audit.len(), 1);
    let encoded = serde_json::to_string(&audit).unwrap();
    assert!(!encoded.contains("alice") && !encoded.contains("private"));
    let request = EraseRequest { declarations: &declarations, tables: &tables, selector: EraseSelector::Subject("bob"), admission: &admission,
        signer: Some(signer), audit_dir: &audit_dir, audit_key: b"fixture-project-audit-key", boundary: &boundary, clock: &FixedClock(now) };
    let again = erase_subject(&erased.store, request).unwrap();
    assert!(contextful_context::rows::table_rows(&again.store, &table, &["subject"]).unwrap().is_empty());
    let face = contextful_context::read::Face::open(again.store.clone(), "[[pipeline.tables]]\nname = \"notes\"\nprimary_key = [\"id\"]", crate::read::pepper()).unwrap();
    let session = face.session(&reader, &contextful_policy::enforce::session::Request { zone:None }, Default::default()).unwrap();
    assert_eq!(face.reference(&session, "notes", "run-0001", 0, Default::default()).unwrap().to_json()["rows"], json!([[false, "erased"]]), "a second erasure drops the prior reference index");
    let restricted = contextful_context::read::Face::open(again.store.clone(), "[[pipeline.tables]]\nname = \"notes\"\nprimary_key = [\"id\"]\n[pipeline.tables.policy.rows]\npredicate = \"false\"", crate::read::pepper()).unwrap();
    let restricted_session = restricted.session(&reader, &contextful_policy::enforce::session::Request { zone:None }, Default::default()).unwrap();
    assert_eq!(restricted.reference(&restricted_session, "notes", "run-0001", 0, Default::default()).unwrap().to_json()["rows"], json!([[false, "unreadable"]]));
    let ungranted = reads.authority(crate::read::loop_subject("agent://outsider"), vec![crate::read::read(&["other"], None)]);
    let ungranted_session = face.session(&ungranted, &contextful_policy::enforce::session::Request { zone:None }, Default::default()).unwrap();
    assert!(face.reference(&ungranted_session, "notes", "run-0001", 0, Default::default()).is_err());
    fn no_subject(directory: &std::path::Path, subject: &str) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() { no_subject(&path, subject); }
            else if path.extension().is_some_and(|extension| extension == "parquet") {
                for batch in contextful_context::parquet_io::read(&path).unwrap() {
                    let columns = batch.schema();
                    let names = columns.fields().iter().map(|field| field.name().as_str()).collect::<Vec<_>>();
                    let rows = contextful_context::rows::batch_rows(&batch, &names).unwrap();
                    assert!(!serde_json::to_string(&rows).unwrap().contains(subject), "an older signed baseline retains the erased subject in {}", path.display());
                }
            }
        }
    }
    no_subject(again.store.root(), "bob");
    let index = again.store.root().join("_erasure/committed").join(&again.transaction_id).join("tables/notes/_erased_references.json");
    let certified = std::fs::read(&index).unwrap();
    for substituted in [b"{malformed".as_slice(), br#"{"version":1,"store_id":"foreign","table":"notes","references":[["run-0001",0]]}"#.as_slice()] {
        std::fs::write(&index, substituted).unwrap();
        assert!(face.reference(&session, "notes", "run-0001", 0, Default::default()).is_err());
    }
    std::fs::remove_file(&index).unwrap();
    assert!(face.reference(&session, "notes", "run-0001", 0, Default::default()).is_err(), "a deleted cumulative index converts erased into missing");
    std::fs::write(&index, certified).unwrap();
    assert_eq!(face.reference(&session, "notes", "run-0001", 0, Default::default()).unwrap().to_json()["rows"], json!([[false, "erased"]]));
}

#[test]
#[cfg(feature = "read")]
fn keyset_erasure_uses_the_same_signed_publication_and_retains_other_keys() {
    use contextful_context::erase::{erase, EraseRequest, EraseSelector};
    use contextful_core::{grant::Action, issue::SignatureAlgorithm, ports::FixedClock};
    use contextful_policy::{enforce::erase::ForgetAdmission, issue::SeedSigner, revoke::RevocationState, verify::{effect_boundary, Admission}};
    let fixture = Fixture::new();
    let table = decl("name = \"notes\"\nprimary_key = [\"id\"]\nerasure_key = \"id\"");
    fixture.land(&table, "run-0001", json!([{ "id":"private-key", "text":"private" }, {"id":"other-key", "text":"retained"}]), "2030-01-01T00:00:00Z").unwrap();
    contextful_context::project::ensure_store_id(fixture.store.root()).unwrap();
    let reads = crate::read::Reads::new();
    let mut grant = crate::read::read(&["*"], None); grant.actions = vec![Action::Forget];
    let authority = reads.authority(crate::read::loop_subject("agent://eraser"), vec![grant]);
    let admission = ForgetAdmission::admit(&authority, &["notes"]).unwrap();
    let now = crate::read::at("2030-01-01T00:05:00Z");
    let revocations = RevocationState::default();
    let boundary = |authority: &contextful_policy::verify::AdmittedAuthority| effect_boundary(authority, &Admission::new(now, &revocations));
    let keys = std::collections::BTreeMap::from([("notes".into(), vec![json!({"id":"private-key"}).as_object().unwrap().clone()])]);
    let subject_hash = contextful_policy::audit::query_digest(b"fixture-project-audit-key", "contextful.erasure.subject\nalice");
    let audit_dir = fixture.store.root().parent().unwrap().join("fixture-audit");
    let declarations = [table.clone()]; let tables = ["notes".to_string()];
    let signer = std::sync::Arc::new(SeedSigner::generate(SignatureAlgorithm::Ed25519));
    let clock = FixedClock(now);
    let request = || EraseRequest { declarations:&declarations, tables:&tables,
        selector:EraseSelector::KeySet { subject_hash:&subject_hash, keys:&keys }, admission:&admission,
        signer:Some(signer.clone()), audit_dir:&audit_dir,
        audit_key:b"fixture-project-audit-key", boundary:&boundary, clock:&clock };
    let other = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let wrong = fixture.store.with_erasure_verifier(audit_dir.clone(), vec![contextful_policy::issue::SignerKey::of(&other)]).unwrap();
    let refused = erase(&wrong, request());
    assert!(refused.is_err(), "the private signer replaces independently configured trust");
    assert!(!fixture.store.root().join("_erasure_frontier.json").exists());
    let configured = fixture.store.with_erasure_verifier(audit_dir.clone(), vec![contextful_policy::issue::SignerKey::of(signer.as_ref())]).unwrap();
    let result = erase(&configured, request()).unwrap();
    assert_eq!(result.subject_hash, subject_hash);
    let rows = contextful_context::rows::table_rows(&result.store, &table, &["id"]).unwrap();
    assert_eq!(rows, vec![json!({"id":"other-key"}).as_object().unwrap().clone()]);
    let evidence = serde_json::to_string(&contextful_policy::audit::entries(&audit_dir).unwrap()).unwrap();
    assert!(!evidence.contains("private-key") && !evidence.contains("alice"));
    assert!(!fixture.store.root().join("tables/notes").exists());
}

#[test]
fn a_changed_certificate_refuses_every_table_at_the_same_frontier() {
    let fixture = Fixture::new();
    let table = decl("name = \"notes\"\nprimary_key = [\"id\"]");
    fixture.land(&table, "run-0001", json!([{ "id":"survivor" }]), "2030-01-01T00:00:00Z").unwrap();
    let selected = select_replacement(&fixture, "notes");
    std::fs::write(selected.join("_erasure_certificate.json"), b"{}").unwrap();
    for name in ["notes", "unaffected"] {
        let error = fixture.store.table_dir(name).unwrap_err();
        assert!(error.to_string().starts_with("ErasureTransactionIncomplete"));
    }
}

#[test]
fn an_unsigned_structural_frontier_has_no_authority_to_publish_a_replacement() {
    let fixture = Fixture::new();
    let table = decl("name = \"notes\"\nprimary_key = [\"id\"]");
    fixture.land(&table, "run-0001", json!([{ "id":"survivor" }]), "2030-01-01T00:00:00Z").unwrap();
    select_replacement(&fixture, "notes");
    let result = fixture.store.table_dir("notes");
    assert!(result.is_err(), "unsigned structural records published a table: {result:?}");
    assert!(result.unwrap_err().to_string().starts_with("ErasureTransactionIncomplete"));
}
