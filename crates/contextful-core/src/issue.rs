//! The persisted issuance policy and the mint-time checks over a request.

use crate::grant::{Action, Grant};
use crate::identify::Subject;
use crate::ports::{Clock, SigningPort};
use crate::time::Instant;
use crate::AuthorityError;
use serde::{Deserialize, Serialize};

/// Longest lifetime a persisted issuance policy permits: 24 h (`authority.issue.ceiling`).
pub const ISSUANCE_LIFETIME_CEILING_SECS: u64 = 24 * 60 * 60;

/// Rotation interval of the issuer signing key: 90 d (`authority.issue.key-rotation`).
pub const ISSUER_KEY_ROTATION_CADENCE_SECS: u64 = 90 * 24 * 60 * 60;

/// A persisted policy file that does not decode or breaks its bounds.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("policy malformed: {0}")]
    Malformed(String),
    #[error("max_lifetime_secs {0} exceeds the {ISSUANCE_LIFETIME_CEILING_SECS} s issuance bound")]
    CeilingAboveBound(u64),
    #[error(transparent)]
    Authority(#[from] AuthorityError),
}

/// A ceiling lowering: the value in force before it, and when it changed
/// (`authority.issue.ceiling-lowering`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CeilingLowering {
    pub previous_max_lifetime_secs: u64,
    pub at: Instant,
}

/// The project's issuance policy, persisted in version control at [`IssuancePolicy::PATH`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssuancePolicy {
    pub default_audience: String,
    pub max_lifetime_secs: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lowered: Vec<CeilingLowering>,
}

/// The signature scheme a credential names (`authority.issue.algorithm`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SignatureAlgorithm {
    #[default]
    Ed25519,
    /// ECDSA over P-256.
    #[serde(rename = "ES256")]
    Es256,
}

/// The encoding a signing port answers in, which names its scheme
/// (`authority.issue.signature-encoding`). The tag text is `ed25519`, `es256-der` or
/// `es256-raw`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignatureEncoding {
    /// A 32-byte key signing 64 bytes (RFC 8032).
    Ed25519,
    /// A SEC1 P-256 point; ECDSA over SHA-256 as an ASN.1 DER `Ecdsa-Sig-Value`.
    Es256Der,
    /// A SEC1 P-256 point; ECDSA over SHA-256 as the 64-byte `r ‖ s`.
    Es256Raw,
}

impl SignatureEncoding {
    /// The scheme the encoding signs under.
    pub fn scheme(self) -> SignatureAlgorithm {
        match self {
            SignatureEncoding::Ed25519 => SignatureAlgorithm::Ed25519,
            SignatureEncoding::Es256Der | SignatureEncoding::Es256Raw => SignatureAlgorithm::Es256,
        }
    }

    /// The encoding a credential and the audit chain store for `scheme`
    /// (`authority.issue.der-at-the-edge`).
    pub fn canonical(scheme: SignatureAlgorithm) -> SignatureEncoding {
        match scheme {
            SignatureAlgorithm::Ed25519 => SignatureEncoding::Ed25519,
            SignatureAlgorithm::Es256 => SignatureEncoding::Es256Der,
        }
    }

    /// The encoding a tag names, if any.
    pub fn parse(tag: &str) -> Option<SignatureEncoding> {
        match tag {
            "ed25519" => Some(SignatureEncoding::Ed25519),
            "es256-der" => Some(SignatureEncoding::Es256Der),
            "es256-raw" => Some(SignatureEncoding::Es256Raw),
            _ => None,
        }
    }
}

impl std::fmt::Display for SignatureEncoding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SignatureEncoding::Ed25519 => "ed25519",
            SignatureEncoding::Es256Der => "es256-der",
            SignatureEncoding::Es256Raw => "es256-raw",
        })
    }
}

/// Where a mint runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
    Primary,
    Replica,
}

