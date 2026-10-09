//! `authority.exchange`: the exchange policy and the mint a verified assertion earns.

use contextful_core::exchange::{
    ExchangePolicy, VerifyingMaterial, EXCHANGE_LIFETIME_CEILING_SECS, EXCHANGE_LIFETIME_DEFAULT_SECS,
};
use contextful_core::grant::{Action, TablePattern, TenantScope};
use contextful_core::issue::{IssuancePolicy, MintContext, MintPlan, NodeRole, PolicyError, SignatureEncoding};
use contextful_core::ports::{FixedClock, SigningPort};
use contextful_core::time::Instant;
use contextful_core::AuthorityError;
use serde_json::{json, Map, Value};

const NOW: &str = "2030-01-01T00:00:00Z";
const PROJECT_AUD: &str = "contextful://acme-research";

/// The exchange policy example in `spec/50-authority.md`, verbatim.
const SPEC_POLICY: &str = r#"
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

[[role_grants.loader]]
actions = ["write"]
tables  = ["research/filings"]
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

fn material() -> VerifyingMaterial {
    VerifyingMaterial::SharedSecret(b"s3cret".to_vec())
}

fn issuance(max_lifetime_secs: u64) -> IssuancePolicy {
    IssuancePolicy::parse(&format!("default_audience = \"{PROJECT_AUD}\"\nmax_lifetime_secs = {max_lifetime_secs}\n"))
        .unwrap()
}

/// Claims of a verified assertion for Dana at acme-eu, expiring 10 min after `NOW`.
fn claims(roles: Value) -> Map<String, Value> {
    let mut c = json!({
        "iss": "https://login.acme.example/",
        "aud": "contextful-console",
        "exp": now().unix_secs() + 600,
        "sub": "dana@acme.example",
        "org_id": "acme-eu",
    });
    if !roles.is_null() {
        c["roles"] = roles;
    }
    c.as_object().unwrap().clone()
}

fn with(mut c: Map<String, Value>, key: &str, value: Value) -> Map<String, Value> {
    if value.is_null() {
        c.remove(key);
    } else {
        c.insert(key.to_string(), value);
    }
    c
}

fn mint(policy: &ExchangePolicy, claims: &Map<String, Value>, ceiling: u64) -> Result<MintPlan, AuthorityError> {
    let clock = FixedClock(now());
    let ctx = MintContext { node: NodeRole::Primary, signer: &FakeSigner, clock: &clock };
    policy.mint(Some(&material()), claims, &issuance(ceiling), &ctx)
}

fn spec_policy() -> ExchangePolicy {
    ExchangePolicy::parse(SPEC_POLICY).unwrap()
}

