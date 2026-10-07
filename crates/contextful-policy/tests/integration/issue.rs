//! `authority.issue`, credential side: the seed-file adapter and the mint.

use crate::support::*;
use contextful_core::grant::Action;
use contextful_core::issue::{SignatureAlgorithm, SignatureEncoding};
use contextful_core::ports::SigningPort;
use contextful_core::AuthorityError;
use contextful_policy::audit::{verify_signed, AuditError, AuditLog, SignedRoot, SignedTip, AUDIT_SEGMENT_ENTRIES};
use contextful_policy::issue::{mint, MintClaims, SeedSigner, SignerKey, KEYGEN_COMMAND};
use contextful_policy::keyset::{KeySet, KeySource, StaticPins};
use contextful_policy::verify::{introspect, verify_inherited_pipe, Admission, AdmittedAuthority, BiscuitFormat, CredentialFormat};
use std::sync::Mutex;

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("contextful-policy-issue-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A set issuer key reference that resolves to no material raises `IssuerKeyUnresolvable`; no local key is fabricated.
// spec: authority.issue.unresolvable-key@763c7765
#[test]
fn an_issuer_key_reference_resolving_to_nothing_is_refused_and_fabricates_no_key() {
    let dir = scratch("unresolvable");
    let absent = dir.join("issuer.seed");
    refused(SeedSigner::resolve(Some(&absent)), "IssuerKeyUnresolvable");
    assert!(!absent.exists(), "no key is written in its place");
    let garbage = dir.join("garbage.seed");
    std::fs::write(&garbage, "not a key\n").unwrap();
    refused(SeedSigner::resolve(Some(&garbage)), "IssuerKeyUnresolvable");
    assert_eq!(std::fs::read_to_string(&garbage).unwrap(), "not a key\n");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
}

#[test]
fn no_configured_issuer_key_names_the_command_that_writes_one() {
    let err = SeedSigner::resolve(None).unwrap_err();
    assert_eq!(err_name(&err), "IssuerKeyMissing");
    assert!(err.to_string().contains(KEYGEN_COMMAND), "{err}");
}

#[test]
fn a_seed_file_round_trips_and_its_public_key_is_a_static_pin() {
    for algorithm in [SignatureAlgorithm::Ed25519, SignatureAlgorithm::Es256] {
        let signer = SeedSigner::generate(algorithm);
        let dir = scratch(&format!("seed-{algorithm}"));
        let path = dir.join("issuer.seed");
        std::fs::write(&path, format!("{}\n", signer.seed())).unwrap();
        let loaded = SeedSigner::resolve(Some(&path)).unwrap();
        assert_eq!(loaded.public_key_text(), signer.public_key_text());
        assert_eq!(loaded.algorithm(), algorithm);
        let pins = StaticPins::parse(&signer.public_key_text()).unwrap();
        assert_eq!(pins.keys().unwrap().keys().next().unwrap().algorithm(), algorithm);
        // A credential minted under either scheme admits under its pin.
        let credential = mint(&plan(&loaded), &MintClaims::default(), &loaded).unwrap();
        assert_eq!(introspect(&credential).unwrap().authority.alg, algorithm.to_string());
        assert!(admit(&credential, &signer, DURING).is_ok());
    }
}

#[test]
fn a_mint_carries_the_plan_the_policy_checked() {
    let signer = issuer();
    let p = plan(&signer);
    let credential = mint(&p, &MintClaims { confirmation: Some("jkt-1".into()), epoch: 7, ..MintClaims::default() }, &signer).unwrap();
    let block = introspect(&credential).unwrap().authority;
    assert_eq!(block.aud, AUD);
    assert_eq!(block.iss, AUD);
    assert_eq!(block.iat, at(MINTED).unix_secs());
    assert_eq!(block.exp, at(MINTED).unix_secs() + TTL as i64);
    assert_eq!(block.sub, dana());
    assert_eq!(block.att, p.subject.clone().normalize().attestations());
    assert_eq!(block.grants, p.grants);
    assert_eq!(block.cnf.unwrap().jkt, "jkt-1");
    assert_eq!(block.rev.epoch, 7);
    assert_eq!(block.rev.id, format!("rev://{}", block.jti));
    // Every mint names a fresh credential identifier.
    assert_ne!(introspect(&mint(&p, &MintClaims::default(), &signer).unwrap()).unwrap().authority.jti, block.jti);
}

#[test]
fn a_plan_naming_another_scheme_than_the_signing_key_mints_nothing() {
    let signer = issuer();
    let mut p = plan(&signer);
    p.algorithm = SignatureAlgorithm::Es256;
    refused(mint(&p, &MintClaims::default(), &signer), "SignatureAlgorithmMismatch");
}

/// A custodian reachable only through the signing port: its key never leaves it, it
/// records every signature it returns, and it names one encoding tag while answering in
/// another encoding when a test mislabels it.
struct Custodian {
    key: SeedSigner,
    tag: SignatureEncoding,
    answer: SignatureEncoding,
    signatures: Mutex<Vec<Vec<u8>>>,
}

impl Custodian {
    fn new(algorithm: SignatureAlgorithm) -> Custodian {
        let native = SignatureEncoding::canonical(algorithm);
        Custodian::tagged(native, native)
    }

    /// A custodian naming `tag` and answering in `answer`.
    fn tagged(tag: SignatureEncoding, answer: SignatureEncoding) -> Custodian {
        Custodian { key: SeedSigner::generate(tag.scheme()), tag, answer, signatures: Mutex::default() }
    }

    fn calls(&self) -> usize {
        self.signatures.lock().unwrap().len()
    }

    /// The static pin a checkpoint reads, built from the port's public key alone.
    fn pin(&self) -> KeySet {
        let scheme = match self.algorithm() {
            SignatureAlgorithm::Ed25519 => "ed25519",
            SignatureAlgorithm::Es256 => "secp256r1",
        };
        let pins = StaticPins::parse(&format!("{scheme}/{}", hex::encode(self.public_key()))).unwrap();
        (*pins.keys().unwrap()).clone()
    }
}

impl SigningPort for Custodian {
    fn encoding(&self) -> SignatureEncoding {
        self.tag
    }
    fn public_key(&self) -> Vec<u8> {
        self.key.public_key()
    }
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, AuthorityError> {
        let mut signature = self.key.sign(message)?;
        if self.answer == SignatureEncoding::Es256Raw {
            signature = p256::ecdsa::Signature::from_der(&signature).unwrap().to_bytes().to_vec();
        }
        self.signatures.lock().unwrap().push(signature.clone());
        Ok(signature)
    }
}

fn read_research(port: &dyn SigningPort) -> contextful_core::issue::MintPlan {
    plan_for(port, dana(), vec![grant(&[Action::Read], &["research/*"])])
}

fn admit_under(credential: &str, custodian: &Custodian) -> Result<AdmittedAuthority, AuthorityError> {
    let revocation = no_revocation();
    verify_inherited_pipe(credential, &custodian.pin(), &Admission::new(at(DURING), &revocation).expecting(AUD))
}

/// One signing port signs every credential's authority block and every audit root and tip; no mint path reads a private key. Seed files, secret references resolved at mint time and remote signing oracles are its adapters.
// spec: authority.issue.signing-port@2b15aa2a
#[test]
fn a_mint_signs_through_the_port_and_admits_under_the_ports_public_key() {
    for algorithm in [SignatureAlgorithm::Ed25519, SignatureAlgorithm::Es256] {
        let custodian = Custodian::new(algorithm);
        let credential = mint(&read_research(&custodian), &MintClaims::default(), &custodian).unwrap();
        assert_eq!(introspect(&credential).unwrap().authority.alg, algorithm.to_string());
        assert!(admit_under(&credential, &custodian).is_ok(), "{algorithm} admits under the port's key");
        // The build key is discarded: another key of the same scheme admits nothing.
        refused(admit_under(&credential, &Custodian::new(algorithm)), "SignatureInvalid");
        // The format interface mints through the same port.
        let format: &dyn CredentialFormat = &BiscuitFormat;
        let issued = format.issue(&read_research(&custodian), &MintClaims::default(), &custodian).unwrap();
        assert!(admit_under(&issued, &custodian).is_ok(), "{algorithm} mints through the format interface");
    }
    // The port's scheme is authoritative, and a mismatch reaches no signing call.
    let ed25519 = Custodian::new(SignatureAlgorithm::Ed25519);
    let mut p = read_research(&ed25519);
    p.algorithm = SignatureAlgorithm::Es256;
    refused(mint(&p, &MintClaims::default(), &ed25519), "SignatureAlgorithmMismatch");
    assert_eq!(ed25519.calls(), 0);
}

/// Under the oracle adapter the seed stays inside the custodian, which records each mint as a signing call.
// spec: authority.issue.oracle-custody@a8ed7d90
#[test]
fn the_custodian_records_one_signing_call_per_mint() {
    let custodian = Custodian::new(SignatureAlgorithm::Es256);
    let p = read_research(&custodian);
    for n in 1..=3 {
        mint(&p, &MintClaims::default(), &custodian).unwrap();
        assert_eq!(custodian.calls(), n);
    }
}

/// A signing port names its encoding: `ed25519`, a 32-byte key and 64-byte signature; `es256-der` or `es256-raw`, a SEC1 P-256 point signing ECDSA over SHA-256 of the message as ASN.1 DER or 64-byte `r ‖ s`.
// spec: authority.issue.signature-encoding@f2216099
#[test]
fn a_port_names_its_encoding_and_every_tag_mints_a_credential_that_admits() {
    for tag in [SignatureEncoding::Ed25519, SignatureEncoding::Es256Der, SignatureEncoding::Es256Raw] {
        assert_eq!(SignatureEncoding::parse(&tag.to_string()), Some(tag));
        let custodian = Custodian::tagged(tag, tag);
        assert_eq!(custodian.algorithm(), tag.scheme());
        let credential = mint(&read_research(&custodian), &MintClaims::default(), &custodian).unwrap();
        assert!(admit_under(&credential, &custodian).is_ok(), "{tag} admits");
        let (key, signature) = (custodian.public_key(), custodian.signatures.lock().unwrap()[0].clone());
        match tag {
            SignatureEncoding::Ed25519 => assert_eq!((key.len(), signature.len()), (32, 64)),
            SignatureEncoding::Es256Der => {
                assert!(p256::PublicKey::from_sec1_bytes(&key).is_ok());
                assert!(p256::ecdsa::Signature::from_der(&signature).is_ok());
            }
            SignatureEncoding::Es256Raw => {
                assert!(p256::PublicKey::from_sec1_bytes(&key).is_ok());
                assert_eq!(signature.len(), 64);
            }
        }
    }
    assert_eq!(SignatureEncoding::Ed25519.to_string(), "ed25519");
    assert_eq!(SignatureEncoding::Es256Der.to_string(), "es256-der");
    assert_eq!(SignatureEncoding::Es256Raw.to_string(), "es256-raw");
    assert_eq!(SignatureEncoding::parse("es256"), None);
    assert_eq!(SignatureEncoding::Es256Raw.scheme(), SignatureAlgorithm::Es256);
}

/// The authority block's signature bytes inside an encoded credential.
fn authority_signature(credential: &str) -> Vec<u8> {
    use base64::Engine;
    use prost::Message;
    let bytes = base64::engine::general_purpose::URL_SAFE.decode(credential).unwrap();
    biscuit_auth::format::schema::Biscuit::decode(bytes.as_slice()).unwrap().authority.signature
}

/// An `es256-raw` signature converts to DER where it leaves the port; a credential and an audit root or tip carry ES256 only as DER.
// spec: authority.issue.der-at-the-edge@3426c5ab
#[test]
fn an_es256_raw_signature_leaves_the_port_as_der_in_the_credential_and_the_audit_chain() {
    let raw = Custodian::tagged(SignatureEncoding::Es256Raw, SignatureEncoding::Es256Raw);
    let credential = mint(&read_research(&raw), &MintClaims::default(), &raw).unwrap();
    let stored = authority_signature(&credential);
    assert!(p256::ecdsa::Signature::from_der(&stored).is_ok(), "the credential carries DER");
    assert_ne!(stored, raw.signatures.lock().unwrap()[0], "the port answered r || s");

    let dir = tempfile::tempdir().unwrap();
    let key = SignerKey::of(&raw);
    let log = AuditLog::open(dir.path(), raw).unwrap();
    log.append_all((0..AUDIT_SEGMENT_ENTRIES).map(|i| serde_json::json!({ "n": i })).collect()).unwrap();
    drop(log);
    let root: SignedRoot = serde_json::from_str(&std::fs::read_to_string(dir.path().join("segments/000001.root.json")).unwrap()).unwrap();
    let tip: SignedTip = serde_json::from_str(&std::fs::read_to_string(dir.path().join("chain.tip")).unwrap()).unwrap();
    for signature in [root.signature, tip.signature.unwrap()] {
        assert!(p256::ecdsa::Signature::from_der(&hex::decode(signature).unwrap()).is_ok(), "the chain carries DER");
    }
    assert_eq!(verify_signed(dir.path(), &key).unwrap().seq, AUDIT_SEGMENT_ENTRIES);
}

/// A port answering Ed25519 under the key of another port.
struct Impostor {
    named: SeedSigner,
    signing: SeedSigner,
}

impl SigningPort for Impostor {
    fn encoding(&self) -> SignatureEncoding {
        SignatureEncoding::Ed25519
    }
    fn public_key(&self) -> Vec<u8> {
        self.named.public_key()
    }
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, AuthorityError> {
        self.signing.sign(message)
    }
}

/// A port signature that does not decode under its encoding tag, or does not verify under the port's public key, raises `SignatureEncodingInvalid`; nothing is minted and no root or tip is written.
// spec: authority.issue.encoding-invalid@7d06e4f1
#[test]
fn a_signature_off_its_tag_or_its_key_raises_signature_encoding_invalid() {
    refuses(Custodian::tagged(SignatureEncoding::Es256Raw, SignatureEncoding::Es256Der));
    refuses(Custodian::tagged(SignatureEncoding::Es256Der, SignatureEncoding::Es256Raw));
    refuses(Impostor { named: SeedSigner::generate(SignatureAlgorithm::Ed25519), signing: SeedSigner::generate(SignatureAlgorithm::Ed25519) });
}

/// `port` mints nothing, and a log opened through it writes no tip.
fn refuses<P: SigningPort + Send + Sync + 'static>(port: P) {
    refused(mint(&read_research(&port), &MintClaims::default(), &port), "SignatureEncodingInvalid");
    let dir = tempfile::tempdir().unwrap();
    match AuditLog::open(dir.path(), port) {
        Err(AuditError::AuditEntryUnpersisted(why)) => assert!(why.contains("SignatureEncodingInvalid"), "{why}"),
        other => panic!("expected the tip to refuse, got {other:?}"),
    }
    assert!(!dir.path().join("chain.tip").exists(), "no tip is written");
}

