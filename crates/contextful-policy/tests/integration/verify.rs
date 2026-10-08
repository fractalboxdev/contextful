//! `authority.verify`: admission, the admitted-authority value, the effect boundary and
//! the credential-format interface.

use crate::support::*;
use contextful_core::grant::{Action, TablePattern};
use contextful_core::revoke::Denylist;
use contextful_policy::attenuate::Derivation;
use contextful_policy::issue::MintClaims;
use contextful_policy::revoke::{parse_denylist, RevocationState};
use contextful_core::ports::FixedClock;
use contextful_core::AuthorityError;
use contextful_core::issue::{IssuancePolicy, Lifetime, MintContext, MintRequest, NodeRole};
use contextful_policy::verify::BEARER_LIFETIME_SECS;
use contextful_policy::possession::{sign_proof, verify_proof, NonceCache, ProofRefusal, ProofRequest};
use contextful_policy::verify::{
    checkpoint_uid, effect_boundary, no_holder_proof, verify_inherited_pipe, verify_local, verify_network, Admission, BiscuitFormat,
    CredentialFormat, LocalTransport,
};
use std::os::unix::net::UnixStream;

/// Verification yields an admitted-authority value carrying the normalized subject tuple and its grants. Every read surface and row-landing effect takes that value as an argument; nothing downstream re-parses a credential or reads ambient state.
// spec: authority.verify.admitted-authority@6a9d4f19
#[test]
fn verification_yields_an_admitted_value_with_the_normalized_subject_and_grants() {
    let signer = issuer();
    let mut padded = dana();
    padded.task = Some("  ".into());
    let credential =
        contextful_policy::issue::mint(&plan_for(&signer, padded, vec![grant(&[Action::Read], &["research/*"])]), &MintClaims::default(), &signer)
            .unwrap();
    let admitted = admit(&credential, &signer, DURING).unwrap();
    assert_eq!(admitted.subject().on_behalf_of(), Some("user://dana@acme.example"));
    assert_eq!(admitted.subject().agent(), Some("agent://research-loop"));
    assert_eq!(admitted.subject().task(), None, "a blank member is dropped once, at normalization");
    assert_eq!(admitted.grants(), &[grant(&[Action::Read], &["research/*"])]);
    assert_eq!(admitted.audience(), AUD);
    assert_eq!(admitted.expires_at(), at("2030-01-01T00:15:00Z"));
    assert_eq!(admitted.key_version(), signer.public_key_text());
    let json = admitted.to_json();
    assert!(json.contains("user://dana@acme.example") && json.contains("research/*"), "{json}");
    // A downstream effect reads the carried value: the boundary takes no credential.
    let revocation = no_revocation();
    assert_eq!(effect_boundary(&admitted, &Admission::new(at(DURING), &revocation)), Ok(()));
}

/// A statement's start and each commit are effect boundaries. Each boundary re-reads expiry, revocation and policy version against the carried value; a lapse between boundaries stops the effect at the next one.
// spec: authority.verify.effect-boundary@b9728137
#[test]
fn each_effect_boundary_re_reads_expiry_revocation_and_profile_version() {
    let signer = issuer();
    let admitted = admit(&minted(&signer), &signer, DURING).unwrap();
    let clean = no_revocation();
    assert_eq!(effect_boundary(&admitted, &Admission::new(at("2030-01-01T00:10:00Z"), &clean)), Ok(()));
    // Expiry lapses between boundaries.
    refused(effect_boundary(&admitted, &Admission::new(at("2030-01-01T00:15:01Z"), &clean)), "AuthorityExpired");
    // A denylist entry lands between boundaries.
    let mut denylist = Denylist::default();
    denylist.deny(admitted.revocation_ids().last().unwrap(), "k1");
    let denied = RevocationState { denylist, ..RevocationState::default() };
    refused(effect_boundary(&admitted, &Admission::new(at(DURING), &denied)), "AuthorityRevoked");
    // An epoch bump lands between boundaries.
    let mut bumped = RevocationState::default();
    bumped.epochs.bump(contextful_core::revoke::EpochScope { project: AUD.into(), tenant: None, principal_class: None });
    refused(effect_boundary(&admitted, &Admission::new(at(DURING), &bumped)), "AuthorityRevoked");
    // The supported profile set moves between boundaries.
    let moved = Admission { profiles: &[2], ..Admission::new(at(DURING), &clean) };
    refused(effect_boundary(&admitted, &moved), "ProfileVersionUnsupported");
}

