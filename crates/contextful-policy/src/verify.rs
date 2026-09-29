//! `authority.verify`: admission at a checkpoint, the admitted-authority value, the
//! effect boundary, and the one credential-format interface.
//!
//! Admission runs in the order of the admission flowchart in `spec/50-authority.md`:
//! format withdrawal, every block signature against a pinned key, the profile and its
//! evaluator bound, the credential's scheme against the verifying key's, audience, the
//! chain's narrowing, timestamps and expiry, the possession proof, revocation, and a
//! subject member.

use crate::attenuate::Derivation;
use crate::issue::MintClaims;
use crate::keyset::KeySet;
use crate::profile::{read_chain, Chain, Hop, SUPPORTED_PROFILE_VERSIONS};
use crate::revoke::RevocationState;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use base64::engine::DecodePaddingMode;
use base64::Engine;
use biscuit_auth::Biscuit;
use contextful_core::claims::AuthorityBlock;
use contextful_core::grant::{Action, Grant};
use contextful_core::identify::NormalizedSubject;
use contextful_core::issue::{MintPlan, SignatureAlgorithm};
use contextful_core::ports::SigningPort;
use contextful_core::time::Instant;
use contextful_core::AuthorityError;
use serde::Serialize;

/// The name of the one credential format: the delegation profile over biscuit.
pub const BISCUIT_FORMAT: &str = "biscuit";

/// The surface name `AuthoritySubjectMissing` reports at admission.
const ADMISSION_SURFACE: &str = "the checkpoint";

/// The library's encoding: URL-safe base64, padded on output, either way on input.
pub(crate) const TOKEN_BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::URL_SAFE,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// What a checkpoint admits against: the expected audience, the evaluation instant, the
/// revocation state and the supported profile versions.
#[derive(Debug, Clone, Copy)]
pub struct Admission<'a> {
    /// `None` performs no audience check (`authority.verify.audience-mismatch`).
    pub audience: Option<&'a str>,
    pub at: Instant,
    pub revocation: &'a RevocationState,
    pub profiles: &'a [i64],
}