/// A port's public key prints as `ed25519:<hex>` or `es256:<hex>` of its SEC1 point; either form, with a compressed or uncompressed point, parses back to the key it names.
// spec: authority.issue.public-key-text@5eaac8b5
#[test]
fn a_public_key_prints_under_its_scheme_tag_and_parses_back() {
    let ed25519 = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let text = SignerKey::of(&ed25519).to_string();
    assert_eq!(text, format!("ed25519:{}", hex::encode(ed25519.public_key())));
    assert_eq!(text.parse::<SignerKey>().unwrap(), SignerKey::of(&ed25519));

    let es256 = SeedSigner::generate(SignatureAlgorithm::Es256);
    let text = SignerKey::of(&es256).to_string();
    assert_eq!(text, format!("es256:{}", hex::encode(es256.public_key())));
    let point = p256::PublicKey::from_sec1_bytes(&es256.public_key()).unwrap();
    let signature = es256.sign(b"message").unwrap();
    for compress in [true, false] {
        use p256::elliptic_curve::sec1::ToEncodedPoint;
        let text = format!("es256:{}", hex::encode(point.to_encoded_point(compress).as_bytes()));
        let key: SignerKey = text.parse().unwrap();
        assert_eq!(key.algorithm, SignatureAlgorithm::Es256);
        assert!(key.verifies(b"message", &signature), "compressed: {compress}");
    }
    for bad in ["es256:zz", "rsa:00", "ed25519:0011", "es256:0011", "ed25519"] {
        assert!(bad.parse::<SignerKey>().is_err(), "{bad}");
    }
}
