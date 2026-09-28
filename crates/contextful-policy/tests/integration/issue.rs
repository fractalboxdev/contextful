//! `authority.issue`, credential side: the seed-file adapter and the mint.

use crate::support::*;
use contextful_core::grant::Action;
use contextful_core::issue::SignatureAlgorithm;
use contextful_core::ports::SigningPort;
use contextful_core::AuthorityError;
use contextful_policy::issue::{mint, MintClaims, SeedSigner, KEYGEN_COMMAND};
use contextful_policy::keyset::{KeySet, KeySource, StaticPins};
use contextful_policy::verify::{introspect, verify, Admission, AdmittedAuthority};
use std::cell::RefCell;

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
    let credential = mint(&p, &MintClaims { confirmation: Some("jkt-1".into()), epoch: 7 }, &signer).unwrap();
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
/// records every signature it returns, and in `raw` mode it answers ES256 in the fixed
/// `r || s` form rather than DER.
struct Custodian {
    key: SeedSigner,
    raw: bool,
    signatures: RefCell<Vec<Vec<u8>>>,
}

impl Custodian {
    fn new(algorithm: SignatureAlgorithm) -> Custodian {
        Custodian { key: SeedSigner::generate(algorithm), raw: false, signatures: RefCell::default() }
    }

    fn calls(&self) -> usize {
        self.signatures.borrow().len()
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
    fn algorithm(&self) -> SignatureAlgorithm {
        self.key.algorithm()
    }
    fn public_key(&self) -> Vec<u8> {
        self.key.public_key()
    }
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, AuthorityError> {
        let mut signature = self.key.sign(message)?;
        if self.raw && self.algorithm() == SignatureAlgorithm::Es256 {
            signature = p256::ecdsa::Signature::from_der(&signature).unwrap().to_bytes().to_vec();
        }
        self.signatures.borrow_mut().push(signature.clone());
        Ok(signature)
    }
}

fn read_research(port: &dyn SigningPort) -> contextful_core::issue::MintPlan {
    plan_for(port, dana(), vec![grant(&[Action::Read], &["research/*"])])
}

fn admit_under(credential: &str, custodian: &Custodian) -> Result<AdmittedAuthority, AuthorityError> {
    let revocation = no_revocation();
    verify(credential, &custodian.pin(), &Admission::new(at(DURING), &revocation).expecting(AUD))
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

/// Through the port, an Ed25519 key is 32 raw bytes signing 64; an ES256 key is a SEC1 P-256 point signing ECDSA over SHA-256 of the message, encoded as ASN.1 DER.
// spec: authority.issue.signature-encoding@d03c452a
#[test]
fn a_port_signature_is_raw_ed25519_or_der_es256_and_another_encoding_mints_nothing() {
    let ed25519 = Custodian::new(SignatureAlgorithm::Ed25519);
    mint(&read_research(&ed25519), &MintClaims::default(), &ed25519).unwrap();
    assert_eq!(ed25519.public_key().len(), 32);
    assert_eq!(ed25519.signatures.borrow()[0].len(), 64);

    let es256 = Custodian::new(SignatureAlgorithm::Es256);
    mint(&read_research(&es256), &MintClaims::default(), &es256).unwrap();
    assert!(p256::PublicKey::from_sec1_bytes(&es256.public_key()).is_ok());
    assert!(p256::ecdsa::Signature::from_der(&es256.signatures.borrow()[0]).is_ok());

    // A custodian answering in the fixed `r || s` form, which a checkpoint rejects, mints nothing.
    let raw = Custodian { raw: true, ..Custodian::new(SignatureAlgorithm::Es256) };
    refused(mint(&read_research(&raw), &MintClaims::default(), &raw), "IssuerKeyUnresolvable");
}