/// The credential format sits behind one interface of issue, attenuate, verify and introspect; no enforcement call site names a credential type.
// spec: authority.verify.format-interface@c50ea1e5
#[test]
fn issue_attenuate_verify_and_introspect_run_through_one_interface() {
    let format: &dyn CredentialFormat = &BiscuitFormat;
    let signer = issuer();
    let parent = format.issue(&plan(&signer), &MintClaims::default(), &signer).unwrap();
    let declared = format.introspect(&parent).unwrap();
    assert_eq!(declared.format, format.name());
    let child = format
        .attenuate(&parent, &Derivation::narrowing(&declared.authority.grants, None, Some(&[TablePattern::parse("research/filings").unwrap()])))
        .unwrap();
    let revocation = no_revocation();
    let admitted = format.verify(&child, &keys(&signer), &Admission::new(at(DURING), &revocation).expecting(AUD)).unwrap();
    assert_eq!(admitted.format(), format.name());
    assert!(admitted.permits(Action::Read, &["research/filings"]));
    assert!(!admitted.permits(Action::Read, &["research/notes"]));
}

/// A credential with any block signature failing against a pinned key raises `SignatureInvalid` and admits nothing, no verified prefix included.
// spec: authority.verify.bad-signature@a143346b
#[test]
fn any_failing_block_signature_admits_nothing_not_even_a_verified_prefix() {
    let signer = issuer();
    let parent = minted(&signer);
    let child = contextful_policy::attenuate::attenuate(
        &parent,
        &Derivation::narrowing(&[grant(&[Action::Read], &["research/*"])], None, Some(&[TablePattern::parse("research/filings").unwrap()])),
    )
    .unwrap();
    // A key nobody pinned.
    refused(admit(&parent, &issuer(), DURING), "SignatureInvalid");
    // A tampered child block: its parent prefix verifies, and still nothing is admitted.
    let key = keys(&signer).keys().next().unwrap().public_key;
    let token = biscuit_auth::Biscuit::from_base64(&child, key).unwrap();
    let mut container = token.container().clone();
    // The child block carries the authority block's signature, which signs other bytes.
    container.blocks[0].signature = container.authority.signature.clone();
    let tampered = base64_url(&container.to_vec().unwrap());
    refused(admit(&tampered, &signer, DURING), "SignatureInvalid");
    // Flipped transmitted text, as the milestone flow tampers it.
    let mut bytes = child.clone().into_bytes();
    let mid = bytes.len() / 2;
    bytes[mid] = if bytes[mid] == b'A' { b'B' } else { b'A' };
    refused(admit(&String::from_utf8(bytes).unwrap(), &signer, DURING), "SignatureInvalid");
}

fn base64_url(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE.encode(bytes)
}

/// A checkpoint declaring an expected audience raises `AudienceMismatch` for a credential with a different audience or none; a checkpoint declaring none performs no audience check.
// spec: authority.verify.audience-mismatch@2e0b0171
#[test]
fn a_declared_audience_refuses_another_or_none_and_an_undeclared_one_checks_nothing() {
    let signer = issuer();
    let credential = minted(&signer);
    let revocation = no_revocation();
    let other = Admission::new(at(DURING), &revocation).expecting("contextful://other");
    refused(verify_inherited_pipe(&credential, &keys(&signer), &other), "AudienceMismatch");
    let none = craft(&signer, &block(&signer), &["aud"], "");
    refused(admit(&none, &signer, DURING), "AudienceMismatch");
    let undeclared = Admission::new(at(DURING), &revocation);
    assert!(verify_inherited_pipe(&credential, &keys(&signer), &undeclared).is_ok());
    assert!(verify_inherited_pipe(&none, &keys(&signer), &undeclared).is_ok());
}

