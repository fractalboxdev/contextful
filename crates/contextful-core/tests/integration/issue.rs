//! `authority.issue`: the persisted issuance policy and the mint-time checks.

use contextful_core::grant::{Action, Grant, TablePattern};
use contextful_core::identify::Subject;
use contextful_core::issue::{
    key_rotation_due, IssuancePolicy, Lifetime, MintAuthority, MintContext, MintRequest, NodeRole, PolicyError,
    SignatureAlgorithm, SignatureEncoding, ISSUANCE_LIFETIME_CEILING_SECS, ISSUER_KEY_ROTATION_CADENCE_SECS,
};
use contextful_core::ports::{Clock, FixedClock, SigningPort};
use contextful_core::time::Instant;
use contextful_core::AuthorityError;

const AUD: &str = "contextful://acme-research";
const NOW: &str = "2030-01-01T00:00:00Z";

struct FakeSigner(SignatureAlgorithm);

impl SigningPort for FakeSigner {
    fn encoding(&self) -> SignatureEncoding {
        SignatureEncoding::canonical(self.0)
    }
    fn public_key(&self) -> Vec<u8> {
        vec![7; 32]
    }
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, AuthorityError> {
        Ok(message.iter().rev().copied().collect())
    }
}

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

fn policy(max_lifetime_secs: u64) -> IssuancePolicy {
    IssuancePolicy::parse(&format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = {max_lifetime_secs}\n")).unwrap()
}

fn grant(actions: &[Action]) -> Grant {
    Grant {
        actions: actions.to_vec(),
        tables: vec![TablePattern::Prefix("research/".into())],
        tenant: None,
        aggregate: None,
        templates: None,
        max_rows: None,
        max_duration_ms: None,
        max_response_bytes: None,
    }
}

fn dana() -> Subject {
    Subject {
        on_behalf_of: Some("user://dana@acme.example".into()),
        agent: Some("agent://research-loop".into()),
        ..Subject::default()
    }
}

fn check(p: &IssuancePolicy, req: &MintRequest) -> Result<contextful_core::issue::MintPlan, AuthorityError> {
    let signer = FakeSigner(SignatureAlgorithm::Ed25519);
    let clock = FixedClock(at(NOW));
    p.check(req, &MintContext { node: NodeRole::Primary, signer: &signer, clock: &clock })
}

/// A project persists in version control an issuance policy naming the default audience and the longest lifetime any mint produces, at most 24 h.
// spec: authority.issue.ceiling@afa3814e
#[test]
fn the_persisted_ceiling_holds_at_most_24_hours() {
    assert_eq!(ISSUANCE_LIFETIME_CEILING_SECS, 24 * 60 * 60);
    assert_eq!(IssuancePolicy::PATH, ".contextful/issuance.toml");

    let p = policy(ISSUANCE_LIFETIME_CEILING_SECS);
    assert_eq!(p.default_audience, AUD);
    assert_eq!(p.max_lifetime_secs, 86_400);

    let over = IssuancePolicy::parse(&format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 86401\n"));
    assert_eq!(over, Err(PolicyError::CeilingAboveBound(86_401)));
    assert!(matches!(IssuancePolicy::parse("max_lifetime_secs = 900\n"), Err(PolicyError::Malformed(_))));
    assert!(matches!(
        IssuancePolicy::parse(&format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 900\nextra = 1\n")),
        Err(PolicyError::Malformed(_))
    ));

    // The longest lifetime any mint produces: a mint naming no lifetime lives the ceiling.
    let plan = check(&policy(3600), &MintRequest::custody(dana(), vec![grant(&[Action::Read])])).unwrap();
    assert_eq!(plan.lifetime_secs, 3600);
    assert_eq!(plan.expires_at, at("2030-01-01T01:00:00Z"));
    assert_eq!(plan.audience, AUD);
}

/// An explicitly requested lifetime above the persisted ceiling raises `IssuanceLifetimeAboveCeiling`; an exchange policy's lifetime clamps down to the ceiling instead.
// spec: authority.issue.above-ceiling@2e27fe73
#[test]
fn an_explicit_lifetime_above_the_ceiling_refuses_and_an_exchange_lifetime_clamps() {
    let p = policy(3600);
    let mut req = MintRequest::custody(dana(), vec![grant(&[Action::Read])]);

    req.lifetime = Lifetime::Requested(900);
    assert_eq!(check(&p, &req).unwrap().lifetime_secs, 900);
    req.lifetime = Lifetime::Requested(3600);
    assert_eq!(check(&p, &req).unwrap().lifetime_secs, 3600);

    req.lifetime = Lifetime::Requested(7200);
    let err = check(&p, &req).unwrap_err();
    assert!(matches!(err, AuthorityError::IssuanceLifetimeAboveCeiling(_)), "{err}");
    assert!(err.to_string().starts_with("IssuanceLifetimeAboveCeiling"), "{err}");

    req.lifetime = Lifetime::Clamped(7200);
    let plan = check(&p, &req).unwrap();
    assert_eq!(plan.lifetime_secs, 3600);
    assert_eq!(plan.expires_at, at("2030-01-01T01:00:00Z"));
}

/// Lowering the ceiling records the previous value and its instant; rotation-grace validation uses the recorded value until the last credential minted under it lapses.
#[test]
fn lowering_the_ceiling_records_the_previous_value_until_its_credentials_lapse() {
    let lowered_at = at(NOW);
    let mut p = policy(86_400);
    p.lower_ceiling(3600, lowered_at).unwrap();
    assert_eq!(p.max_lifetime_secs, 3600);
    assert_eq!(p.lowered.len(), 1);
    assert_eq!(p.lowered[0].previous_max_lifetime_secs, 86_400);
    assert_eq!(p.lowered[0].at, lowered_at);

    // The record persists in the policy file.
    let reread = IssuancePolicy::parse(&p.to_toml()).unwrap();
    assert_eq!(reread, p);

    // A credential minted just before the lowering lives until lowered_at + 24 h.
    assert_eq!(p.effective_ceiling_secs(lowered_at.plus_secs(3600)), 86_400);
    assert_eq!(p.effective_ceiling_secs(lowered_at.plus_secs(86_399)), 86_400);
    assert_eq!(p.effective_ceiling_secs(lowered_at.plus_secs(86_400)), 3600);

    // Mints bind the lowered value at once.
    let mut req = MintRequest::custody(dana(), vec![grant(&[Action::Read])]);
    req.lifetime = Lifetime::Requested(7200);
    assert!(matches!(check(&p, &req), Err(AuthorityError::IssuanceLifetimeAboveCeiling(_))));

    // Raising records nothing; a raise past the bound refuses.
    let mut raised = policy(3600);
    raised.lower_ceiling(7200, lowered_at).unwrap();
    assert!(raised.lowered.is_empty());
    assert_eq!(raised.effective_ceiling_secs(lowered_at), 7200);
    assert_eq!(raised.lower_ceiling(90_000, lowered_at), Err(PolicyError::CeilingAboveBound(90_000)));
}

/// A mint granting `write`, `execute` or `forget` whose subject names no `on_behalf_of` raises `IssuancePrincipalRequired`.
// spec: authority.issue.principal-required@23221e9a
#[test]
fn a_write_execute_or_forget_mint_without_a_principal_refuses() {
    let p = policy(3600);
    let agent_only = Subject { agent: Some("agent://loader".into()), ..Subject::default() };

    for action in [Action::Write, Action::Execute, Action::Forget] {
        let req = MintRequest::custody(agent_only.clone(), vec![grant(&[Action::Read]), grant(&[action])]);
        let err = check(&p, &req).unwrap_err();
        assert!(matches!(err, AuthorityError::IssuancePrincipalRequired(_)), "{action:?}: {err}");

        let with_principal = MintRequest::custody(dana(), vec![grant(&[action])]);
        assert!(check(&p, &with_principal).is_ok(), "{action:?}");
    }
    let read_only = MintRequest::custody(agent_only, vec![grant(&[Action::Read])]);
    assert!(check(&p, &read_only).is_ok());
}

#[test]
fn a_forget_mint_requires_an_acting_principal_while_subjectless_read_remains_valid() {
    let forget = Action::parse("forget").expect("forget is an attributed effect action");
    let p = policy(3600);
    let agent_only = Subject { agent: Some("agent://eraser".into()), ..Subject::default() };
    let request = MintRequest::custody(agent_only.clone(), vec![grant(&[forget])]);
    assert!(matches!(check(&p, &request), Err(AuthorityError::IssuancePrincipalRequired(_))));
    assert!(check(&p, &MintRequest::custody(dana(), vec![grant(&[forget])])).is_ok());
    assert!(check(&p, &MintRequest::custody(agent_only, vec![grant(&[Action::Read])])).is_ok());
}

/// A mint's default action set is `read` alone.
// spec: authority.issue.default-read@508f9199
#[test]
fn a_grant_naming_no_action_mints_read_alone() {
    let agent_only = Subject { agent: Some("agent://research-loop".into()), ..Subject::default() };
    let plan = check(&policy(3600), &MintRequest::custody(agent_only, vec![grant(&[])])).unwrap();
    assert_eq!(plan.grants.len(), 1);
    assert_eq!(plan.grants[0].actions, vec![Action::Read]);
    assert_eq!(IssuancePolicy::DEFAULT_ACTIONS, &[Action::Read]);
}

/// A credential names Ed25519, the default, or ECDSA over P-256; the pinned key's scheme is authoritative.
// spec: authority.issue.algorithm@e51d2b50
#[test]
fn a_credential_names_the_pinned_keys_scheme_ed25519_by_default() {
    assert_eq!(SignatureAlgorithm::default(), SignatureAlgorithm::Ed25519);
    assert_eq!(SignatureAlgorithm::parse("Ed25519"), Some(SignatureAlgorithm::Ed25519));
    assert_eq!(SignatureAlgorithm::parse("ES256"), Some(SignatureAlgorithm::Es256));
    assert_eq!(SignatureAlgorithm::parse("RS256"), None);
    assert_eq!(SignatureAlgorithm::Es256.to_string(), "ES256");
    assert_eq!(serde_json::to_string(&SignatureAlgorithm::Ed25519).unwrap(), "\"Ed25519\"");

    let p = policy(3600);
    let req = MintRequest::custody(dana(), vec![grant(&[Action::Read])]);
    let clock = FixedClock(at(NOW));
    for key in [SignatureAlgorithm::Ed25519, SignatureAlgorithm::Es256] {
        let signer = FakeSigner(key);
        let plan = p.check(&req, &MintContext { node: NodeRole::Primary, signer: &signer, clock: &clock }).unwrap();
        assert_eq!(plan.algorithm, key);
        assert_eq!(plan.issued_at, clock.now());
    }
}

/// A credential naming a scheme other than its pinned key's raises `SignatureAlgorithmMismatch`.
// spec: authority.issue.algorithm-mismatch@fcf0eb13
#[test]
fn a_named_scheme_other_than_the_pinned_keys_refuses() {
    assert_eq!(SignatureAlgorithm::check_named("Ed25519", SignatureAlgorithm::Ed25519), Ok(SignatureAlgorithm::Ed25519));
    for (named, pinned) in
        [("ES256", SignatureAlgorithm::Ed25519), ("Ed25519", SignatureAlgorithm::Es256), ("RS256", SignatureAlgorithm::Ed25519)]
    {
        let err = SignatureAlgorithm::check_named(named, pinned).unwrap_err();
        assert!(matches!(err, AuthorityError::SignatureAlgorithmMismatch(_)), "{named}: {err}");
        assert!(err.to_string().contains(named), "{err}");
    }

    let mut req = MintRequest::custody(dana(), vec![grant(&[Action::Read])]);
    req.algorithm = Some(SignatureAlgorithm::Es256);
    assert!(matches!(check(&policy(3600), &req), Err(AuthorityError::SignatureAlgorithmMismatch(_))));
}

/// A mint request presenting no grant carrying `admin` raises `IssuanceUnauthorized`.
// spec: authority.issue.unauthorized-mint@7a3de1bc
#[test]
fn a_mint_request_presenting_no_admin_grant_refuses() {
    let p = policy(3600);
    let mut req = MintRequest::custody(dana(), vec![grant(&[Action::Read])]);

    for presented in [vec![], vec![grant(&[Action::Read, Action::Write, Action::Execute])]] {
        req.authority = MintAuthority::Presented(presented);
        let err = check(&p, &req).unwrap_err();
        assert!(matches!(err, AuthorityError::IssuanceUnauthorized(_)), "{err}");
    }
    req.authority = MintAuthority::Presented(vec![grant(&[Action::Read]), grant(&[Action::Admin])]);
    assert!(check(&p, &req).is_ok());
}

/// A replica holds the issuer's public key and no signing material; a mint there raises `ReplicaCannotIssue`.
// spec: authority.issue.replica-mint@ef2c1d27
#[test]
fn a_mint_on_a_replica_refuses() {
    let signer = FakeSigner(SignatureAlgorithm::Ed25519);
    let clock = FixedClock(at(NOW));
    let req = MintRequest::custody(dana(), vec![grant(&[Action::Read])]);
    let err =
        policy(3600).check(&req, &MintContext { node: NodeRole::Replica, signer: &signer, clock: &clock }).unwrap_err();
    assert!(matches!(err, AuthorityError::ReplicaCannotIssue(_)), "{err}");
    assert!(err.to_string().starts_with("ReplicaCannotIssue"), "{err}");
}

/// A mint whose subject declares a wildcard inference zone raises `IssuanceZoneWildcard`.
// spec: authority.issue.zone-wildcard@514807b9
#[test]
fn a_subject_declaring_a_wildcard_zone_refuses() {
    let p = policy(3600);
    for zone in ["*", " * ", "on-prem:*", "public-cloud:*"] {
        let subject = Subject { zone: Some(zone.into()), ..dana() };
        let err = check(&p, &MintRequest::custody(subject, vec![grant(&[Action::Read])])).unwrap_err();
        assert!(matches!(err, AuthorityError::IssuanceZoneWildcard(_)), "{zone}: {err}");
    }
    for zone in ["local:device", "on-prem:rack-7", "public-cloud:eu-west"] {
        let subject = Subject { zone: Some(zone.into()), ..dana() };
        assert!(check(&p, &MintRequest::custody(subject, vec![grant(&[Action::Read])])).is_ok(), "{zone}");
    }
}

/// The issuer signing key rotates every 90 d, and at once on suspected compromise.
#[test]
fn the_issuer_key_rotates_every_90_days_and_at_once_on_compromise() {
    assert_eq!(ISSUER_KEY_ROTATION_CADENCE_SECS, 90 * 24 * 60 * 60);
    let rotated = at(NOW);
    assert!(!key_rotation_due(rotated, rotated.plus_secs(ISSUER_KEY_ROTATION_CADENCE_SECS - 1), false));
    assert!(key_rotation_due(rotated, rotated.plus_secs(ISSUER_KEY_ROTATION_CADENCE_SECS), false));
    assert!(key_rotation_due(rotated, rotated.plus_secs(1), true));
}