impl<'a> Admission<'a> {
    /// Admission at `at` under `revocation`, checking no audience, over the supported
    /// profile versions.
    pub fn new(at: Instant, revocation: &'a RevocationState) -> Admission<'a> {
        Admission { audience: None, at, revocation, profiles: SUPPORTED_PROFILE_VERSIONS }
    }

    /// The same admission expecting `audience`.
    pub fn expecting(self, audience: &'a str) -> Admission<'a> {
        Admission { audience: Some(audience), ..self }
    }
}

/// The authority a verification admitted: the normalized subject and its grants, and
/// what each effect boundary re-reads. Its fields are private and no function outside
/// this module constructs one, so a value exists only after a verification
/// (`authority.verify.no-bypass-constructor`):
///
/// ```compile_fail
/// let forged = contextful_policy::verify::AdmittedAuthority { grants: Vec::new() };
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct AdmittedAuthority {
    subject: NormalizedSubject,
    grants: Vec<Grant>,
    issuer: String,
    audience: String,
    issued_at: Instant,
    expires_at: Instant,
    confirmation: Option<String>,
    credential_id: String,
    revocation_ids: Vec<String>,
    epoch: u64,
    profile: i64,
    key_version: String,
    format: &'static str,
}

impl AdmittedAuthority {
    pub fn subject(&self) -> &NormalizedSubject {
        &self.subject
    }
    pub fn grants(&self) -> &[Grant] {
        &self.grants
    }
    pub fn issuer(&self) -> &str {
        &self.issuer
    }
    pub fn audience(&self) -> &str {
        &self.audience
    }
    pub fn issued_at(&self) -> Instant {
        self.issued_at
    }
    pub fn expires_at(&self) -> Instant {
        self.expires_at
    }
    /// The holder key thumbprint the chain-final block binds.
    pub fn confirmation(&self) -> Option<&str> {
        self.confirmation.as_deref()
    }
    /// The authority block's `rev.id`.
    pub fn credential_id(&self) -> &str {
        &self.credential_id
    }
    /// One revocation identifier per derivation, root first.
    pub fn revocation_ids(&self) -> &[String] {
        &self.revocation_ids
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn profile(&self) -> i64 {
        self.profile
    }
    /// The version of the pinned key the chain verified under.
    pub fn key_version(&self) -> &str {
        &self.key_version
    }
    pub fn format(&self) -> &'static str {
        self.format
    }

    /// Whether one grant carries `action` over every table in `tables`: the scoped
    /// session never flattens actions and tables into independent allowlists
    /// (`authority.profile.scoped-session`).
    pub fn permits(&self, action: Action, tables: &[&str]) -> bool {
        self.grants
            .iter()
            .any(|g| g.actions.contains(&action) && tables.iter().all(|t| g.tables.iter().any(|p| p.covers_name(t))))
    }

    /// The admitted value as JSON, for a surface to print.
    pub fn to_json(&self) -> String {
        #[derive(Serialize)]
        struct View<'a> {
            subject: contextful_core::identify::Subject,
            attestations: std::collections::BTreeMap<contextful_core::identify::Member, contextful_core::identify::Attestation>,
            grants: &'a [Grant],
            iss: &'a str,
            aud: &'a str,
            iat: String,
            exp: String,
            cnf: Option<&'a str>,
            rev_ids: &'a [String],
            epoch: u64,
            profile: i64,
            key_version: &'a str,
            format: &'a str,
        }
        serde_json::to_string_pretty(&View {
            subject: self.subject.to_subject(),
            attestations: self.subject.attestations(),
            grants: &self.grants,
            iss: &self.issuer,
            aud: &self.audience,
            iat: self.issued_at.to_rfc3339(),
            exp: self.expires_at.to_rfc3339(),
            cnf: self.confirmation(),
            rev_ids: &self.revocation_ids,
            epoch: self.epoch,
            profile: self.profile,
            key_version: &self.key_version,
            format: self.format,
        })
        .expect("an admitted authority serializes")
    }
}

/// Decode transmitted text into the library's bytes. Text that is not the encoding
/// carries no signature anybody made.
pub(crate) fn token_bytes(credential: &str) -> Result<Vec<u8>, AuthorityError> {
    TOKEN_BASE64
        .decode(credential.trim())
        .map_err(|e| AuthorityError::SignatureInvalid(format!("the credential is not a signed chain: {e}")))
}

/// A local transport (`authority.verify.local-transport`): the stdio pipe the checkpoint
/// inherited from the process that spawned it, or one accepted Unix socket connection.
/// Every other transport is a network transport, admitted through [`verify_network`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalTransport {
    InheritedPipe,
    /// The checkpoint's own number for the accepted connection, and the uid the kernel
    /// reports for its peer; `None` when the platform reports none.
    Socket { connection: u64, peer_uid: Option<u32> },
}

impl LocalTransport {
    /// The accepted connection `stream`, numbered `connection` by the checkpoint, with the
    /// peer uid the kernel reports: `SO_PEERCRED` on Linux, `getpeereid` elsewhere.
    pub fn socket(connection: u64, stream: &std::os::unix::net::UnixStream) -> LocalTransport {
        LocalTransport::Socket { connection, peer_uid: peer_uid(stream) }
    }
}

