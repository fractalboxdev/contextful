//! Fixtures shared by the credential tests: an issuer, a checked mint plan, and raw
//! chains built beside the profile to exercise its refusals.

use biscuit_auth::{BiscuitBuilder, BlockBuilder, UnverifiedBiscuit};
use contextful_core::claims::AuthorityBlock;
use contextful_core::grant::{Action, Grant, TablePattern};
use contextful_core::identify::Subject;
use contextful_core::issue::{IssuancePolicy, Lifetime, MintContext, MintPlan, MintRequest, NodeRole, SignatureAlgorithm};
use contextful_core::ports::{FixedClock, SigningPort};
use contextful_core::revoke::Denylist;
use contextful_core::time::Instant;
use contextful_core::AuthorityError;
use contextful_policy::issue::{authority_block, mint, MintClaims, SeedSigner};
use contextful_policy::keyset::{KeySet, KeySource, StaticPins};
use contextful_policy::profile::authority_facts;
use contextful_policy::revoke::RevocationState;
use contextful_policy::verify::{verify, Admission, AdmittedAuthority};

pub const AUD: &str = "contextful://acme-research";
pub const MINTED: &str = "2030-01-01T00:00:00Z";
pub const DURING: &str = "2030-01-01T00:05:00Z";
pub const TTL: u64 = 900;

pub fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}

pub fn issuer() -> SeedSigner {
    SeedSigner::generate(SignatureAlgorithm::Ed25519)
}

pub fn keys(signer: &SeedSigner) -> KeySet {
    let pins = StaticPins::parse(&signer.public_key_text()).unwrap();
    (*pins.keys().unwrap()).clone()
}

pub fn grant(actions: &[Action], tables: &[&str]) -> Grant {
    Grant {
        actions: actions.to_vec(),
        tables: tables.iter().map(|t| TablePattern::parse(t).unwrap()).collect(),
        tenant: None,
        aggregate: None,
        templates: None,
        max_rows: None,
    }
}

pub fn dana() -> Subject {
    Subject {
        on_behalf_of: Some("user://dana@acme.example".into()),
        agent: Some("agent://research-loop".into()),
        ..Subject::default()
    }
}

pub fn plan_for(signer: &dyn SigningPort, subject: Subject, grants: Vec<Grant>) -> MintPlan {
    let policy = IssuancePolicy::parse(&format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n")).unwrap();
    let mut req = MintRequest::custody(subject, grants);
    req.lifetime = Lifetime::Requested(TTL);
    let clock = FixedClock(at(MINTED));
    policy.check(&req, &MintContext { node: NodeRole::Primary, signer, clock: &clock }).unwrap()
}

pub fn plan(signer: &SeedSigner) -> MintPlan {
    plan_for(signer, dana(), vec![grant(&[Action::Read], &["research/*"])])
}

/// A credential for dana reading `research/*`, minted at [`MINTED`] for [`TTL`] seconds.
pub fn minted(signer: &SeedSigner) -> String {
    mint(&plan(signer), &MintClaims::default(), signer).unwrap()
}

pub fn no_revocation() -> RevocationState {
    RevocationState::default()
}

pub fn admit(credential: &str, signer: &SeedSigner, at_: &str) -> Result<AdmittedAuthority, AuthorityError> {
    let revocation = no_revocation();
    verify(credential, &keys(signer), &Admission::new(at(at_), &revocation).expecting(AUD))
}

pub fn admit_denying(credential: &str, signer: &SeedSigner, ids: &[&str]) -> Result<AdmittedAuthority, AuthorityError> {
    let mut denylist = Denylist::default();
    for id in ids {
        denylist.deny(id, "k1");
    }
    let revocation = RevocationState { denylist, ..RevocationState::default() };
    verify(credential, &keys(signer), &Admission::new(at(DURING), &revocation).expecting(AUD))
}

/// The authority block [`minted`] carries, for crafting variants of it.
pub fn block(signer: &SeedSigner) -> AuthorityBlock {
    authority_block(&plan(signer), &MintClaims::default())
}

/// An authority block signed by `signer` holding `block`'s facts except those named in
/// `drop`, plus the datalog `extra`.
pub fn craft(signer: &SeedSigner, block: &AuthorityBlock, drop: &[&str], extra: &str) -> String {
    let mut builder = BiscuitBuilder::new();
    for f in authority_facts(block).unwrap() {
        if !drop.contains(&f.predicate.name.as_str()) {
            builder = builder.fact(f).unwrap();
        }
    }
    if !extra.is_empty() {
        builder = builder.code(extra).unwrap();
    }
    builder.build(signer.key_pair()).unwrap().to_base64().unwrap()
}

/// Append a block holding the datalog `code`, bypassing the holder-side checks.
pub fn append_raw(credential: &str, code: &str) -> String {
    let block = BlockBuilder::new().code(code).unwrap();
    UnverifiedBiscuit::from_base64(credential).unwrap().append(block).unwrap().to_base64().unwrap()
}

pub fn err_name(e: &AuthorityError) -> String {
    e.to_string().split(':').next().unwrap_or_default().to_string()
}

#[track_caller]
pub fn refused<T: std::fmt::Debug>(r: Result<T, AuthorityError>, error: &str) {
    match r {
        Err(e) => assert_eq!(err_name(&e), error, "{e}"),
        Ok(v) => panic!("expected {error}, admitted {v:?}"),
    }
}