/// A credential whose expiry precedes the evaluation instant raises `AuthorityExpired` at admission and at each later effect boundary.
// spec: authority.verify.expired@3fb3ae75
#[test]
fn a_credential_past_its_expiry_is_refused_at_admission_and_every_later_boundary() {
    let signer = issuer();
    let credential = minted(&signer);
    // Expiry is 2030-01-01T00:15:00Z; the instant itself still admits.
    assert!(admit(&credential, &signer, "2030-01-01T00:15:00Z").is_ok());
    refused(admit(&credential, &signer, "2030-01-01T00:15:01Z"), "AuthorityExpired");
    // A child's own earlier expiry governs the chain.
    let child = contextful_policy::attenuate::attenuate(
        &credential,
        &Derivation { expires_at: Some(at("2030-01-01T00:06:00Z")), ..Derivation::default() },
    )
    .unwrap();
    let admitted = admit(&child, &signer, DURING).unwrap();
    refused(admit(&child, &signer, "2030-01-01T00:07:00Z"), "AuthorityExpired");
    let revocation = no_revocation();
    for later in ["2030-01-01T00:07:00Z", "2030-01-01T00:20:00Z"] {
        refused(effect_boundary(&admitted, &Admission::new(at(later), &revocation)), "AuthorityExpired");
    }
}

#[test]
fn a_credential_naming_no_subject_member_admits_nothing() {
    let signer = issuer();
    let credential = craft(&signer, &block(&signer), &["sub", "att"], "");
    refused(admit(&credential, &signer, DURING), "AuthoritySubjectMissing");
}

#[test]
fn a_credential_naming_another_scheme_than_its_pinned_key_is_refused() {
    let signer = issuer();
    let credential = craft(&signer, &block(&signer), &["alg"], "alg(\"ES256\");");
    refused(admit(&credential, &signer, DURING), "SignatureAlgorithmMismatch");
}

#[test]
fn a_timestamp_outside_the_grammar_is_refused() {
    let signer = issuer();
    refused(admit(&craft(&signer, &block(&signer), &["exp"], "exp(\"soon\");"), &signer, DURING), "TimestampMalformed");
}

/// The milestone-1 flow, through the API a command line wires up.
#[test]
fn the_authority_core_flow_admits_narrows_and_re_reads() {
    let signer = issuer();
    let parent = minted(&signer);
    let admitted = admit(&parent, &signer, DURING).unwrap().to_json();
    assert!(admitted.contains("user://dana@acme.example") && admitted.contains("research/*"));
    let grants = contextful_policy::verify::introspect(&parent).unwrap().authority.grants;
    let child =
        contextful_policy::attenuate::attenuate(&parent, &Derivation::narrowing(&grants, None, Some(&[TablePattern::parse("research/filings").unwrap()])))
            .unwrap();
    assert!(admit(&child, &signer, DURING).unwrap().to_json().contains("research/filings"));
    refused(admit(&child, &signer, "2030-01-01T00:20:00Z"), "AuthorityExpired");
    let rev = contextful_policy::verify::introspect(&child).unwrap().rev_id;
    let revocation = RevocationState { denylist: parse_denylist(&format!("{rev}\n"), "k1"), ..RevocationState::default() };
    let admission = Admission::new(at(DURING), &revocation).expecting(AUD);
    refused(verify_inherited_pipe(&child, &keys(&signer), &admission), "AuthorityRevoked");
    assert!(verify_inherited_pipe(&parent, &keys(&signer), &admission).is_ok());
}