/// An exchange policy declares `expected_iss`, optional `expected_aud`, `subject_map`, `tenant_claim`, `role_claim`, `role_grants`, `default_grants`, `ttl_secs`, `minted_iss` and `minted_aud`.
// spec: authority.exchange.policy@3795898c
#[test]
fn the_exchange_policy_declares_its_fields() {
    assert_eq!(ExchangePolicy::PATH, ".contextful/exchange/policy.toml");
    assert_eq!(ExchangePolicy::VERIFY_KEY_PATH, ".contextful/exchange/verify.key");

    let p = spec_policy();
    assert_eq!(p.expected_iss, "https://login.acme.example/");
    assert_eq!(p.expected_aud.as_deref(), Some("contextful-console"));
    assert_eq!(p.tenant_claim.as_deref(), Some("org_id"));
    assert_eq!(p.role_claim.as_deref(), Some("roles"));
    assert!(p.default_grants.is_empty());
    assert_eq!(p.ttl_secs, Some(900));
    assert_eq!(p.minted_iss.as_deref(), Some("contextful://acme-research"));
    assert_eq!(p.minted_aud.as_deref(), Some("contextful://acme-research"));
    let obo = p.subject_map.on_behalf_of.as_ref().unwrap();
    assert_eq!((obo.claim.as_str(), obo.template.as_deref()), ("sub", Some("user://{}")));
    assert!(p.subject_map.agent.is_none());
    let analyst = &p.role_grants["analyst"];
    assert_eq!(analyst.len(), 1);
    assert_eq!(analyst[0].actions, vec![Action::Read]);
    assert_eq!(analyst[0].tables, vec![TablePattern::Prefix("research/".into())]);
    assert_eq!(analyst[0].templates, Some(vec!["quarterly_rollup".to_string()]));
    assert_eq!(p.role_grants["loader"][0].tables, vec![TablePattern::Exact("research/filings".into())]);

    // Only `expected_iss` is required.
    let minimal = ExchangePolicy::parse("expected_iss = \"https://idp.example/\"\n").unwrap();
    assert!(minimal.expected_aud.is_none() && minimal.role_grants.is_empty() && minimal.ttl_secs.is_none());

    for (bad, why) in [
        ("expected_aud = \"x\"\n", "missing expected_iss"),
        ("expected_iss = \"x\"\nttl = 9\n", "unknown field"),
        ("expected_iss = \"x\"\n[subject_map]\ntenant = { claim = \"org\" }\n", "tenancy is no subject member"),
        ("expected_iss = \"x\"\n[subject_map]\nagent = { claim = \"azp\", template = \"agent://\" }\n", "template without {}"),
    ] {
        assert!(matches!(ExchangePolicy::parse(bad), Err(PolicyError::Malformed(_))), "{why}");
    }
    let star = "expected_iss = \"x\"\n[[role_grants.a]]\nactions = [\"read\"]\ntables = [\"re*search\"]\n";
    assert!(matches!(ExchangePolicy::parse(star), Err(PolicyError::Authority(AuthorityError::GrantPatternMalformed(_)))));
    let verb = "expected_iss = \"x\"\n[[role_grants.a]]\nactions = [\"delete\"]\ntables = [\"*\"]\n";
    assert!(matches!(ExchangePolicy::parse(verb), Err(PolicyError::Authority(AuthorityError::GrantActionUnknown(_)))));
}

/// Minted grants come from `role_grants` and `default_grants` alone. A verified role matching no entry earns `default_grants`, which is empty unless declared.
// spec: authority.exchange.minted-grants@3956461f
#[test]
fn minted_grants_come_from_role_grants_and_default_grants_alone() {
    let p = spec_policy();
    let tables = |plan: &MintPlan| plan.grants.iter().map(|g| (g.actions.clone(), g.tables.clone())).collect::<Vec<_>>();

    let analyst = mint(&p, &claims(json!(["analyst"])), 3600).unwrap();
    assert_eq!(tables(&analyst), vec![(vec![Action::Read], vec![TablePattern::Prefix("research/".into())])]);

    let one = mint(&p, &claims(json!("loader")), 3600).unwrap();
    assert_eq!(one.grants[0].actions, vec![Action::Write]);
    let both = mint(&p, &claims(json!(["loader", "analyst"])), 3600).unwrap();
    assert_eq!(both.grants.len(), 2);
    assert_eq!(both.grants[0].actions, vec![Action::Write]);
    assert_eq!(both.grants[1].actions, vec![Action::Read]);

    // A role or claim the policy does not name earns the empty default.
    for roles in [json!(["intern"]), json!("intern"), Value::Null] {
        let plan = mint(&p, &claims(roles.clone()), 3600).unwrap();
        assert!(plan.grants.is_empty(), "{roles}: {:?}", plan.grants);
    }

    // A declared default is what an unmatched role earns.
    let declared = ExchangePolicy::parse(&SPEC_POLICY.replace(
        "default_grants = []",
        "default_grants = [{ actions = [\"read\"], tables = [\"public/*\"] }]",
    ))
    .unwrap();
    let plan = mint(&declared, &claims(json!(["intern"])), 3600).unwrap();
    assert_eq!(tables(&plan), vec![(vec![Action::Read], vec![TablePattern::Prefix("public/".into())])]);
    let plan = mint(&declared, &claims(json!(["analyst"])), 3600).unwrap();
    assert_eq!(tables(&plan), vec![(vec![Action::Read], vec![TablePattern::Prefix("research/".into())])]);
}

