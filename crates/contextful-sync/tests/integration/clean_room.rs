//! `disclosure.set-mode`: a cross-owner store opens only over an owner boundary the
//! object store enforces.

use contextful_core::issue::SignatureAlgorithm;
use contextful_core::store::object::{Condition, ObjectError, ObjectStore, Put};
use contextful_core::disclosure::DisclosureError;
use contextful_policy::issue::{SeedSigner, SignerKey};
use contextful_sync::clean_room::{open_cross_owner, write_owner_manifest, OwnerBoundary};
use contextful_sync::FsBucket;
use std::sync::Arc;

/// A bucket behind one owner's credential: reads reach every key, and the access policy
/// admits a write only under the listed prefixes.
struct PolicyBucket {
    inner: Arc<FsBucket>,
    writable: Vec<String>,
}

impl PolicyBucket {
    fn denies(&self, key: &str) -> Result<(), ObjectError> {
        if self.writable.iter().any(|p| key.starts_with(p.as_str())) {
            Ok(())
        } else {
            Err(ObjectError::Forbidden(format!("AccessDenied on `{key}`")))
        }
    }
}

impl ObjectStore for PolicyBucket {
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, String)>, ObjectError> {
        self.inner.get(key)
    }
    fn put(&self, key: &str, bytes: &[u8], condition: Condition) -> Result<Put, ObjectError> {
        self.denies(key)?;
        self.inner.put(key, bytes, condition)
    }
    fn delete(&self, key: &str) -> Result<(), ObjectError> {
        self.denies(key)?;
        self.inner.delete(key)
    }
    fn list(&self, prefix: &str) -> Result<Vec<String>, ObjectError> {
        self.inner.list(prefix)
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    bucket: Arc<FsBucket>,
    retailer: PolicyBucket,
    publisher: PolicyBucket,
    retailer_key: SeedSigner,
    publisher_key: SeedSigner,
}

const RETAILER: &str = "owners/retailer/";
const PUBLISHER: &str = "owners/publisher/";

/// Two owners, each signing its manifest subtree; `publisher_writes` lists the prefixes
/// the publisher's credential writes under.
fn fixture(publisher_writes: &[&str]) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let bucket = Arc::new(FsBucket::open(dir.path(), "shared").unwrap());
    let retailer = PolicyBucket { inner: bucket.clone(), writable: vec![RETAILER.into()] };
    let publisher = PolicyBucket { inner: bucket.clone(), writable: publisher_writes.iter().map(|p| p.to_string()).collect() };
    let retailer_key = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let publisher_key = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    write_owner_manifest(&retailer, RETAILER, "retailer", &retailer_key, b"{\"tables\":[\"orders\"]}").unwrap();
    write_owner_manifest(&publisher, PUBLISHER, "publisher", &publisher_key, b"{\"tables\":[\"visits\"]}").unwrap();
    Fixture { _dir: dir, bucket, retailer, publisher, retailer_key, publisher_key }
}

impl Fixture {
    fn owners(&self) -> Vec<OwnerBoundary<'_>> {
        vec![
            OwnerBoundary { owner: "retailer".into(), prefix: RETAILER.into(), store: &self.retailer, signer: Some(SignerKey::of(&self.retailer_key)) },
            OwnerBoundary { owner: "publisher".into(), prefix: PUBLISHER.into(), store: &self.publisher, signer: Some(SignerKey::of(&self.publisher_key)) },
        ]
    }
}

fn unmet(result: Result<impl std::fmt::Debug, DisclosureError>, names: &str) {
    match result {
        Err(e @ DisclosureError::CleanRoomPreconditionUnmet(_)) => {
            assert_eq!(e.identifier(), "DisclosureCleanRoomPreconditionUnmet");
            assert!(e.to_string().contains(names), "{e} names `{names}`");
        }
        other => panic!("expected DisclosureCleanRoomPreconditionUnmet, got {other:?}"),
    }
}

