//! `authority.exchange`: a signed external assertion, verified under operator-injected
//! material, traded for the mint plan of that reader's credential.
//!
//! The RSA key pairs under `tests/fixtures/exchange/` are test-only and sign nothing
//! outside this suite.

use contextful_core::exchange::VerifyingMaterial;
use contextful_core::issue::{IssuancePolicy, MintContext, MintPlan, NodeRole, SignatureAlgorithm, SignatureEncoding};
use contextful_core::ports::{FixedClock, SigningPort};
use contextful_core::time::Instant;
use contextful_core::AuthorityError;
use contextful_policy::exchange::{
    answer, material_from_bytes, redeem, verify_assertion, Exchange, ExchangeAnswer, EXCHANGE_BODY_BYTES, EXCHANGE_PATH,
};
use contextful_policy::possession::{jwk_thumbprint, sign_proof, ProofChecker, ProofRefusal, ProofRequest};
use contextful_policy::verify::BEARER_LIFETIME_SECS;
use ed25519_dalek::SigningKey;
use contextful_policy::attenuate::{attenuate, Derivation};
use contextful_policy::issue::SeedSigner;
use contextful_policy::verify::introspect;
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

/// An issuer key whose reference resolves to no material when the mint signs.
struct UnresolvableSigner;

impl SigningPort for UnresolvableSigner {
    fn encoding(&self) -> SignatureEncoding {
        SignatureEncoding::Ed25519
    }
    fn public_key(&self) -> Vec<u8> {
        vec![7; 32]
    }
    fn sign(&self, _: &[u8]) -> Result<Vec<u8>, AuthorityError> {
        Err(AuthorityError::IssuerKeyUnresolvable("vault://issuer/primary resolves to no material".into()))
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

/// The wire handler's inputs: the persisted issuance policy and a real issuer key at `at`.
fn answer_at(exchange: Option<&Exchange>, body: &[u8], signer: &dyn SigningPort, at: Instant) -> ExchangeAnswer {
    answer_on(NodeRole::Primary, exchange, body, signer, at)
}

/// The wire handler's answer to `body` carrying the `DPoP` header `dpop`, its proofs
/// checked under `checker`.
fn answer_proved(
    exchange: Option<&Exchange>,
    body: &[u8],
    dpop: Option<&str>,
    checker: &ProofChecker<FixedClock>,
    signer: &dyn SigningPort,
) -> ExchangeAnswer {
    let issuance =
        IssuancePolicy::parse("default_audience = \"contextful://acme-research\"\nmax_lifetime_secs = 3600\n").unwrap();
    let clock = FixedClock(now());
    let ctx = MintContext { node: NodeRole::Primary, signer, clock: &clock };
    answer(exchange, body, dpop, checker, &issuance, &ctx)
}

/// A holder's proof over the exchange request `body`, issued at `iat` under `nonce`.
fn holder_proof(key: &SigningKey, body: &[u8], iat: Instant, nonce: &str) -> String {
    sign_proof(key, &ProofRequest { method: "POST", target: EXCHANGE_PATH, body }, iat, nonce)
}

fn answer_on(
    node: NodeRole,
    exchange: Option<&Exchange>,
    body: &[u8],
    signer: &dyn SigningPort,
    at: Instant,
) -> ExchangeAnswer {
    let issuance =
        IssuancePolicy::parse("default_audience = \"contextful://acme-research\"\nmax_lifetime_secs = 3600\n").unwrap();
    let clock = FixedClock(at);
    let ctx = MintContext { node, signer, clock: &clock };
    answer(exchange, body, None, &ProofChecker::new(clock), &issuance, &ctx)
}

fn request(assertion: &str) -> Vec<u8> {
    json!({ "jwt": assertion }).to_string().into_bytes()
}

fn identifier(a: &ExchangeAnswer) -> &str {
    a.body["error"]["identifier"].as_str().unwrap_or_default()
}

/// An exchange request body is the JSON object `{"jwt": "<assertion>"}`, which the command line builds from its assertion; a body of any other shape raises `ExchangeRequestMalformed` and mints nothing. A served face answers a mint `{"token": "<credential>"}`.
// spec: authority.exchange.wire@a5625ddd
#[test]
fn a_jwt_object_answers_a_token_object_and_any_other_body_answers_400() {
    let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let x = exchange(SECRET);
    let a = answer_at(Some(&x), &request(&hs256(&claims("dana@acme.example"), SECRET)), &signer, now());
    assert_eq!(a.status, 200, "{}", a.body);
    let token = a.body["token"].as_str().unwrap();
    assert_eq!(a.body.as_object().unwrap().len(), 1, "{}", a.body);
    let minted = introspect(token).unwrap();
    assert_eq!(minted.authority.sub.on_behalf_of.as_deref(), Some("user://dana@acme.example"));
    assert_eq!(minted.authority.aud, "contextful://acme-research");
    assert_eq!(minted.authority.exp - minted.authority.iat, 900);

    for body in [
        &b""[..],
        b"not json",
        b"[]",
        b"{}",
        b"{\"jwt\": 7}",
        b"{\"assertion\": \"x\"}",
        b"{\"jwt\": \"x\", \"extra\": 1}",
    ] {
        let a = answer_at(Some(&x), body, &signer, now());
        let shown = String::from_utf8_lossy(body);
        assert_eq!((a.status, identifier(&a)), (400, "ExchangeRequestMalformed"), "{shown}: {}", a.body);
        assert!(a.body.get("token").is_none(), "{}", a.body);
    }
}

/// An exchange request body holds at most 64 KiB; a longer body raises `ExchangeBodyTooLarge`, mints nothing and is not parsed.
// spec: authority.exchange.body-ceiling@31fc2b24
#[test]
fn a_body_past_64_kib_mints_nothing_and_is_not_parsed() {
    assert_eq!(EXCHANGE_BODY_BYTES, 64 * 1024);
    let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let x = exchange(SECRET);
    let good = request(&hs256(&claims("dana@acme.example"), SECRET));

    // A valid request padded with trailing whitespace to exactly the ceiling mints.
    let mut at_ceiling = good.clone();
    at_ceiling.resize(EXCHANGE_BODY_BYTES, b' ');
    assert_eq!(answer_at(Some(&x), &at_ceiling, &signer, now()).status, 200);

    // One byte more answers 413: the same valid request mints nothing.
    let mut past = good;
    past.resize(EXCHANGE_BODY_BYTES + 1, b' ');
    let a = answer_at(Some(&x), &past, &signer, now());
    assert_eq!((a.status, identifier(&a)), (413, "ExchangeBodyTooLarge"), "{}", a.body);
    assert!(a.body.get("token").is_none());
    // Garbage past the ceiling answers 413, not 400: the body is never parsed.
    let a = answer_at(Some(&x), &vec![b'x'; EXCHANGE_BODY_BYTES + 1], &signer, now());
    assert_eq!((a.status, identifier(&a)), (413, "ExchangeBodyTooLarge"), "{}", a.body);
}

/// An exchange in a project declaring no exchange policy raises `ExchangeUnconfigured`, naming `.contextful/exchange/policy.toml`, before the body is read.
// spec: authority.exchange.unconfigured@4f66ff43
#[test]
fn an_unconfigured_exchange_refuses_before_reading_the_body() {
    let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let issuance =
        IssuancePolicy::parse("default_audience = \"contextful://acme-research\"\nmax_lifetime_secs = 3600\n").unwrap();
    let clock = FixedClock(now());
    let ctx = MintContext { node: NodeRole::Primary, signer: &signer, clock: &clock };
    let dana = request(&hs256(&claims("dana@acme.example"), SECRET));
    // A valid request, garbage, and a body past the ceiling all refuse the same way.
    for body in [&dana[..], b"not json", &vec![b'x'; EXCHANGE_BODY_BYTES + 1][..]] {
        let err = redeem(None, body, None, &ProofChecker::new(clock), &issuance, &ctx).unwrap_err();
        assert!(
            matches!(&err, ProofRefusal::Refused(AuthorityError::ExchangeUnconfigured(m)) if m.contains(".contextful/exchange/policy.toml")),
            "{err}"
        );
    }
}

/// Over HTTP, {{authority.exchange.unconfigured}} answers 404, {{authority.exchange.wire}} and {{authority.exchange.holder-proof-invalid}} 400, {{authority.exchange.body-ceiling}} 413, {{authority.exchange.material-missing}} 503, {{authority.exchange.assertion-invalid}} 401, and any other refusal 403; every refusal body is `{"error": {"http", "identifier", "message"}}`.
// spec: authority.exchange.status-map@63a8fbea
#[test]
fn each_exchange_refusal_answers_its_mapped_status_naming_its_identifier() {
    let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let dana = request(&hs256(&claims("dana@acme.example"), SECRET));
    let x = exchange(SECRET);
    let oversized = vec![b' '; EXCHANGE_BODY_BYTES + 1];
    let forged = request(&hs256(&claims("dana@acme.example"), b"another"));
    let padded = request(&hs256(&claims(" dana@acme.example"), SECRET));

    let refusals = [
        // No exchange policy: 404, whatever the body.
        (answer_at(None, &dana, &signer, now()), 404, "ExchangeUnconfigured"),
        (answer_at(None, &oversized, &signer, now()), 404, "ExchangeUnconfigured"),
        // A body of another shape: 400.
        (answer_at(Some(&x), b"{}", &signer, now()), 400, "ExchangeRequestMalformed"),
        // Past the body ceiling: 413.
        (answer_at(Some(&x), &oversized, &signer, now()), 413, "ExchangeBodyTooLarge"),
        // No verifying material: 503.
        (answer_at(Some(&exchange(b"")), &dana, &signer, now()), 503, "ExchangeMaterialMissing"),
        // An assertion failing verification, by key or by lapse: 401.
        (answer_at(Some(&x), &forged, &signer, now()), 401, "ExchangeAssertionInvalid"),
        (answer_at(Some(&x), &dana, &signer, now().plus_secs(3600)), 401, "ExchangeAssertionInvalid"),
        // Any other refusal: a padded subject value the exchange refuses answers 403.
        (answer_at(Some(&x), &padded, &signer, now()), 403, "AuthoritySubjectMalformed"),
    ];
    for (a, status, id) in &refusals {
        assert_eq!((a.status, identifier(a)), (*status, *id), "{}", a.body);
        assert_eq!(a.body["error"]["http"], *status, "{}", a.body);
        assert!(a.body.get("token").is_none(), "{}", a.body);
        assert!(a.body["error"]["message"].as_str().is_some_and(|m| !m.is_empty()), "{}", a.body);
    }
}

/// Over HTTP, {{authority.issue.missing-key}}, {{authority.issue.unresolvable-key}}, {{authority.issue.algorithm-mismatch}} or {{authority.issue.replica-mint}} during an exchange answers 500, the body naming the identifier and withholding the refusal's detail.
// spec: authority.exchange.signing-fault@53d76d72
#[test]
fn a_signing_fault_answers_500_naming_its_identifier_and_withholding_its_detail() {
    let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let dana = request(&hs256(&claims("dana@acme.example"), SECRET));
    let x = exchange(SECRET);

    let faults = [
        // A replica mints nothing.
        (answer_on(NodeRole::Replica, Some(&x), &dana, &signer, now()), "ReplicaCannotIssue"),
        // An issuer key reference resolving to no material at signing time.
        (answer_at(Some(&x), &dana, &UnresolvableSigner, now()), "IssuerKeyUnresolvable"),
    ];
    for (a, id) in &faults {
        assert_eq!((a.status, identifier(a)), (500, *id), "{}", a.body);
        assert_eq!(a.body["error"]["http"], 500, "{}", a.body);
        assert!(a.body.get("token").is_none(), "{}", a.body);
        let message = a.body["error"]["message"].as_str().unwrap();
        assert!(!message.contains(':') && !message.contains("vault") && !message.contains("replica"), "{message}");
    }
}

/// A holder renews a served-face credential by exchanging a fresh assertion for a new credential with its own identifier and a full lifetime from the exchange instant.
// spec: authority.exchange.refresh@d16a1185
#[test]
fn a_fresh_assertion_renews_the_credential_as_a_new_one() {
    let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let x = exchange(SECRET);
    let first = answer_at(Some(&x), &request(&hs256(&claims("dana@acme.example"), SECRET)), &signer, now());
    let first = introspect(first.body["token"].as_str().unwrap()).unwrap();

    // Ten minutes on, a fresh assertion earns a new credential with its own identity and a
    // full lifetime from the renewal instant; the first keeps its expiry.
    let later = now().plus_secs(600);
    let mut fresh = claims("dana@acme.example");
    fresh["exp"] = json!(later.unix_secs() + 600);
    let second = answer_at(Some(&x), &request(&hs256(&fresh, SECRET)), &signer, later);
    let second = introspect(second.body["token"].as_str().unwrap()).unwrap();
    assert_ne!(first.authority.jti, second.authority.jti);
    assert_eq!(first.authority.exp, now().unix_secs() + 900);
    assert_eq!(second.authority.exp, later.unix_secs() + 900);
    assert_eq!(first.authority.sub, second.authority.sub);

    // A spent assertion renews nothing once it lapses.
    let spent = hs256(&claims("dana@acme.example"), SECRET);
    assert_eq!(answer_at(Some(&x), &request(&spent), &signer, now().plus_secs(600)).status, 401);
}

/// No surface extends a minted credential's expiry: a minted credential presented as the exchange assertion raises {{authority.exchange.assertion-invalid}}, and {{authority.attenuate.expiry-extended}} bounds every derived child.
// spec: authority.exchange.no-extension@def32ccc
#[test]
fn neither_the_exchange_nor_attenuation_extends_a_minted_credentials_expiry() {
    let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let x = exchange(SECRET);
    let first = answer_at(Some(&x), &request(&hs256(&claims("dana@acme.example"), SECRET)), &signer, now());
    let first = first.body["token"].as_str().unwrap().to_string();

    // The minted credential presented as the assertion renews nothing.
    let a = answer_at(Some(&x), &request(&first), &signer, now().plus_secs(600));
    assert_eq!((a.status, identifier(&a)), (401, "ExchangeAssertionInvalid"), "{}", a.body);
    assert!(a.body.get("token").is_none());

    // Attenuation derives no expiry past the parent's.
    let later = Derivation { expires_at: Some(now().plus_secs(3600)), ..Derivation::default() };
    let err = attenuate(&first, &later).unwrap_err();
    assert!(matches!(err, AuthorityError::AttenuationExpiryExtended(_)), "{err}");
}

/// An exchange request carrying a `DPoP` proof over `POST /auth/exchange` and its body mints a credential whose `cnf.jkt` is the proof key's thumbprint, the proof checked as {{authority.verify.possession-binding}}.
// spec: authority.exchange.holder-binding@c8a6525d
#[test]
fn a_holder_proof_binds_the_minted_credential_to_the_proof_key() {
    let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let x = exchange(SECRET);
    let checker = ProofChecker::new(FixedClock(now()));
    let key = SigningKey::from_bytes(&[9; 32]);
    let body = request(&hs256(&claims("dana@acme.example"), SECRET));
    let proof = holder_proof(&key, &body, now(), "exchange-1");

    let a = answer_proved(Some(&x), &body, Some(&proof), &checker, &signer);
    assert_eq!(a.status, 200, "{}", a.body);
    let minted = introspect(a.body["token"].as_str().unwrap()).unwrap();
    let cnf = minted.authority.cnf.expect("a proved exchange binds the holder key");
    assert_eq!(cnf.jkt, jwk_thumbprint(key.verifying_key().as_bytes()));
    assert_eq!(minted.authority.sub.on_behalf_of.as_deref(), Some("user://dana@acme.example"));

    // The command line's path binds the same way.
    let issuance =
        IssuancePolicy::parse("default_audience = \"contextful://acme-research\"\nmax_lifetime_secs = 3600\n").unwrap();
    let clock = FixedClock(now());
    let ctx = MintContext { node: NodeRole::Primary, signer: &signer, clock: &clock };
    let proof = holder_proof(&key, &body, now(), "exchange-2");
    let token = redeem(Some(&x), &body, Some(&proof), &checker, &issuance, &ctx).unwrap();
    assert_eq!(introspect(&token).unwrap().authority.cnf.unwrap().jkt, cnf.jkt);
}

/// An exchange request carrying no proof mints a credential with no confirmation claim, its lifetime clamped by {{authority.exchange.lifetime-ceiling}}, so a network face admits it under {{authority.verify.bearer-lifetime}}.
/// No issuance policy, face or command setting raises the cap {{authority.verify.bearer-lifetime}} sets; a credential living longer admits over a network only bound to a holder key.
// spec: authority.exchange.bearer-mint@22f9b05b
// spec: authority.issue.bearer-cap-fixed@4a4193c3
#[test]
fn an_exchange_without_a_proof_mints_a_short_lived_bearer() {
    let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let checker = ProofChecker::new(FixedClock(now()));
    let body = request(&hs256(&claims("dana@acme.example"), SECRET));
    for ttl in [900, 3600, 86_400] {
        let policy = POLICY.replace("ttl_secs       = 900", &format!("ttl_secs       = {ttl}"));
        let x = Exchange::from_config(&policy, SECRET).unwrap();
        let a = answer_proved(Some(&x), &body, None, &checker, &signer);
        assert_eq!(a.status, 200, "{}", a.body);
        let minted = introspect(a.body["token"].as_str().unwrap()).unwrap();
        assert!(minted.authority.cnf.is_none(), "ttl {ttl}: {:?}", minted.authority.cnf);
        let lifetime = minted.authority.exp - minted.authority.iat;
        assert!(lifetime <= BEARER_LIFETIME_SECS as i64, "ttl {ttl} minted a bearer living {lifetime} s");
        assert_eq!(lifetime, ttl.min(3600));
    }
}

/// An exchange request whose proof fails any check of {{authority.exchange.holder-binding}}, a replayed nonce included, raises `ExchangeHolderProofInvalid` and mints nothing.
// spec: authority.exchange.holder-proof-invalid@cb272e5d
#[test]
fn an_invalid_holder_proof_is_refused_and_mints_nothing() {
    let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let x = exchange(SECRET);
    let checker = ProofChecker::new(FixedClock(now()));
    let key = SigningKey::from_bytes(&[9; 32]);
    let body = request(&hs256(&claims("dana@acme.example"), SECRET));
    let other = request(&hs256(&claims("eli@acme.example"), SECRET));

    let replayed = holder_proof(&key, &body, now(), "exchange-replayed");
    assert_eq!(answer_proved(Some(&x), &body, Some(&replayed), &checker, &signer).status, 200);

    let mut tampered = holder_proof(&key, &body, now(), "exchange-tampered");
    tampered.replace_range(tampered.len() - 4.., "AAAA");
    let refused = [
        ("not a proof", "not-a-proof".to_string()),
        ("over another body", holder_proof(&key, &other, now(), "exchange-other")),
        ("over another target", sign_proof(&key, &ProofRequest { method: "POST", target: "/mcp", body: &body }, now(), "exchange-target")),
        ("stale", holder_proof(&key, &body, now().minus_secs(301), "exchange-stale")),
        ("forged signature", tampered),
        ("replayed nonce", replayed),
    ];
    for (why, proof) in &refused {
        let a = answer_proved(Some(&x), &body, Some(proof), &checker, &signer);
        assert_eq!((a.status, identifier(&a)), (400, "ExchangeHolderProofInvalid"), "{why}: {}", a.body);
        assert!(a.body.get("token").is_none(), "{why}: {}", a.body);
    }
}
