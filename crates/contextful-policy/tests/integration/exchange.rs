//! `authority.exchange`: a signed external assertion, verified under operator-injected
//! material, traded for the mint plan of that reader's credential.
//!
//! The RSA key pairs under `tests/fixtures/exchange/` are test-only and sign nothing
//! outside this suite.

use contextful_core::exchange::VerifyingMaterial;
use contextful_core::issue::{IssuancePolicy, MintContext, MintPlan, NodeRole, SignatureEncoding};
use contextful_core::ports::{FixedClock, SigningPort};
use contextful_core::time::Instant;
use contextful_core::AuthorityError;
use contextful_policy::exchange::{material_from_bytes, verify_assertion, Exchange};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde_json::{json, Value};

const NOW: &str = "2030-01-01T00:00:00Z";
const SECRET: &[u8] = b"exchange-shared-secret";

const IDP_KEY: &str = include_str!("../fixtures/exchange/idp.key.pem");
const IDP_PUB: &str = include_str!("../fixtures/exchange/idp.pub.pem");
const OTHER_KEY: &str = include_str!("../fixtures/exchange/other.key.pem");
const OTHER_PUB: &str = include_str!("../fixtures/exchange/other.pub.pem");
const JWKS: &str = include_str!("../fixtures/exchange/jwks.json");

/// The exchange policy example in `spec/50-authority.md`.
const POLICY: &str = r#"
expected_iss   = "https://login.acme.example/"
expected_aud   = "contextful-console"
tenant_claim   = "org_id"
role_claim     = "roles"
default_grants = []
ttl_secs       = 900
minted_iss     = "contextful://acme-research"
minted_aud     = "contextful://acme-research"

[subject_map]
on_behalf_of = { claim = "sub", template = "user://{}" }

[[role_grants.analyst]]
actions   = ["read"]
tables    = ["research/*"]
templates = ["quarterly_rollup"]
"#;

struct FakeSigner;

impl SigningPort for FakeSigner {
    fn encoding(&self) -> SignatureEncoding {
        SignatureEncoding::Ed25519
    }
    fn public_key(&self) -> Vec<u8> {
        vec![7; 32]
    }
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, AuthorityError> {
        Ok(message.to_vec())
    }
}

fn now() -> Instant {
    Instant::parse(NOW).unwrap()
}

fn claims(sub: &str) -> Value {
    json!({
        "iss": "https://login.acme.example/",
        "aud": "contextful-console",
        "exp": now().unix_secs() + 600,
        "sub": sub,
        "org_id": "acme-eu",
        "roles": ["analyst"],
    })
}

fn hs256(claims: &Value, secret: &[u8]) -> String {
    encode(&Header::new(Algorithm::HS256), claims, &EncodingKey::from_secret(secret)).unwrap()
}

fn rs256(claims: &Value, key_pem: &str, kid: Option<&str>) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = kid.map(str::to_string);
    encode(&header, claims, &EncodingKey::from_rsa_pem(key_pem.as_bytes()).unwrap()).unwrap()
}

fn exchange(material: &[u8]) -> Exchange {
    Exchange::from_config(POLICY, material).unwrap()
}

fn plan(exchange: &Exchange, assertion: &str) -> Result<MintPlan, AuthorityError> {
    let issuance =
        IssuancePolicy::parse("default_audience = \"contextful://acme-research\"\nmax_lifetime_secs = 3600\n").unwrap();
    let clock = FixedClock(now());
    let ctx = MintContext { node: NodeRole::Primary, signer: &FakeSigner, clock: &clock };
    exchange.plan(assertion, &issuance, &ctx)
}

fn assert_invalid(result: Result<MintPlan, AuthorityError>, why: &str) {
    match result {
        Err(e @ AuthorityError::ExchangeAssertionInvalid(_)) => {
            assert!(e.to_string().starts_with("ExchangeAssertionInvalid"), "{why}: {e}")
        }
        other => panic!("{why}: expected ExchangeAssertionInvalid, got {other:?}"),
    }
}