/// A credential bound to `holder`'s key, and the request and proof a local client sends.
fn key_bound(signer: &contextful_policy::issue::SeedSigner, holder: &ed25519_dalek::SigningKey) -> String {
    let jkt = contextful_policy::possession::jwk_thumbprint(holder.verifying_key().as_bytes());
    contextful_policy::issue::mint(&plan(signer), &MintClaims { confirmation: Some(jkt), epoch: 0, ..MintClaims::default() }, signer).unwrap()
}

const LOCAL_TARGET: &str = "unix:///run/contextful.sock/v1/query";

fn local_request() -> ProofRequest<'static> {
    ProofRequest { method: "POST", target: LOCAL_TARGET, body: b"{\"sql\":\"select 1\"}" }
}

fn holder_proof<'a>(proof: &'a str, nonces: &'a mut NonceCache) -> impl FnOnce(&str) -> Result<(), ProofRefusal> + 'a {
    move |jkt| verify_proof(jkt, proof, &local_request(), &FixedClock(at(DURING)), nonces)
}

#[track_caller]
fn refused_proof<T: std::fmt::Debug>(r: Result<T, ProofRefusal>, error: &str) {
    match r {
        Err(ProofRefusal::Refused(e)) => assert_eq!(err_name(&e), error, "{e}"),
        other => panic!("expected {error}, got {other:?}"),
    }
}

fn own_socket(connection: u64) -> (LocalTransport, UnixStream) {
    let (checkpoint_end, client_end) = UnixStream::pair().unwrap();
    (LocalTransport::socket(connection, &checkpoint_end), client_end)
}

/// A local transport is a stdio pipe the checkpoint inherited from the process that spawned it, or a Unix socket; every other transport is a network transport.
// spec: authority.verify.local-transport@1976a641
#[test]
fn a_local_transport_is_the_inherited_pipe_or_a_unix_socket_reporting_its_peer_uid() {
    let (socket, _client) = own_socket(1);
    assert_eq!(socket, LocalTransport::Socket { connection: 1, peer_uid: Some(checkpoint_uid()) });
    assert_ne!(LocalTransport::InheritedPipe, socket);
}

/// On a local transport, a credential whose confirmation claim holds a thumbprint admits only with a proof as {{authority.verify.possession-binding}}; the peer fallback never applies to it, and a missing or failing proof is refused as {{authority.verify.possession-invalid}}.
// spec: authority.verify.local-holder-proof@6d88ae5d
#[test]
fn a_key_bound_credential_admits_locally_only_with_its_holder_proof() {
    let signer = issuer();
    let holder = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let credential = key_bound(&signer, &holder);
    let revocation = no_revocation();
    let admission = Admission::new(at(DURING), &revocation).expecting(AUD);
    let (socket, _client) = own_socket(1);
    for transport in [LocalTransport::InheritedPipe, socket] {
        // No proof: the peer fallback does not stand in for the key.
        refused_proof(verify_local(&credential, &keys(&signer), &admission, transport, no_holder_proof), "PossessionProofInvalid");
        // A thief's proof.
        let thief = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
        let mut nonces = NonceCache::new();
        let stolen = sign_proof(&thief, &local_request(), at(DURING), "n-thief");
        refused_proof(verify_local(&credential, &keys(&signer), &admission, transport, holder_proof(&stolen, &mut nonces)), "PossessionProofInvalid");
        // The holder's proof.
        let mut nonces = NonceCache::new();
        let proof = sign_proof(&holder, &local_request(), at(DURING), "n-holder");
        let admitted = verify_local(&credential, &keys(&signer), &admission, transport, holder_proof(&proof, &mut nonces)).unwrap();
        assert_eq!(admitted.authority().confirmation(), Some(contextful_policy::possession::jwk_thumbprint(holder.verifying_key().as_bytes()).as_str()));
    }
    // The inherited-pipe shorthand carries no proof, so a key-bound credential refuses through it.
    refused(verify_inherited_pipe(&credential, &keys(&signer), &admission), "PossessionProofInvalid");
}