/// What authorizes a mint.
#[derive(Debug, Clone, PartialEq)]
pub enum MintAuthority {
    /// The minter holds the signing material in the project tree, as the owner running
    /// `contextful token mint --issuer-key` does.
    Custody,
    /// A request presented to a minting face, carrying the requester's admitted grants.
    Presented(Vec<Grant>),
    /// A verified external assertion under the project's exchange policy.
    Exchange,
}

/// How a mint's lifetime is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifetime {
    /// No lifetime named: the ceiling.
    Default,
    /// An explicit request, refused above the ceiling.
    Requested(u64),
    /// A configured lifetime, clamped down to the ceiling.
    Clamped(u64),
}

/// One mint request.
#[derive(Debug, Clone, PartialEq)]
pub struct MintRequest {
    pub authority: MintAuthority,
    pub subject: Subject,
    pub grants: Vec<Grant>,
    pub lifetime: Lifetime,
    /// Absent: the policy's default audience.
    pub audience: Option<String>,
    pub issuer: Option<String>,
    /// A scheme the request names; absent takes the signing key's.
    pub algorithm: Option<SignatureAlgorithm>,
}

/// The environment a mint runs in.
pub struct MintContext<'a> {
    pub node: NodeRole,
    pub signer: &'a dyn SigningPort,
    pub clock: &'a dyn Clock,
}

/// What a checked request mints: the claims the credential format encodes and signs.
#[derive(Debug, Clone, PartialEq)]
pub struct MintPlan {
    pub subject: Subject,
    pub grants: Vec<Grant>,
    pub audience: String,
    pub issuer: Option<String>,
    pub algorithm: SignatureAlgorithm,
    pub issued_at: Instant,
    pub expires_at: Instant,
    pub lifetime_secs: u64,
}

impl IssuancePolicy {
    /// Where a project persists its issuance policy.
    pub const PATH: &'static str = ".contextful/issuance.toml";

    /// A grant naming no action mints these (`authority.issue.default-read`).
    pub const DEFAULT_ACTIONS: &'static [Action] = &[Action::Read];

    /// Decode a policy file, refusing a ceiling above 24 h or a zero ceiling.
    pub fn parse(text: &str) -> Result<IssuancePolicy, PolicyError> {
        let policy: IssuancePolicy =
            toml::from_str(text).map_err(|e| PolicyError::Malformed(format!("{}: {}", Self::PATH, e.message())))?;
        if policy.default_audience.trim().is_empty() {
            return Err(PolicyError::Malformed(format!("{}: default_audience is empty", Self::PATH)));
        }
        check_bound(policy.max_lifetime_secs)?;
        Ok(policy)
    }