/// Verifying material is operator-injected: a shared secret, an RS256 public key in PEM form, or a key-set document selected by the assertion's `kid`. The exchange makes no network call.
// spec: authority.exchange.injected-material@8c3b1692
#[test]
fn verifying_material_is_a_secret_a_pem_key_or_a_kid_selected_key_set() {
    // The injected bytes classify into the three forms; empty bytes are no material.
    assert_eq!(material_from_bytes(SECRET), Some(VerifyingMaterial::SharedSecret(SECRET.to_vec())));
    assert_eq!(material_from_bytes(IDP_PUB.as_bytes()), Some(VerifyingMaterial::Rs256PublicKeyPem(IDP_PUB.into())));
    assert_eq!(material_from_bytes(JWKS.as_bytes()), Some(VerifyingMaterial::KeySet(JWKS.into())));
    assert_eq!(material_from_bytes(b"exchange-shared-secret\n"), Some(VerifyingMaterial::SharedSecret(SECRET.to_vec())));
    for empty in [&b""[..], b"\n", b"  \t\n"] {
        assert_eq!(material_from_bytes(empty), None);
    }

    let dana = claims("dana@acme.example");

    // A shared secret verifies HS256.
    let secret = exchange(SECRET);
    let p = plan(&secret, &hs256(&dana, SECRET)).unwrap();
    assert_eq!(p.subject.on_behalf_of.as_deref(), Some("user://dana@acme.example"));
    assert_invalid(plan(&secret, &hs256(&dana, b"another-secret")), "HS256 under another secret");
    // A secret never verifies an RS256 assertion, nor a PEM key an HS256 one.
    assert_invalid(plan(&secret, &rs256(&dana, IDP_KEY, None)), "RS256 against a shared secret");

    // An RS256 public key in PEM form.
    let pem = exchange(IDP_PUB.as_bytes());
    assert!(plan(&pem, &rs256(&dana, IDP_KEY, None)).is_ok());
    assert_invalid(plan(&pem, &rs256(&dana, OTHER_KEY, None)), "RS256 under another key");
    assert_invalid(plan(&pem, &hs256(&dana, IDP_PUB.as_bytes())), "HS256 keyed by the public PEM");
    assert!(plan(&exchange(OTHER_PUB.as_bytes()), &rs256(&dana, OTHER_KEY, None)).is_ok());

    // A key-set document: the assertion's `kid` selects the key.
    let set = exchange(JWKS.as_bytes());
    assert!(plan(&set, &rs256(&dana, IDP_KEY, Some("idp-2030"))).is_ok());
    assert!(plan(&set, &rs256(&dana, OTHER_KEY, Some("other-2030"))).is_ok());
    assert_invalid(plan(&set, &rs256(&dana, IDP_KEY, Some("other-2030"))), "kid selecting another key");
    assert_invalid(plan(&set, &rs256(&dana, IDP_KEY, Some("retired-2029"))), "kid absent from the set");
    assert_invalid(plan(&set, &rs256(&dana, IDP_KEY, None)), "no kid");

    // No network call: a header pointing at a remote key set changes nothing, and an
    // unreachable address answers at once with the refusal.
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("remote-only".into());
    header.jku = Some("http://127.0.0.1:9/.well-known/jwks.json".into());
    let remote = encode(&header, &dana, &EncodingKey::from_rsa_pem(IDP_KEY.as_bytes()).unwrap()).unwrap();
    assert_invalid(plan(&set, &remote), "kid only a remote key set holds");
    header.kid = Some("idp-2030".into());
    let local = encode(&header, &dana, &EncodingKey::from_rsa_pem(IDP_KEY.as_bytes()).unwrap()).unwrap();
    assert!(plan(&set, &local).is_ok());

    // Verification alone hands back the verified claims.
    let verified = verify_assertion(Some(&VerifyingMaterial::SharedSecret(SECRET.to_vec())), &hs256(&dana, SECRET)).unwrap();
    assert_eq!(verified.get("sub"), Some(&json!("dana@acme.example")));
}

/// A bad signature, a lapsed assertion, an untrusted issuer or audience, or a mapped claim absent from the assertion raises `ExchangeAssertionInvalid` and mints nothing.
// spec: authority.exchange.assertion-invalid@202f428c
#[test]
fn a_signed_assertion_failing_any_check_is_invalid_and_mints_nothing() {
    let x = exchange(IDP_PUB.as_bytes());
    let good = claims("dana@acme.example");
    assert!(plan(&x, &rs256(&good, IDP_KEY, None)).is_ok());

    let with = |key: &str, value: Value| {
        let mut c = good.clone();
        if value.is_null() {
            c.as_object_mut().unwrap().remove(key);
        } else {
            c[key] = value;
        }
        rs256(&c, IDP_KEY, None)
    };

    // A bad signature: another key, a tampered payload, a stripped signature, garbage.
    let token = rs256(&good, IDP_KEY, None);
    let parts: Vec<&str> = token.split('.').collect();
    let forged_payload = with("sub", json!("mallory@acme.example"));
    let tampered = format!("{}.{}.{}", parts[0], forged_payload.split('.').nth(1).unwrap(), parts[2]);
    // `{"alg":"none","typ":"JWT"}`, base64url-encoded.
    let alg_none = format!("eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.{}.", parts[1]);
    for (why, bad) in [
        ("signed by another key", rs256(&good, OTHER_KEY, None)),
        ("payload swapped under the original signature", tampered),
        ("signature stripped", format!("{}.{}.", parts[0], parts[1])),
        ("alg none", alg_none),
        ("not a JWT", "not-a-jwt".to_string()),
    ] {
        assert_invalid(plan(&x, &bad), why);
    }

    // Lapsed, untrusted issuer or audience, mapped claim absent: all under a good signature.
    let lapsed = now().unix_secs();
    for (why, bad) in [
        ("lapsed at the evaluation instant", with("exp", json!(lapsed))),
        ("lapsed", with("exp", json!(lapsed - 3600))),
        ("no expiry", with("exp", Value::Null)),
        ("untrusted issuer", with("iss", json!("https://evil.example/"))),
        ("untrusted audience", with("aud", json!("another-app"))),
        ("mapped subject claim absent", with("sub", Value::Null)),
        ("tenant claim absent", with("org_id", Value::Null)),
    ] {
        assert_invalid(plan(&x, &bad), why);
    }

    // The expiry is judged at the mint clock's instant, not the host's wall clock: an
    // assertion expiring in 2030 is good at the fixed 2030 instant only while unexpired.
    assert!(plan(&x, &with("exp", json!(lapsed + 1))).is_ok());
}