/// On a local transport, a credential with no confirmation claim admits only through the operating system's peer authentication: an inherited stdio pipe, or a socket peer whose kernel-reported uid equals the checkpoint process's uid.
// spec: authority.verify.local-peer-fallback@02d72015
#[test]
fn a_credential_binding_no_key_admits_through_the_inherited_pipe_or_a_same_uid_socket_peer() {
    let signer = issuer();
    let credential = minted(&signer);
    let revocation = no_revocation();
    let admission = Admission::new(at(DURING), &revocation).expecting(AUD);
    let piped = verify_local(&credential, &keys(&signer), &admission, LocalTransport::InheritedPipe, no_holder_proof::<AuthorityError>).unwrap();
    assert_eq!(piped.authority().confirmation(), None);
    assert!(verify_inherited_pipe(&credential, &keys(&signer), &admission).is_ok());
    let (socket, _client) = own_socket(3);
    let socketed = verify_local(&credential, &keys(&signer), &admission, socket, no_holder_proof::<AuthorityError>).unwrap();
    assert_eq!(socketed.transport(), socket);
    // The fallback admits after every other admission check: an expired credential still refuses.
    let late = Admission::new(at("2030-01-01T00:20:00Z"), &revocation).expecting(AUD);
    refused(verify_local(&credential, &keys(&signer), &late, socket, no_holder_proof::<AuthorityError>), "AuthorityExpired");
}

/// A socket peer presenting a credential with no confirmation claim, whose kernel-reported uid differs from the checkpoint process's uid or which the platform cannot report, raises `TransportPeerMismatch` and admits nothing.
// spec: authority.verify.peer-mismatch@53b4e181
#[test]
fn a_socket_peer_of_another_or_an_unreported_uid_admits_nothing() {
    let signer = issuer();
    let credential = minted(&signer);
    let revocation = no_revocation();
    let admission = Admission::new(at(DURING), &revocation).expecting(AUD);
    let other = checkpoint_uid().wrapping_add(1);
    for peer_uid in [Some(other), None] {
        let transport = LocalTransport::Socket { connection: 4, peer_uid };
        refused(verify_local(&credential, &keys(&signer), &admission, transport, no_holder_proof::<AuthorityError>), "TransportPeerMismatch");
    }
}

/// A confirmation claim holds a client public-key thumbprint. Each request under a credential carrying one bears a proof signed by the matching private key over method, target, body digest, issue instant and nonce; a credential carrying none requires no proof.
// spec: authority.verify.possession-binding@56c181db
#[test]
fn a_network_checkpoint_requires_a_proof_exactly_when_the_credential_binds_a_key() {
    let signer = issuer();
    let holder = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let bound = key_bound(&signer, &holder);
    let revocation = no_revocation();
    let admission = Admission::new(at(DURING), &revocation).expecting(AUD);
    // A key-bound credential presenting no proof admits nothing.
    refused(verify_network(&bound, &keys(&signer), &admission, no_holder_proof::<AuthorityError>), "PossessionProofInvalid");
    // A key-bound credential under its holder's proof admits.
    let mut nonces = NonceCache::new();
    let proof = sign_proof(&holder, &local_request(), at(DURING), "n-network");
    let admitted = verify_network(&bound, &keys(&signer), &admission, holder_proof(&proof, &mut nonces)).unwrap();
    assert!(admitted.confirmation().is_some());
    // A credential binding no key never reaches the proof check.
    let unbound = minted(&signer);
    let admitted = verify_network(&unbound, &keys(&signer), &admission, |_| -> Result<(), AuthorityError> { panic!("a bearer is asked for no proof") }).unwrap();
    assert_eq!(admitted.confirmation(), None);
}