/// The assertion claim `tenant_claim` names becomes the tenant scope of every minted grant; no subject member carries tenancy.
// spec: authority.exchange.tenant@4f8e9e1a
#[test]
fn the_tenant_claim_scopes_every_minted_grant() {
    let p = ExchangePolicy::parse(&SPEC_POLICY.replace(
        "tables  = [\"research/filings\"]",
        "tables  = [\"research/filings\", \"research/notes\"]",
    ))
    .unwrap();
    let plan = mint(&p, &claims(json!(["analyst", "loader"])), 3600).unwrap();

    // One grant per table pattern, each scoped to the claim's bytes.
    let scoped: Vec<(TablePattern, Option<TenantScope>)> =
        plan.grants.iter().map(|g| (g.tables[0].clone(), g.tenant.clone())).collect();
    let tenant = |table: &str| Some(TenantScope { table: table.into(), value: "acme-eu".into() });
    assert_eq!(
        scoped,
        vec![
            (TablePattern::Prefix("research/".into()), tenant("research/*")),
            (TablePattern::Exact("research/filings".into()), tenant("research/filings")),
            (TablePattern::Exact("research/notes".into()), tenant("research/notes")),
        ]
    );
    assert!(!plan.grants.is_empty(), "the exclusion below ranges over no element");
    assert!(plan.grants.iter().all(|g| g.tables.len() == 1));

    // The subject carries the principal and nothing naming the tenant.
    assert_eq!(plan.subject.on_behalf_of.as_deref(), Some("user://dana@acme.example"));
    let subject = serde_json::to_string(&plan.subject).unwrap();
    assert!(!subject.contains("acme-eu"), "{subject}");

    // The claim's bytes pass untouched: no trimming or case folding.
    let padded = mint(&p, &with(claims(json!(["analyst"])), "org_id", json!("Acme-EU ")), 3600).unwrap();
    assert_eq!(padded.grants[0].tenant.as_ref().unwrap().value, "Acme-EU ");

    // A policy naming no tenant claim mints unscoped grants.
    let unscoped = ExchangePolicy::parse(&SPEC_POLICY.replace("tenant_claim   = \"org_id\"\n", "")).unwrap();
    let plan = mint(&unscoped, &claims(json!(["analyst"])), 3600).unwrap();
    assert!(!plan.grants.is_empty(), "the exclusion below ranges over no element");
    assert!(plan.grants.iter().all(|g| g.tenant.is_none()));
}

/// A minted credential lives 900 s where the policy declares no `ttl_secs`.
// spec: authority.exchange.lifetime-default@07b2be75
#[test]
fn a_policy_without_ttl_mints_900_seconds() {
    assert_eq!(EXCHANGE_LIFETIME_DEFAULT_SECS, 900);
    let p = ExchangePolicy::parse(&SPEC_POLICY.replace("ttl_secs       = 900\n", "")).unwrap();
    assert!(p.ttl_secs.is_none());
    let plan = mint(&p, &claims(json!(["analyst"])), 86_400).unwrap();
    assert_eq!(plan.lifetime_secs, 900);
    assert_eq!(plan.expires_at, now().plus_secs(900));
}

/// A configured `ttl_secs` clamps down to 3600 s and never extends.
// spec: authority.exchange.lifetime-ceiling@cdb64908
#[test]
fn a_configured_ttl_clamps_to_3600_seconds_and_the_issuance_ceiling() {
    assert_eq!(EXCHANGE_LIFETIME_CEILING_SECS, 3600);
    let ttl = |secs: u64| ExchangePolicy::parse(&SPEC_POLICY.replace("ttl_secs       = 900", &format!("ttl_secs = {secs}"))).unwrap();

    assert_eq!(mint(&ttl(7200), &claims(json!(["analyst"])), 86_400).unwrap().lifetime_secs, 3600);
    assert_eq!(mint(&ttl(3600), &claims(json!(["analyst"])), 86_400).unwrap().lifetime_secs, 3600);
    assert_eq!(mint(&ttl(600), &claims(json!(["analyst"])), 86_400).unwrap().lifetime_secs, 600);
    // The persisted issuance ceiling clamps it further, never refusing.
    assert_eq!(mint(&ttl(3600), &claims(json!(["analyst"])), 1800).unwrap().lifetime_secs, 1800);
}