/// A cross-owner store carries per-owner signed manifest subtrees, per-owner write prefixes enforced by the object store's access policy, and per-owner signing keys; one missing, checked before any cross-prefix write, raises `DisclosureCleanRoomPreconditionUnmet`.
// spec: disclosure.set-mode.clean-room-preconditions@fd741d72
#[test]
fn open_refuses_when_one_owners_prefix_is_writable_by_the_other() {
    // Every precondition holds: the store opens, and the probe lands no byte.
    let f = fixture(&[PUBLISHER]);
    let before = f.bucket.list("").unwrap();
    let store = open_cross_owner(&f.owners()).unwrap();
    assert_eq!(store.owners(), ["publisher", "retailer"]);
    assert_eq!(f.bucket.list("").unwrap(), before, "the open writes nothing");

    // The publisher's credential also writes under the retailer's prefix: the open refuses
    // before any cross-prefix write lands.
    let f = fixture(&[PUBLISHER, RETAILER]);
    let before = f.bucket.list("").unwrap();
    unmet(open_cross_owner(&f.owners()), "publisher");
    assert_eq!(f.bucket.list("").unwrap(), before, "the refused open writes nothing");

    // A whole-bucket credential is the same breach.
    let f = fixture(&[""]);
    unmet(open_cross_owner(&f.owners()), "write prefix");

    // A policy admitting the publisher's credential on one existing key under the
    // retailer's prefix is the same breach, though the rest of the prefix refuses it.
    let f = fixture(&[PUBLISHER, "owners/retailer/manifest.json"]);
    let before = f.bucket.list("").unwrap();
    let manifest = f.bucket.get(&format!("{RETAILER}manifest.json")).unwrap();
    unmet(open_cross_owner(&f.owners()), "publisher");
    assert_eq!(f.bucket.list("").unwrap(), before, "the refused open writes nothing");
    assert_eq!(f.bucket.get(&format!("{RETAILER}manifest.json")).unwrap(), manifest, "the probe overwrites nothing");
}

/// An owner with no signing key, or one sharing another owner's key, refuses.
#[test]
fn open_refuses_an_owner_without_its_own_signing_key() {
    let f = fixture(&[PUBLISHER]);
    let mut owners = f.owners();
    owners[1].signer = None;
    unmet(open_cross_owner(&owners), "signing key");

    let mut owners = f.owners();
    owners[1].signer = Some(SignerKey::of(&f.retailer_key));
    unmet(open_cross_owner(&owners), "signing key");

    // One ES256 key, presented as a compressed and as an uncompressed SEC1 point, is one key.
    let es256 = SeedSigner::generate(SignatureAlgorithm::Es256);
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    let point = p256::PublicKey::from_sec1_bytes(&SignerKey::of(&es256).public_key).unwrap();
    let encoded = |compress: bool| SignerKey { algorithm: SignatureAlgorithm::Es256, public_key: point.to_encoded_point(compress).as_bytes().to_vec() };
    assert_ne!(encoded(true).public_key, encoded(false).public_key);
    let mut owners = f.owners();
    owners[0].signer = Some(encoded(true));
    owners[1].signer = Some(encoded(false));
    unmet(open_cross_owner(&owners), "share one signing key");
}

/// A manifest subtree absent, unsigned, or signed under another owner's key refuses.
#[test]
fn open_refuses_a_manifest_subtree_its_owner_did_not_sign() {
    // Signed by the retailer's key, presented as the publisher's.
    let f = fixture(&[PUBLISHER]);
    write_owner_manifest(&f.publisher, PUBLISHER, "publisher", &f.retailer_key, b"{}").unwrap();
    unmet(open_cross_owner(&f.owners()), "manifest");

    // Altered after signing.
    let f = fixture(&[PUBLISHER]);
    f.publisher.put(&format!("{PUBLISHER}manifest.json"), b"{\"tables\":[]}", Condition::None).unwrap();
    unmet(open_cross_owner(&f.owners()), "manifest");

    // Absent.
    let f = fixture(&[PUBLISHER]);
    f.publisher.delete(&format!("{PUBLISHER}manifest.sig")).unwrap();
    unmet(open_cross_owner(&f.owners()), "manifest");
}

/// Overlapping prefixes and a single owner are no per-owner boundary.
#[test]
fn open_refuses_overlapping_prefixes_and_a_lone_owner() {
    let f = fixture(&[PUBLISHER]);
    let mut owners = f.owners();
    owners[1].prefix = "owners/".into();
    unmet(open_cross_owner(&owners), "prefix");

    let owners = f.owners().into_iter().take(1).collect::<Vec<_>>();
    unmet(open_cross_owner(&owners), "two owners");

    // One owner named twice, each entry with its own key, prefix and signed manifest, is
    // one owner: the second credential writes under the first prefix, and the open refuses.
    let f = fixture(&[PUBLISHER, RETAILER]);
    write_owner_manifest(&f.publisher, PUBLISHER, "retailer", &f.publisher_key, b"{}").unwrap();
    let mut owners = f.owners();
    owners[1].owner = "retailer".into();
    unmet(open_cross_owner(&owners), "retailer");
    unmet(open_cross_owner(&owners), "names owner");
}
