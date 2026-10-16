//! A tenant purge receipt: its salted pseudonym, its versioned coverage claim and its freshness.

use contextful_core::disclosure::erase::ErasureError;
use contextful_core::issue::SignatureAlgorithm;
use contextful_policy::issue::{SeedSigner, SignerKey};
use contextful_policy::receipt::{Coverage, ReceiptClaim, TenantReceipt, RECEIPT_VERSION};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

fn claim() -> ReceiptClaim {
    ReceiptClaim {
        request_id: "11".repeat(32),
        transaction_id: "22".repeat(32),
        store_id: "6f1c1f0e-8a52-4d7e-9a43-2b1f0f3c9d10".into(),
        affected_counts: BTreeMap::from([("orders".to_string(), 2), ("events".to_string(), 1)]),
        coverage: Coverage::of_version(RECEIPT_VERSION).unwrap(),
        issued_at: "2030-01-01T00:05:00+00:00".into(),
    }
}

/// `tenant_hash` is SHA-256 over a random per-receipt `salt` and the tenant identifier. A receipt carrying the identifier in cleartext raises `ReceiptIdentifierLeak` before signing.
// spec: disclosure.receipt.pseudonym@bfa6865a
#[test]
fn the_tenant_hash_is_salted_per_receipt_and_a_cleartext_identifier_refuses_before_signing() {
    for algorithm in [SignatureAlgorithm::Ed25519, SignatureAlgorithm::Es256] {
        let signer = SeedSigner::generate(algorithm);
        let trusted = [SignerKey::of(&signer)];
        let first = TenantReceipt::sign(claim(), "acme-tenant", &signer).unwrap();
        let salt = hex::decode(&first.salt).unwrap();
        assert_eq!(salt.len(), 32);
        let mut preimage = salt.clone();
        preimage.extend_from_slice(b"acme-tenant");
        assert_eq!(first.tenant_hash, format!("sha256:{}", hex::encode(Sha256::digest(&preimage))));
        assert!(!serde_json::to_string(&first).unwrap().contains("acme-tenant"));
        first.verify("acme-tenant", &trusted).unwrap();
        assert!(first.verify("other-tenant", &trusted).is_err(), "a receipt verifies for a tenant it does not name");
        assert!(first.verify("acme-tenant", &[SignerKey::of(&SeedSigner::generate(algorithm))]).is_err());
        let second = TenantReceipt::sign(claim(), "acme-tenant", &signer).unwrap();
        assert_ne!(first.salt, second.salt, "two receipts share a salt");
        assert_ne!(first.tenant_hash, second.tenant_hash, "two receipts link through one tenant hash");
        let mut altered = first.clone();
        altered.affected_counts.insert("orders".into(), 9);
        assert!(altered.verify("acme-tenant", &trusted).is_err(), "an altered claim verifies");
    }
    let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let mut leaking = claim();
    leaking.affected_counts.insert("acme-tenant".into(), 1);
    assert_eq!(TenantReceipt::sign(leaking, "acme-tenant", &signer).unwrap_err(), ErasureError::ReceiptIdentifierLeak);
    let mut leaking = claim();
    leaking.issued_at = "acme-tenant".into();
    assert_eq!(TenantReceipt::screen(&leaking, "acme-tenant", &signer).unwrap_err(), ErasureError::ReceiptIdentifierLeak);
    assert_eq!(TenantReceipt::sign(leaking, "acme-tenant", &signer).unwrap_err(), ErasureError::ReceiptIdentifierLeak);
    for short in ["1", "2030", "10", "ed25519", "store", "id", "canonical", "order", "receipt_version"] {
        TenantReceipt::screen(&claim(), short, &signer).unwrap();
        TenantReceipt::sign(claim(), short, &signer).unwrap().verify(short, &[SignerKey::of(&signer)])
            .unwrap_or_else(|error| panic!("tenant `{short}` collides with an engine-formatted field: {error}"));
    }
}

/// A coverage block naming a scope broader than its `receipt_version` declares raises `ReceiptClaimWidened`.
// spec: disclosure.receipt.widened-claim@6f780a3f
#[test]
fn a_coverage_block_broader_than_its_version_raises_receipt_claim_widened() {
    let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let declared = Coverage::of_version(RECEIPT_VERSION).unwrap();
    let widen = |coverage: Coverage| {
        let mut widened = claim();
        widened.coverage = coverage;
        TenantReceipt::sign(widened, "acme-tenant", &signer).unwrap_err()
    };
    let mut covered = declared.clone();
    covered.covered.push("replication-bucket".into());
    covered.excluded.retain(|scope| scope != "replication-bucket");
    assert!(matches!(widen(covered), ErasureError::ReceiptClaimWidened(_)));
    let mut silent = declared.clone();
    silent.excluded.retain(|scope| scope != "replicas");
    assert!(matches!(widen(silent), ErasureError::ReceiptClaimWidened(_)), "dropping an exclusion widens the claim");
    let mut unknown = claim();
    unknown.coverage = declared.clone();
    let mut receipt = TenantReceipt::sign(unknown, "acme-tenant", &signer).unwrap();
    receipt.receipt_version = 2;
    assert!(receipt.verify("acme-tenant", &[SignerKey::of(&signer)]).is_err(), "an undeclared version verifies");
    assert!(Coverage::of_version(2).is_none());
}

/// Returning the byte-identical earlier artifact in place of a freshly signed one raises `ReceiptReplayed`.
// spec: disclosure.receipt.replayed@40358abe
#[test]
fn returning_an_earlier_artifact_byte_for_byte_raises_receipt_replayed() {
    let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let earlier = TenantReceipt::sign(claim(), "acme-tenant", &signer).unwrap();
    let fresh = TenantReceipt::sign(claim(), "acme-tenant", &signer).unwrap();
    let issued = [earlier.digest()];
    assert_eq!(TenantReceipt::check_fresh(&earlier, &issued).unwrap_err(), ErasureError::ReceiptReplayed);
    TenantReceipt::check_fresh(&fresh, &issued).unwrap();
    assert_ne!(earlier.digest(), fresh.digest());
}

/// A `receipt_version` 1 coverage block covers the canonical store and names replicas and the replication bucket as excluded.
// spec: disclosure.receipt.coverage@db393bb7
#[test]
fn a_version_one_receipt_covers_the_canonical_store_and_excludes_replicas_and_the_bucket() {
    assert_eq!(RECEIPT_VERSION, 1);
    let declared = Coverage::of_version(1).unwrap();
    assert_eq!(declared.covered, ["canonical-store"]);
    assert_eq!(declared.excluded, ["replicas", "replication-bucket"]);
    let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let receipt = TenantReceipt::sign(claim(), "acme-tenant", &signer).unwrap();
    assert_eq!(receipt.coverage, declared);
    assert_eq!(receipt.receipt_version, 1);
}