    /// The policy file's text.
    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("an issuance policy serializes")
    }

    /// Set a new ceiling. Lowering records the value it replaces and the instant.
    pub fn lower_ceiling(&mut self, max_lifetime_secs: u64, at: Instant) -> Result<(), PolicyError> {
        check_bound(max_lifetime_secs)?;
        if max_lifetime_secs < self.max_lifetime_secs {
            self.lowered.push(CeilingLowering { previous_max_lifetime_secs: self.max_lifetime_secs, at });
        }
        self.max_lifetime_secs = max_lifetime_secs;
        Ok(())
    }

    /// The ceiling rotation-grace validation holds at `at`: the current one, or a recorded
    /// earlier one while a credential minted under it can still be live.
    pub fn effective_ceiling_secs(&self, at: Instant) -> u64 {
        self.lowered
            .iter()
            .filter(|l| at < l.at.plus_secs(l.previous_max_lifetime_secs))
            .map(|l| l.previous_max_lifetime_secs)
            .fold(self.max_lifetime_secs, u64::max)
    }

    /// Check a mint request and compute what it mints.
    pub fn check(&self, req: &MintRequest, ctx: &MintContext<'_>) -> Result<MintPlan, AuthorityError> {
        if ctx.node == NodeRole::Replica {
            return Err(AuthorityError::ReplicaCannotIssue(
                "a replica holds the issuer's public key and no signing material".into(),
            ));
        }
        if let MintAuthority::Presented(presented) = &req.authority {
            if !presented.iter().any(|g| g.actions.contains(&Action::Admin)) {
                return Err(AuthorityError::IssuanceUnauthorized("the mint request presents no admin grant".into()));
            }
        }
        if let Some(zone) = &req.subject.zone {
            if is_wildcard_zone(zone) {
                return Err(AuthorityError::IssuanceZoneWildcard(format!("the subject declares zone `{zone}`")));
            }
        }
        let grants: Vec<Grant> = req
            .grants
            .iter()
            .map(|g| {
                let mut g = g.clone();
                if g.actions.is_empty() {
                    g.actions = Self::DEFAULT_ACTIONS.to_vec();
                }
                g
            })
            .collect();
        let lands = grants.iter().flat_map(|g| &g.actions).find(|a| matches!(a, Action::Write | Action::Execute));
        let principal = req.subject.on_behalf_of.as_deref().is_some_and(|p| !p.is_empty());
        if let (Some(action), false) = (lands, principal) {
            return Err(AuthorityError::IssuancePrincipalRequired(format!(
                "a grant carrying `{}` needs a subject naming on_behalf_of",
                action_name(*action)
            )));
        }
        let algorithm = ctx.signer.algorithm();
        if let Some(named) = req.algorithm {
            if named != algorithm {
                return Err(AuthorityError::SignatureAlgorithmMismatch(format!(
                    "the request names {named}; the signing key is {algorithm}"
                )));
            }
        }
        let lifetime_secs = match req.lifetime {
            Lifetime::Default => self.max_lifetime_secs,
            Lifetime::Clamped(secs) => secs.min(self.max_lifetime_secs),
            Lifetime::Requested(secs) if secs > self.max_lifetime_secs => {
                return Err(AuthorityError::IssuanceLifetimeAboveCeiling(format!(
                    "{secs} s requested; the persisted ceiling is {} s",
                    self.max_lifetime_secs
                )))
            }
            Lifetime::Requested(secs) => secs,
        };
        let issued_at = ctx.clock.now();
        Ok(MintPlan {
            subject: req.subject.clone(),
            grants,
            audience: req.audience.clone().unwrap_or_else(|| self.default_audience.clone()),
            issuer: req.issuer.clone(),
            algorithm,
            issued_at,
            expires_at: issued_at.plus_secs(lifetime_secs),
            lifetime_secs,
        })
    }
}

impl MintRequest {
    /// A request from the holder of the signing material, at the default lifetime,
    /// audience and scheme.
    pub fn custody(subject: Subject, grants: Vec<Grant>) -> MintRequest {
        MintRequest {
            authority: MintAuthority::Custody,
            subject,
            grants,
            lifetime: Lifetime::Default,
            audience: None,
            issuer: None,
            algorithm: None,
        }
    }
}

impl SignatureAlgorithm {
    /// The scheme a credential's name denotes, if any.
    pub fn parse(name: &str) -> Option<SignatureAlgorithm> {
        match name {
            "Ed25519" => Some(SignatureAlgorithm::Ed25519),
            "ES256" => Some(SignatureAlgorithm::Es256),
            _ => None,
        }
    }

    /// Hold a credential's named scheme to its pinned key's (`authority.issue.algorithm-mismatch`).
    pub fn check_named(named: &str, pinned: SignatureAlgorithm) -> Result<SignatureAlgorithm, AuthorityError> {
        match SignatureAlgorithm::parse(named) {
            Some(scheme) if scheme == pinned => Ok(scheme),
            _ => Err(AuthorityError::SignatureAlgorithmMismatch(format!(
                "the credential names {named}; the pinned key is {pinned}"
            ))),
        }
    }
}