#[cfg(target_os = "linux")]
fn peer_uid(stream: &std::os::unix::net::UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred` and `len` are valid for writes of the sizes passed.
    let rc = unsafe {
        libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&mut cred as *mut libc::ucred).cast(), &mut len)
    };
    (rc == 0 && len as usize == std::mem::size_of::<libc::ucred>()).then_some(cred.uid)
}

#[cfg(not(target_os = "linux"))]
fn peer_uid(stream: &std::os::unix::net::UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let (mut uid, mut gid) = (0 as libc::uid_t, 0 as libc::gid_t);
    // SAFETY: `uid` and `gid` are valid for writes.
    let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    (rc == 0).then_some(uid)
}

/// The checkpoint process's effective uid, the one a socket peer's must equal
/// (`authority.verify.local-peer-fallback`).
pub fn checkpoint_uid() -> u32 {
    // SAFETY: `geteuid` reads process state and cannot fail.
    unsafe { libc::geteuid() }
}

/// An admission on a local transport, bound to the connection that presented the
/// credential (`authority.verify.connection-scoped`).
#[derive(Debug, Clone, PartialEq)]
pub struct LocalAdmission {
    authority: AdmittedAuthority,
    transport: LocalTransport,
}

impl LocalAdmission {
    pub fn authority(&self) -> &AdmittedAuthority {
        &self.authority
    }
    pub fn transport(&self) -> LocalTransport {
        self.transport
    }
    pub fn into_authority(self) -> AdmittedAuthority {
        self.authority
    }

    /// The admitted authority for a request arriving on `transport`; a request on any
    /// other connection raises `TransportPeerMismatch` until that connection admits itself.
    pub fn on(&self, transport: &LocalTransport) -> Result<&AdmittedAuthority, AuthorityError> {
        if *transport == self.transport {
            Ok(&self.authority)
        } else {
            Err(AuthorityError::TransportPeerMismatch(format!(
                "the credential was admitted on {:?}; a request on {transport:?} presents it afresh",
                self.transport
            )))
        }
    }
}

/// The holder-proof check of a local client that presents none: a credential binding a
/// holder key refuses with `PossessionProofInvalid`.
pub fn no_holder_proof<E: From<AuthorityError>>(_jkt: &str) -> Result<(), E> {
    Err(AuthorityError::PossessionProofInvalid("the credential binds a holder key and the request carries no proof".into()).into())
}

/// Admit a credential on a local transport: a credential binding a holder key admits only
/// through `holder_proof` (`authority.verify.local-holder-proof`); one binding none admits
/// through the inherited pipe or a socket peer of the checkpoint's uid
/// (`authority.verify.local-peer-fallback`), and a socket peer of another or an unreported
/// uid raises `TransportPeerMismatch` (`authority.verify.peer-mismatch`).
pub fn verify_local<E: From<AuthorityError>>(
    credential: &str,
    keys: &KeySet,
    admission: &Admission<'_>,
    transport: LocalTransport,
    holder_proof: impl FnOnce(&str) -> Result<(), E>,
) -> Result<LocalAdmission, E> {
    let authority = admit(credential, keys, admission, |cnf| match cnf {
        Some(jkt) => holder_proof(jkt),
        None => peer_fallback(transport).map_err(E::from),
    })?;
    Ok(LocalAdmission { authority, transport })
}

fn peer_fallback(transport: LocalTransport) -> Result<(), AuthorityError> {
    match transport {
        LocalTransport::InheritedPipe => Ok(()),
        LocalTransport::Socket { peer_uid: Some(uid), .. } if uid == checkpoint_uid() => Ok(()),
        LocalTransport::Socket { peer_uid: Some(uid), .. } => Err(AuthorityError::TransportPeerMismatch(format!(
            "the socket peer runs as uid {uid}; the checkpoint runs as uid {}",
            checkpoint_uid()
        ))),
        LocalTransport::Socket { peer_uid: None, .. } => {
            Err(AuthorityError::TransportPeerMismatch("the platform reports no uid for the socket peer".into()))
        }
    }
}

/// Admit a credential presented through the stdio pipe the checkpoint inherited, with no
/// holder proof: the admission a spawned tool server makes of its one credential.
pub fn verify_inherited_pipe(credential: &str, keys: &KeySet, admission: &Admission<'_>) -> Result<AdmittedAuthority, AuthorityError> {
    verify_local(credential, keys, admission, LocalTransport::InheritedPipe, no_holder_proof).map(LocalAdmission::into_authority)
}

/// Longest span from issue to expiry of a bearer a network checkpoint admits
/// (`authority.verify.bearer-lifetime`).
pub const BEARER_LIFETIME_SECS: u64 = 3600;

/// Admit a credential at a network checkpoint. A credential whose chain-final confirmation
/// claim holds a thumbprint admits only through `proof`, which receives the thumbprint and
/// runs the checkpoint's proof check (`authority.verify.possession-binding`). One binding
/// no key admits as a bearer, asked for no proof, when the checkpoint declares an audience
/// (`authority.verify.network-bearer`) and it lives at most [`BEARER_LIFETIME_SECS`] from
/// issue to expiry (`authority.verify.bearer-lifetime`).
pub fn verify_network<E: From<AuthorityError>>(
    credential: &str,
    keys: &KeySet,
    admission: &Admission<'_>,
    proof: impl FnOnce(&str) -> Result<(), E>,
) -> Result<AdmittedAuthority, E> {
    let admitted = admit(credential, keys, admission, |cnf| match cnf {
        Some(jkt) => proof(jkt),
        None => Ok(()),
    })?;
    if admitted.confirmation.is_none() {
        admit_bearer(&admitted, admission)?;
    }
    Ok(admitted)
}

fn admit_bearer(admitted: &AdmittedAuthority, admission: &Admission<'_>) -> Result<(), AuthorityError> {
    if admission.audience.is_none() {
        return Err(AuthorityError::AudienceMismatch(
            "the credential binds no holder key, and a network checkpoint admits a bearer only against a declared audience".into(),
        ));
    }
    let lifetime = admitted.expires_at.unix_secs() - admitted.issued_at.unix_secs();
    if lifetime > BEARER_LIFETIME_SECS as i64 {
        return Err(AuthorityError::BearerLifetimeExceeded(format!(
            "the bearer lives {lifetime} s from issue to expiry; a network checkpoint admits a bearer living at most \
             {BEARER_LIFETIME_SECS} s. Exchange for a short-lived credential, or mint with `--holder` and sign each request"
        )));
    }
    Ok(())
}

fn admit<E: From<AuthorityError>>(
    credential: &str,
    keys: &KeySet,
    admission: &Admission<'_>,
    possession: impl FnOnce(Option<&str>) -> Result<(), E>,
) -> Result<AdmittedAuthority, E> {
    admission.revocation.withdrawals.check(BISCUIT_FORMAT, admission.at)?;
    let bytes = token_bytes(credential)?;
    let (token, key_version, pinned) = keys
        .keys()
        .find_map(|k| Biscuit::from(&bytes, k.public_key).ok().map(|t| (t, k.version.clone(), k.algorithm())))
        .ok_or_else(|| AuthorityError::SignatureInvalid("no block chain verifies against a pinned key".into()))?;
    let chain = read_chain(&bytes, admission.profiles)?;
    crate::profile::evaluate(&token, admission.at)?;
    SignatureAlgorithm::check_named(&chain.authority.alg, pinned)?;
    let block = &chain.authority;
    if let Some(expected) = admission.audience {
        if block.aud != expected {
            return Err(AuthorityError::AudienceMismatch(format!(
                "the credential is for `{}`; this checkpoint expects `{expected}`",
                block.aud
            ))
            .into());
        }
    }
    let effective = chain.effective()?;
    let expires_at = Instant::from_unix_secs(effective.exp)?;
    if expires_at < admission.at {
        return Err(AuthorityError::AuthorityExpired(format!("expired at {expires_at}; evaluated at {}", admission.at)).into());
    }
    let confirmation = chain.confirmation().map(|c| c.jkt.clone());
    possession(confirmation.as_deref())?;
    let admitted = AdmittedAuthority {
        subject: effective.subject,
        grants: effective.grants,
        issuer: block.iss.clone(),
        audience: block.aud.clone(),
        issued_at: Instant::from_unix_secs(block.iat)?,
        expires_at,
        confirmation,
        credential_id: block.rev.id.clone(),
        revocation_ids: token.revocation_identifiers().iter().map(hex::encode).collect(),
        epoch: block.rev.epoch,
        profile: chain.version,
        key_version,
        format: BISCUIT_FORMAT,
    };
    admission.revocation.check(&admitted, admission.at)?;
    admitted.subject.require_member(ADMISSION_SURFACE)?;
    Ok(admitted)
}

/// Re-read expiry, revocation and the profile version against a carried value at an
/// effect boundary (`authority.verify.effect-boundary`). The boundary's audience is
/// not consulted: admission settled it.
pub fn effect_boundary(admitted: &AdmittedAuthority, boundary: &Admission<'_>) -> Result<(), AuthorityError> {
    if admitted.expires_at < boundary.at {
        return Err(AuthorityError::AuthorityExpired(format!(
            "expired at {}; the effect boundary is at {}",
            admitted.expires_at, boundary.at
        )));
    }
    boundary.revocation.check(admitted, boundary.at)?;
    if !boundary.profiles.contains(&admitted.profile) {
        return Err(AuthorityError::ProfileVersionUnsupported(format!(
            "profile {} is outside the supported set {:?}",
            admitted.profile, boundary.profiles
        )));
    }
    Ok(())
}

/// The scope a credential declares, read without evaluating it
/// (`authority.profile.declared-scope`): no signature, time, revocation or table policy
/// settles anything here.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Introspection {
    pub format: &'static str,
    pub profile: i64,
    pub authority: AuthorityBlock,
    pub blocks: Vec<Hop>,
    /// One revocation identifier per derivation, root first.
    pub revocation_ids: Vec<String>,
    /// The chain-final block's revocation identifier.
    pub rev_id: String,
}

impl Introspection {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("an introspection serializes")
    }
}

/// Read a credential's declared scope without verifying or evaluating it.
pub fn introspect(credential: &str) -> Result<Introspection, AuthorityError> {
    let bytes = token_bytes(credential)?;
    let chain: Chain = read_chain(&bytes, SUPPORTED_PROFILE_VERSIONS)?;
    let unverified = biscuit_auth::UnverifiedBiscuit::from(&bytes)
        .map_err(|e| AuthorityError::SignatureInvalid(format!("the credential is not a signed chain: {e}")))?;
    let revocation_ids: Vec<String> = unverified.revocation_identifiers().iter().map(hex::encode).collect();
    Ok(Introspection {
        format: BISCUIT_FORMAT,
        profile: chain.version,
        rev_id: revocation_ids.last().cloned().unwrap_or_default(),
        authority: chain.authority,
        blocks: chain.hops,
        revocation_ids,
    })
}

/// The one interface every surface reaches a credential through
/// (`authority.verify.format-interface`); no enforcement call site names a credential type.
pub trait CredentialFormat {
    fn name(&self) -> &'static str;
    fn issue(&self, plan: &MintPlan, claims: &MintClaims, signer: &dyn SigningPort) -> Result<String, AuthorityError>;
    fn attenuate(&self, credential: &str, derivation: &Derivation) -> Result<String, AuthorityError>;
    fn verify(&self, credential: &str, keys: &KeySet, admission: &Admission<'_>) -> Result<AdmittedAuthority, AuthorityError>;
    fn introspect(&self, credential: &str) -> Result<Introspection, AuthorityError>;
}

/// The delegation profile over biscuit, the one format.
#[derive(Debug, Clone, Copy, Default)]
pub struct BiscuitFormat;

impl CredentialFormat for BiscuitFormat {
    fn name(&self) -> &'static str {
        BISCUIT_FORMAT
    }
    fn issue(&self, plan: &MintPlan, claims: &MintClaims, signer: &dyn SigningPort) -> Result<String, AuthorityError> {
        crate::issue::mint(plan, claims, signer)
    }
    fn attenuate(&self, credential: &str, derivation: &Derivation) -> Result<String, AuthorityError> {
        crate::attenuate::attenuate(credential, derivation)
    }
    fn verify(&self, credential: &str, keys: &KeySet, admission: &Admission<'_>) -> Result<AdmittedAuthority, AuthorityError> {
        verify_inherited_pipe(credential, keys, admission)
    }
    fn introspect(&self, credential: &str) -> Result<Introspection, AuthorityError> {
        introspect(credential)
    }
}