/// A bearer credential for `AUD`, minted at [`MINTED`] to live `ttl` seconds.
fn bearer(signer: &contextful_policy::issue::SeedSigner, ttl: u64) -> String {
    let policy = IssuancePolicy::parse(&format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 86400\n")).unwrap();
    let mut req = MintRequest::custody(dana(), vec![grant(&[Action::Read], &["research/*"])]);
    req.lifetime = Lifetime::Requested(ttl);
    let clock = FixedClock(at(MINTED));
    let plan = policy.check(&req, &MintContext { node: NodeRole::Primary, signer, clock: &clock }).unwrap();
    contextful_policy::issue::mint(&plan, &MintClaims::default(), signer).unwrap()
}

/// A network checkpoint admits a credential with no confirmation claim as a bearer when it names the checkpoint's declared audience and meets {{authority.verify.bearer-lifetime}}; the peer fallback never applies over a network.
// spec: authority.verify.network-bearer@131e1f37
#[test]
fn a_network_checkpoint_admits_an_audience_bound_bearer_with_no_proof() {
    let signer = issuer();
    let credential = bearer(&signer, 3600);
    let revocation = no_revocation();
    let admission = Admission::new(at(DURING), &revocation).expecting(AUD);
    let admitted = verify_network(&credential, &keys(&signer), &admission, no_holder_proof::<AuthorityError>).unwrap();
    assert_eq!((admitted.confirmation(), admitted.audience()), (None, AUD));
    // Another audience, or a checkpoint declaring none, admits no bearer.
    let elsewhere = Admission::new(at(DURING), &revocation).expecting("contextful://globex");
    refused(verify_network(&credential, &keys(&signer), &elsewhere, no_holder_proof::<AuthorityError>), "AudienceMismatch");
    let undeclared = Admission::new(at(DURING), &revocation);
    refused(verify_network(&credential, &keys(&signer), &undeclared, no_holder_proof::<AuthorityError>), "AudienceMismatch");
    // Every other admission check still applies: an expired bearer refuses.
    let late = Admission::new(at("2030-01-01T01:00:01Z"), &revocation).expecting(AUD);
    refused(verify_network(&credential, &keys(&signer), &late, no_holder_proof::<AuthorityError>), "AuthorityExpired");
}

/// A bearer whose expiry falls more than 3600 s after its issue instant raises `BearerLifetimeExceeded` at a network checkpoint and admits nothing; its holder refreshes through {{authority.exchange.surface}}.
// spec: authority.verify.bearer-lifetime@c0a6f9b6
#[test]
fn a_bearer_living_past_3600_s_admits_nothing_over_a_network() {
    let signer = issuer();
    let revocation = no_revocation();
    let admission = Admission::new(at(DURING), &revocation).expecting(AUD);
    assert!(verify_network(&bearer(&signer, BEARER_LIFETIME_SECS), &keys(&signer), &admission, no_holder_proof::<AuthorityError>).is_ok());
    let long = bearer(&signer, BEARER_LIFETIME_SECS + 1);
    refused(verify_network(&long, &keys(&signer), &admission, no_holder_proof::<AuthorityError>), "BearerLifetimeExceeded");
    // The same credential still admits locally, where the ceiling does not apply.
    assert!(verify_inherited_pipe(&long, &keys(&signer), &admission).is_ok());
    // A key-bound credential is held to the issuance ceiling alone.
    let holder = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
    let policy = IssuancePolicy::parse(&format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 86400\n")).unwrap();
    let mut req = MintRequest::custody(dana(), vec![grant(&[Action::Read], &["research/*"])]);
    req.lifetime = Lifetime::Requested(86400);
    let clock = FixedClock(at(MINTED));
    let plan = policy.check(&req, &MintContext { node: NodeRole::Primary, signer: &signer, clock: &clock }).unwrap();
    let jkt = contextful_policy::possession::jwk_thumbprint(holder.verifying_key().as_bytes());
    let day = contextful_policy::issue::mint(&plan, &MintClaims { confirmation: Some(jkt), epoch: 0, ..MintClaims::default() }, &signer).unwrap();
    let mut nonces = NonceCache::new();
    let proof = sign_proof(&holder, &local_request(), at(DURING), "n-day");
    assert!(verify_network(&day, &keys(&signer), &admission, holder_proof(&proof, &mut nonces)).is_ok());
}

/// A local admission binds to the pipe or socket connection that presented the credential; a request on any other connection is refused as {{authority.verify.peer-mismatch}} until that connection presents the credential and admits itself.
// spec: authority.verify.connection-scoped@bb49fb3d
#[test]
fn a_local_admission_answers_only_on_the_connection_that_presented_the_credential() {
    let signer = issuer();
    let credential = minted(&signer);
    let revocation = no_revocation();
    let admission = Admission::new(at(DURING), &revocation).expecting(AUD);
    let (first, _a) = own_socket(1);
    let (second, _b) = own_socket(2);
    let admitted = verify_local(&credential, &keys(&signer), &admission, first, no_holder_proof::<AuthorityError>).unwrap();
    assert!(admitted.on(&first).is_ok());
    // A second connection from the same uid relays a request under the first admission.
    refused(admitted.on(&second), "TransportPeerMismatch");
    refused(admitted.on(&LocalTransport::InheritedPipe), "TransportPeerMismatch");
    // The second connection presents the credential itself and admits on its own.
    let own = verify_local(&credential, &keys(&signer), &admission, second, no_holder_proof::<AuthorityError>).unwrap();
    assert!(own.on(&second).is_ok());
    let piped = verify_local(&credential, &keys(&signer), &admission, LocalTransport::InheritedPipe, no_holder_proof::<AuthorityError>).unwrap();
    refused(piped.on(&first), "TransportPeerMismatch");
}

const OFF_TRANSPORT_SEED: u64 = 0x5eed_0057;
const OFF_TRANSPORT_ROUNDS: u64 = 32;

/// A credential binding no key, presented off the local transport that admits it — from a
/// socket peer of another or an unreported uid, or on a connection other than the admitted
/// one — admits nothing, over a seeded loop.
#[test]
fn a_credential_binding_no_key_admits_nothing_off_its_local_transport_over_a_seeded_loop() {
    let signer = issuer();
    let revocation = no_revocation();
    let admission = Admission::new(at(DURING), &revocation).expecting(AUD);
    let mut state = OFF_TRANSPORT_SEED;
    let mut next = move |bound: u64| {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (state >> 33) % bound
    };
    let (mut admitted, mut attempts) = (0u64, 0u64);
    let mut count = |ok: bool| {
        attempts += 1;
        admitted += ok as u64;
    };
    let own = checkpoint_uid();
    for round in 0..OFF_TRANSPORT_ROUNDS {
        let credential = minted(&signer);
        let connection = next(1 << 20);
        let stranger = own.wrapping_add(1 + next(u32::MAX as u64 - 1) as u32);
        for peer_uid in [Some(stranger), None] {
            let transport = LocalTransport::Socket { connection, peer_uid };
            count(verify_local(&credential, &keys(&signer), &admission, transport, no_holder_proof::<AuthorityError>).is_ok());
        }
        let home = LocalTransport::Socket { connection, peer_uid: Some(own) };
        let local = verify_local(&credential, &keys(&signer), &admission, home, no_holder_proof::<AuthorityError>)
            .unwrap_or_else(|e| panic!("round {round}: {e}"));
        let elsewhere = LocalTransport::Socket { connection: connection + 1 + next(1 << 20), peer_uid: Some(own) };
        count(local.on(&elsewhere).is_ok());
        count(local.on(&LocalTransport::InheritedPipe).is_ok());
    }
    contextful_eval::record::emit("bearer-transport-bound", admitted as f64, attempts, OFF_TRANSPORT_SEED);
    assert_eq!(admitted, 0, "{admitted} of {attempts} off-transport presentations admitted");
}
