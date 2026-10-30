//! Subject erasure over a store whose synthesis passes dead-lettered failed batches.

use super::support::*;
use contextful_context::erase::{erase_declared, EraseRequest, EraseSelector};
use contextful_context::read::Face;
use contextful_context::Store;
use contextful_core::grant::Action;
use contextful_core::issue::SignatureAlgorithm;
use contextful_core::ports::FixedClock;
use contextful_core::store::declare::{DeclarationSet, TableDecl};
use contextful_core::store::lay_out::NodeId;
use contextful_core::AuthorityError;
use contextful_memory::synthesize::Pass;
use contextful_memory::MemoryFault;
use contextful_policy::enforce::erase::ForgetAdmission;
use contextful_policy::enforce::mask::Pepper;
use contextful_policy::issue::{SeedSigner, SignerKey};
use contextful_policy::verify::AdmittedAuthority;
use serde_json::{json, Map, Value};
use std::path::Path;

const ERASE_MANIFEST: &str = r#"
[[pipeline.tables]]
name = "research/notes"
subject_id = "subject"

[[pipeline.tables]]
name = "research/calls"
subject_id = "subject"

[[table]]
name = "memory/facts"
shape = "memory_facts"
columns = ["claim_id", "subject", "predicate", "object", "scope", "tier", "confidence", "valid_from", "valid_to", "evidence", "superseded_by", "grant_id", "agent"]
"#;

const LETTERS: &str = "memory/facts_dead_letter";
const ERASED: &str = "erased-letter-canary";
const KEPT: &str = "kept-letter-canary";

fn admit() -> Result<(), AuthorityError> {
    Ok(())
}

/// A pass over `source` whose every extraction answer fails validation, so its batch
/// exhausts its attempts and dead-letters the last answer.
fn exhaust(f: &Fixture, writer: &AdmittedAuthority, source: &str, answer: &str) {
    let node = NodeId::parse("memory-a").unwrap();
    let answers = [answer.to_string(), answer.to_string(), answer.to_string()];
    let inference = Scripted::new(&answers);
    let pass = Pass { face: &f.face, authority: writer, inference: &inference, source, into: "memory/facts", state: &f.state, node: &node, now: at("2030-01-11T00:00:00Z"), boundary: &admit };
    assert!(matches!(pass.run(), Err(MemoryFault::Memory(_))), "the scripted batch exhausts its attempts");
}

fn letters(store: &Store) -> Vec<Map<String, Value>> {
    contextful_context::rows::table_rows(store, &TableDecl::named(LETTERS), &["stage", "reason", "detail", "response", "template_hash", "evidence", "_run_id", "_row_seq"]).unwrap()
}

/// Every retained file under `directory`, with its bytes.
fn retained(directory: &Path, out: &mut Vec<(std::path::PathBuf, Vec<u8>)>) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            retained(&path, out);
        } else {
            let bytes = std::fs::read(&path).unwrap();
            out.push((path, bytes));
        }
    }
}

/// Every retained file under `root`, the dead-letter table's parts among them, holds no
/// `canary` byte, decoded where it is Parquet.
fn holds_no(root: &Path, canary: &str) {
    let mut files = Vec::new();
    retained(root, &mut files);
    let letter_part = |path: &Path| path.to_string_lossy().contains("facts_dead_letter") && path.extension().is_some_and(|extension| extension == "parquet");
    assert!(files.iter().any(|(path, _)| letter_part(path)), "the scan reaches a dead-letter part");
    for (path, bytes) in &files {
        if path.extension().is_some_and(|extension| extension == "parquet") {
            for batch in contextful_context::parquet_io::read(path).unwrap() {
                let schema = batch.schema();
                let columns = schema.fields().iter().map(|field| field.name().as_str()).collect::<Vec<_>>();
                let rows = contextful_context::rows::batch_rows(&batch, &columns).unwrap();
                assert!(!serde_json::to_string(&rows).unwrap().contains(canary), "an erased dead letter survives in {}", path.display());
            }
        }
        assert_eq!(bytes.windows(canary.len()).position(|window| window == canary.as_bytes()), None, "erased bytes survive in {}", path.display());
    }
}

/// A subject's failed batch dead-letters its content; erasing the subject rewrites that
/// letter out of every retained file and leaves another subject's letter unchanged.
// spec: disclosure.erase.dead-letters@a2586c32
// spec: read.synthesize.dead-letter-evidence@81db37cf
#[test]
fn subject_erasure_rewrites_the_dead_letters_of_its_failed_batches() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), "research").unwrap();
    let face = Face::open(store.clone(), ERASE_MANIFEST, Pepper::resolve(|_| None)).unwrap();
    let f = Fixture::over(dir, face);
    let writer = f.writer();
    land_rows(&f.face, "research/notes", "run-0001", json!([{ "subject": "alice", "text": format!("Alice's record {ERASED}.") }]));
    land_rows(&f.face, "research/calls", "run-0002", json!([{ "subject": "bob", "text": format!("Bob's record {KEPT}.") }]));
    land_rows(&f.face, "memory/facts", "run-0003", json!([{ "claim_id": "c1", "subject": "acme", "predicate": "is", "object": "a company" }]));
    exhaust(&f, &writer, "research/notes", &format!("not json: {ERASED}"));
    exhaust(&f, &writer, "research/calls", &format!("not json: {KEPT}"));
    let before = letters(&store);
    assert_eq!(before.len(), 2, "each failed batch dead-letters one answer");
    let kept: Vec<_> = before.iter().filter(|row| row["response"].as_str().unwrap().contains(KEPT)).cloned().collect();
    assert_eq!(kept.len(), 1);
    let cited: Vec<Value> = before.iter().map(|row| serde_json::from_str(row["evidence"].as_str().expect("a dead letter carries its evidence")).unwrap()).collect();
    assert!(cited.contains(&json!([{ "table": "research/notes", "run": "run-0001", "seq": 0 }])), "{cited:?}");
    assert!(cited.contains(&json!([{ "table": "research/calls", "run": "run-0002", "seq": 0 }])), "{cited:?}");

    contextful_context::project::ensure_store_id(store.root()).unwrap();
    let project = contextful_context::project::Project { dir: f.dir.path().to_path_buf(), name: "research".into() };
    let audit_key = contextful_context::project::audit_key(&store, &project).unwrap();
    let audit_dir = project.audit_dir();
    let pack = DeclarationSet::parse(ERASE_MANIFEST, &[]).unwrap();
    let eraser = f.authority("agent://eraser", &[Action::Forget], &["*"]);
    let names: Vec<&str> = pack.tables().iter().map(|decl| decl.name.as_str()).collect();
    let admission = ForgetAdmission::admit(&eraser, &names).unwrap();
    let signer = std::sync::Arc::new(SeedSigner::generate(SignatureAlgorithm::Ed25519));
    let configured = store.with_erasure_verifier(audit_dir.clone(), vec![SignerKey::of(signer.as_ref())]).unwrap();
    let boundary = |_: &AdmittedAuthority| Ok(());
    let tables = ["research/notes".to_string(), "research/calls".to_string()];
    let erased = erase_declared(&configured, &pack, EraseRequest {
        declarations: pack.tables(), tables: &tables, selector: EraseSelector::Subject("alice"), admission: &admission,
        signer: Some(signer), audit_dir: &audit_dir, audit_key: &audit_key, boundary: &boundary, clock: &FixedClock(at("2030-01-12T00:00:00Z")),
    }).unwrap();

    holds_no(erased.store.root(), ERASED);
    assert_eq!(letters(&erased.store), kept, "another subject's dead letter survives unchanged");
}