impl std::fmt::Display for SignatureAlgorithm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SignatureAlgorithm::Ed25519 => "Ed25519",
            SignatureAlgorithm::Es256 => "ES256",
        })
    }
}

/// Whether the issuer key rotated at `rotated_at` is due to rotate at `now`: every
/// [`ISSUER_KEY_ROTATION_CADENCE_SECS`], and at once on suspected compromise.
pub fn key_rotation_due(rotated_at: Instant, now: Instant, suspected_compromise: bool) -> bool {
    suspected_compromise || now >= rotated_at.plus_secs(ISSUER_KEY_ROTATION_CADENCE_SECS)
}

fn check_bound(max_lifetime_secs: u64) -> Result<(), PolicyError> {
    if max_lifetime_secs == 0 {
        return Err(PolicyError::Malformed("max_lifetime_secs is zero".into()));
    }
    if max_lifetime_secs > ISSUANCE_LIFETIME_CEILING_SECS {
        return Err(PolicyError::CeilingAboveBound(max_lifetime_secs));
    }
    Ok(())
}

/// `*`, or a zone category with a starred identifier such as `on-prem:*`.
fn is_wildcard_zone(zone: &str) -> bool {
    let zone = zone.trim();
    zone == "*" || zone.ends_with(":*")
}

fn action_name(action: Action) -> &'static str {
    match action {
        Action::Read => "read",
        Action::Write => "write",
        Action::Execute => "execute",
        Action::Admin => "admin",
    }
}

/// Who authors a table write when no credential accompanies it
/// (`authority.issue.authoring-posture`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthoringPosture {
    /// One verified ambient credential authors every write; a write without it refuses.
    Session,
    /// No ambient principal: a write without a credential lands unauthored.
    PerRequest,
}

impl AuthoringPosture {
    /// The manifest's top-level key (`authority.issue.posture-key`).
    pub const KEY: &'static str = "authoring_posture";

    /// The value the manifest key spells.
    pub fn as_str(self) -> &'static str {
        match self {
            AuthoringPosture::Session => "session",
            AuthoringPosture::PerRequest => "per_request",
        }
    }

    /// The posture `value` spells, or `None` for any other value.
    pub fn parse(value: &str) -> Option<AuthoringPosture> {
        [AuthoringPosture::Session, AuthoringPosture::PerRequest].into_iter().find(|p| p.as_str() == value)
    }

    /// The posture a manifest declares. The key has no default: absent, or naming any
    /// value but `session` or `per_request`, it raises `AuthoringPostureUndeclared`.
    pub fn from_manifest(text: &str) -> Result<AuthoringPosture, AuthorityError> {
        let undeclared = |why: String| {
            AuthorityError::AuthoringPostureUndeclared(format!(
                "{why}; declare `{key} = \"session\"` or `{key} = \"per_request\"` at the top of the manifest, \
                 or run `contextful init <name> --authoring-posture <session|per_request>`",
                key = Self::KEY
            ))
        };
        let value: toml::Value = toml::from_str(text).map_err(|e| undeclared(format!("the manifest does not parse: {}", e.message())))?;
        match value.get(Self::KEY) {
            None => Err(undeclared(format!("the manifest declares no `{}`", Self::KEY))),
            Some(other) => match other.as_str().and_then(AuthoringPosture::parse) {
                Some(posture) => Ok(posture),
                None => Err(undeclared(format!("`{}` is `{other}`", Self::KEY))),
            },
        }
    }

    /// Refuse a write `what` names that arrives with no credential under `session`
    /// (`authority.issue.session-credential`); under `per_request` it lands unauthored.
    pub fn unaccompanied(self, what: &str) -> Result<(), AuthorityError> {
        match self {
            AuthoringPosture::Session => Err(AuthorityError::AuthoringCredentialMissing(format!(
                "{what} runs under the `session` authoring posture and carries no credential"
            ))),
            AuthoringPosture::PerRequest => Ok(()),
        }
    }
}
