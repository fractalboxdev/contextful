//! Supplemental private-owner boundary control; the built CLI supplies runtime base RED.
use super::*;
use contextful_core::{grant::{Action, Grant, TablePattern}, identify::Subject, issue::{IssuancePolicy, Lifetime, MintContext, MintRequest, NodeRole, SignatureAlgorithm}, ports::FixedClock, revoke::EpochScope};
use contextful_policy::{issue::{mint, MintClaims, SeedSigner, SignerKey}, keyset::{KeySource, StaticPins}, revoke::RevocationState, verify::{effect_boundary, verify_inherited_pipe, Admission}};
use std::cell::{Cell, RefCell};

#[test]
fn revocation_between_retirements_stops_later_unlink_and_authorized_replay_completes() {
    let temporary = tempfile::tempdir().unwrap();
    let store = Store::open(temporary.path(), "research").unwrap();
    let project = crate::project::Project { dir:temporary.path().to_path_buf(), name:"research".into() };
    crate::project::audit_key(&store, &project).unwrap();
    let store_id = crate::project::ensure_store_id(store.root()).unwrap();
    let transaction = "a".repeat(64);
    let mut inventories = BTreeMap::new();
    let mut retirements = BTreeMap::new();
    for table in ["first", "second"] {
        let relative = format!("_erasure/committed/{transaction}/tables/{table}");
        let baseline = store.root().join(relative);
        let working = store.root().join(format!("_erasure/working/{transaction}/tables/{table}"));
        let retired = store.root().join(format!("tables/{table}"));
        for directory in [&baseline, &working, &retired] {
            std::fs::create_dir_all(directory).unwrap();
            std::fs::write(directory.join("schema.json"), b"{}").unwrap();
        }
        let inventory = crate::erasure_frontier::inventory(&baseline).unwrap();
        let retired_files = crate::erasure_frontier::retirement_inventory(&retired).unwrap();
        retirements.insert(table, json!([{"version":2,"directory":format!("tables/{table}"),"inventory_sha256":crate::store::etag(&serde_json::to_vec(&retired_files).unwrap()),"files":retired_files}]));
        inventories.insert(table, inventory);
    }
    let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let key = SignerKey::of(&signer);
    let audit_dir = project.audit_dir();
    let log = AuditLog::anchor(&audit_dir, signer).unwrap();
    let hashes: BTreeMap<_,_> = inventories.iter().map(|(table, inventory)| (*table, crate::store::etag(&serde_json::to_vec(inventory).unwrap()))).collect();
    let entry = log.append(json!({"operation":"erasure","transaction_id":transaction,"store_id":store_id,"replacement_hashes":hashes,"retired_directories":retirements})).unwrap();
    log.export().unwrap(); drop(log);
    let mut tables = BTreeMap::new();
    for (table, files) in inventories {
        let baseline = format!("_erasure/committed/{transaction}/tables/{table}");
        let bytes = serde_json::to_vec(&Certificate { version:1, transaction_id:transaction.clone(), table:table.into(), audit_hash:entry.entry_hash.clone(), files }).unwrap();
        std::fs::write(store.root().join(&baseline).join(CERTIFICATE_FILE), &bytes).unwrap();
        tables.insert(table.into(), TableReplacement { directory:format!("_erasure/working/{transaction}/tables/{table}"), baseline_directory:baseline, certificate_sha256:crate::store::etag(&bytes) });
    }
    let frontier = Frontier { version:1, transaction_id:transaction.clone(), audit_hash:entry.entry_hash, audit_seq:entry.seq, tables };
    let frontier_bytes = serde_json::to_vec(&frontier).unwrap();
    std::fs::write(store.root().join(FRONTIER_FILE), &frontier_bytes).unwrap();
    let store = store.with_erasure_verifier(audit_dir, vec![key]).unwrap();
    let issuer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let policy = IssuancePolicy::parse("default_audience = \"recovery-fixture\"\nmax_lifetime_secs = 3600\n").unwrap();
    let now = contextful_core::time::Instant::parse("2030-01-01T00:00:00Z").unwrap();
    let grant = Grant { actions:vec![Action::Forget], tables:vec![TablePattern::parse("*").unwrap()], tenant:None, aggregate:None, templates:None, max_rows:None, max_duration_ms:None, max_response_bytes:None };
    let mut request = MintRequest::custody(Subject { on_behalf_of:Some("user://fixture".into()), agent:Some("agent://fixture".into()), zone:Some("on-prem:fixture".into()), ..Subject::default() }, vec![grant]);
    request.lifetime = Lifetime::Requested(900);
    let plan = policy.check(&request, &MintContext { node:NodeRole::Primary, signer:&issuer, clock:&FixedClock(now) }).unwrap();
    let token = mint(&plan, &MintClaims::default(), &issuer).unwrap();
    let pins = StaticPins::parse(&issuer.public_key_text()).unwrap().keys().unwrap();
    let revocation = RefCell::new(RevocationState::default());
    let authority = verify_inherited_pipe(&token, &pins, &Admission::new(now, &revocation.borrow()).expecting("recovery-fixture")).unwrap();
    let boundaries = Cell::new(0);
    let boundary = |authority: &AdmittedAuthority| {
        boundaries.set(boundaries.get() + 1);
        if boundaries.get() == 3 {
            assert!(!store.root().join("tables/first").exists());
            assert!(store.root().join("tables/second").is_dir());
            revocation.borrow_mut().epochs.bump(EpochScope { project:"recovery-fixture".into(), tenant:None, principal_class:None });
        }
        effect_boundary(authority, &Admission::new(now, &revocation.borrow()))
    };
    let refused = recover_admitted_erasure(&store, &authority, &boundary).unwrap_err();
    assert!(refused.to_string().contains("AuthorityRevoked"), "{refused}");
    assert_eq!(boundaries.get(), 3);
    assert!(store.root().join("tables/second/schema.json").is_file());
    assert_eq!(std::fs::read(store.root().join(FRONTIER_FILE)).unwrap(), frontier_bytes);
    let token = mint(&plan, &MintClaims { epoch:1, ..MintClaims::default() }, &issuer).unwrap();
    let renewed = verify_inherited_pipe(&token, &pins, &Admission::new(now, &revocation.borrow()).expecting("recovery-fixture")).unwrap();
    let current = |authority: &AdmittedAuthority| effect_boundary(authority, &Admission::new(now, &revocation.borrow()));
    assert_eq!(recover_admitted_erasure(&store, &renewed, &current).unwrap(), transaction);
    assert!(!store.root().join("tables/second").exists());
    assert_eq!(recover_admitted_erasure(&store, &renewed, &current).unwrap(), transaction);
}