/// A minted credential carries `minted_aud`, falling back to the project's persisted default audience.
// spec: authority.exchange.audience@c7641f20
#[test]
fn the_minted_audience_falls_back_to_the_persisted_default() {
    let declared = ExchangePolicy::parse(&SPEC_POLICY.replace(
        "minted_aud     = \"contextful://acme-research\"",
        "minted_aud     = \"contextful://acme-console\"",
    ))
    .unwrap();
    let plan = mint(&declared, &claims(json!(["analyst"])), 3600).unwrap();
    assert_eq!(plan.audience, "contextful://acme-console");
    assert_eq!(plan.issuer.as_deref(), Some("contextful://acme-research"));

    let fallback =
        ExchangePolicy::parse(&SPEC_POLICY.replace("minted_aud     = \"contextful://acme-research\"\n", "")).unwrap();
    assert!(fallback.minted_aud.is_none());
    assert_eq!(mint(&fallback, &claims(json!(["analyst"])), 3600).unwrap().audience, PROJECT_AUD);
}

/// The claim checks behind `authority.exchange.assertion-invalid`: issuer, audience, expiry
/// and mapped claims. The policy crate's suite pins the clause over signed assertions.
#[test]
fn an_assertion_failing_any_check_is_invalid_and_mints_nothing() {
    let p = spec_policy();
    let good = claims(json!(["analyst"]));
    assert!(mint(&p, &good, 3600).is_ok());

    let lapsed = now().unix_secs();
    for (why, bad) in [
        ("untrusted issuer", with(good.clone(), "iss", json!("https://evil.example/"))),
        ("issuer absent", with(good.clone(), "iss", Value::Null)),
        ("untrusted audience", with(good.clone(), "aud", json!("another-app"))),
        ("audience absent", with(good.clone(), "aud", Value::Null)),
        ("lapsed at the evaluation instant", with(good.clone(), "exp", json!(lapsed))),
        ("lapsed", with(good.clone(), "exp", json!(lapsed - 1))),
        ("no expiry", with(good.clone(), "exp", Value::Null)),
        ("expiry not numeric", with(good.clone(), "exp", json!("2030-01-01T00:10:00Z"))),
        ("mapped subject claim absent", with(good.clone(), "sub", Value::Null)),
        ("mapped subject claim not a string", with(good.clone(), "sub", json!(42))),
        ("tenant claim absent", with(good.clone(), "org_id", Value::Null)),
        ("role claim not strings", with(good.clone(), "roles", json!([1, 2]))),
    ] {
        match mint(&p, &bad, 3600) {
            Err(AuthorityError::ExchangeAssertionInvalid(msg)) => assert!(!msg.is_empty(), "{why}"),
            other => panic!("{why}: expected ExchangeAssertionInvalid, got {other:?}"),
        }
    }

    // An audience list containing the expected audience is trusted.
    assert!(mint(&p, &with(good.clone(), "aud", json!(["other", "contextful-console"])), 3600).is_ok());
    // A policy expecting no audience checks none.
    let open = ExchangePolicy::parse(&SPEC_POLICY.replace("expected_aud   = \"contextful-console\"\n", "")).unwrap();
    assert!(mint(&open, &with(good, "aud", Value::Null), 3600).is_ok());
}

/// An exchange configured with no verifying material raises `ExchangeMaterialMissing` and mints nothing.
// spec: authority.exchange.material-missing@fad72385
#[test]
fn an_exchange_without_verifying_material_mints_nothing() {
    let p = spec_policy();
    let clock = FixedClock(now());
    let ctx = MintContext { node: NodeRole::Primary, signer: &FakeSigner, clock: &clock };
    let good = claims(json!(["analyst"]));

    for material in [
        None,
        Some(VerifyingMaterial::SharedSecret(vec![])),
        Some(VerifyingMaterial::Rs256PublicKeyPem("  \n".into())),
        Some(VerifyingMaterial::KeySet(String::new())),
    ] {
        let err = p.mint(material.as_ref(), &good, &issuance(3600), &ctx).unwrap_err();
        assert!(matches!(err, AuthorityError::ExchangeMaterialMissing(_)), "{material:?}: {err}");
        assert!(err.to_string().starts_with("ExchangeMaterialMissing"), "{err}");
    }
    let pem = VerifyingMaterial::Rs256PublicKeyPem("-----BEGIN PUBLIC KEY-----\n...".into());
    assert!(p.mint(Some(&pem), &good, &issuance(3600), &ctx).is_ok());
}