/// Material absent or unusable refuses before any claim is read.
#[test]
fn an_exchange_without_usable_material_mints_nothing() {
    let dana = claims("dana@acme.example");
    for material in [&b""[..], b"\n  \n"] {
        let x = exchange(material);
        let err = plan(&x, &hs256(&dana, SECRET)).unwrap_err();
        assert!(matches!(err, AuthorityError::ExchangeMaterialMissing(_)), "{err}");
    }
    assert!(matches!(
        verify_assertion(None, &hs256(&dana, SECRET)),
        Err(AuthorityError::ExchangeMaterialMissing(_))
    ));
    // A PEM block or key-set document that holds no usable key is no material either.
    for broken in ["-----BEGIN PUBLIC KEY-----\nAAAA\n-----END PUBLIC KEY-----\n", "{\"keys\": [{\"kty\": \"RSA\"}]}"] {
        let err = plan(&exchange(broken.as_bytes()), &rs256(&dana, IDP_KEY, Some("idp-2030"))).unwrap_err();
        assert!(matches!(err, AuthorityError::ExchangeMaterialMissing(_)), "{broken}: {err}");
    }
}

/// An embedding application holds no project-wide credential; per request it exchanges its signed-in reader's assertion for that reader's short-lived credential and reads as them.
// spec: authority.exchange.per-reader@bf47cebb
#[test]
fn each_readers_assertion_mints_that_readers_short_lived_credential() {
    // The application holds material and policy only; each request carries one reader's
    // assertion and earns a plan for that reader alone.
    let x = exchange(JWKS.as_bytes());
    for reader in ["dana@acme.example", "eli@acme.example", "fox@acme.example"] {
        let p = plan(&x, &rs256(&claims(reader), IDP_KEY, Some("idp-2030"))).unwrap();
        assert_eq!(p.subject.on_behalf_of, Some(format!("user://{reader}")));
        assert_eq!(p.issued_at, now());
        assert_eq!(p.lifetime_secs, 900);
        assert!(p.lifetime_secs <= contextful_core::exchange::EXCHANGE_LIFETIME_CEILING_SECS);
        assert_eq!(p.expires_at, now().plus_secs(900));
    }
    // With no reader assertion there is nothing to exchange.
    assert_invalid(plan(&x, ""), "no assertion");
}

/// An exchanged credential serves only the reader it was minted for.
// spec: authority.exchange.no-cross-reader@8a399e5d
#[test]
fn a_plan_for_one_reader_never_carries_another_readers_principal() {
    let x = exchange(SECRET);
    let a = plan(&x, &hs256(&claims("dana@acme.example"), SECRET)).unwrap();
    let b = plan(&x, &hs256(&claims("eli@acme.example"), SECRET)).unwrap();

    assert_ne!(a.subject, b.subject);
    assert_eq!(a.subject.on_behalf_of.as_deref(), Some("user://dana@acme.example"));
    assert_eq!(b.subject.on_behalf_of.as_deref(), Some("user://eli@acme.example"));
    let a_json = format!("{a:?}");
    let b_json = format!("{b:?}");
    assert!(!a_json.contains("eli@acme.example"), "{a_json}");
    assert!(!b_json.contains("dana@acme.example"), "{b_json}");

    // Replaying reader A's assertion earns A's subject again, never B's.
    let again = plan(&x, &hs256(&claims("dana@acme.example"), SECRET)).unwrap();
    assert_eq!(again.subject, a.subject);
}
