//! A pulled control receipt binds the project, version, predecessor and snapshot bytes.

use contextful_core::issue::SignatureAlgorithm;
use contextful_policy::control_receipt::ControlReceipt;
use contextful_policy::issue::{SeedSigner, SignerKey};
use sha2::{Digest, Sha256};

/// A JSON control receipt carries `format: 1`, signer public key and signature over the
/// versioned UTF-8 message; its parent is `-` only for the first version.
// spec: surface.apply.receipt-message@67ff9279
#[test]
fn a_control_receipt_binds_every_signed_field_and_the_snapshot_bytes() {
    let snapshot = b"[pipeline]\nid = 'orders'\n";
    let digest = hex::encode(Sha256::digest(snapshot));
    for algorithm in [SignatureAlgorithm::Ed25519, SignatureAlgorithm::Es256] {
        let signer = SeedSigner::generate(algorithm);
        let trusted = [SignerKey::of(&signer)];
        let receipt = ControlReceipt::sign("research", 1, None, snapshot, &signer).unwrap();
        assert_eq!(receipt.format, 1);
        assert_eq!(receipt.message(), format!("contextful-control-v1\nresearch\n1\n-\n{digest}\n{}\n", receipt.signer).into_bytes());
        assert!(receipt.verify("research", snapshot, &trusted).is_ok());
        assert!(receipt.verify("other", snapshot, &trusted).is_err());
        assert!(receipt.verify("research", b"changed", &trusted).is_err());
        assert!(receipt.verify("research", snapshot, &[SignerKey::of(&SeedSigner::generate(algorithm))]).is_err());

        let mut changed = receipt.clone();
        changed.version = 2;
        assert!(changed.verify("research", snapshot, &trusted).is_err());
        changed = receipt.clone();
        changed.parent = Some("00".repeat(32));
        assert!(changed.verify("research", snapshot, &trusted).is_err());
        changed = receipt.clone();
        changed.snapshot_sha256 = "11".repeat(32);
        assert!(changed.verify("research", snapshot, &trusted).is_err());
        changed = receipt.clone();
        changed.project = "other".into();
        assert!(changed.verify("research", snapshot, &trusted).is_err());
        changed = receipt.clone();
        let (scheme, bytes) = changed.signer.split_once(':').unwrap();
        changed.signer = format!("{scheme}:{}", bytes.to_uppercase());
        assert!(changed.verify("research", snapshot, &trusted).is_err());
    }
}

#[test]
fn a_p256_receipt_accepts_an_equivalent_key_pin_in_either_point_encoding() {
    use p256::elliptic_curve::sec1::ToEncodedPoint;

    let signer = SeedSigner::generate(SignatureAlgorithm::Es256);
    let receipt = ControlReceipt::sign("research", 1, None, b"snapshot", &signer).unwrap();
    let point = p256::PublicKey::from_sec1_bytes(&SignerKey::of(&signer).public_key).unwrap();
    for compressed in [true, false] {
        let pin = SignerKey {
            algorithm: SignatureAlgorithm::Es256,
            public_key: point.to_encoded_point(compressed).as_bytes().to_vec(),
        };
        assert!(receipt.verify("research", b"snapshot", &[pin]).is_ok(), "compressed: {compressed}");
    }
}

/// A predecessor and the bucket head name the digest of canonical JSON receipt bytes.
// spec: surface.apply.receipt-digest@358e1146
#[test]
fn a_receipt_digest_uses_canonical_json_for_its_successor() {
    let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let first = ControlReceipt::sign("research", 1, None, b"one", &signer).unwrap();
    let canonical = serde_json_canonicalizer::to_vec(&first).unwrap();
    assert_eq!(first.digest(), hex::encode(Sha256::digest(canonical)));
    let second = ControlReceipt::sign("research", 2, Some(&first.digest()), b"two", &signer).unwrap();
    assert_eq!(second.parent.as_deref(), Some(first.digest().as_str()));
    assert!(second.verify("research", b"two", &[SignerKey::of(&signer)]).is_ok());
}
