//! The subject tuple, attestation, mint hygiene, normalization and identity links.

use crate::AuthorityError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Longest subject value the mint accepts, in bytes (`authority.identify.value-hygiene`).
pub const SUBJECT_VALUE_BYTES: usize = 256;

/// A subject: the combination a credential binds and admits under
/// (`authority.identify.subject-tuple`). An absent member is `None`. This is the wire
/// shape; consumers read a [`NormalizedSubject`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subject {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<String>,
    #[serde(default)]
    pub incognito: bool,
}

/// A valued member of the subject tuple; the incognito flag is not a member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Member {
    OnBehalfOf,
    Agent,
    Host,
    Task,
    Zone,
}

impl Member {
    pub const ALL: [Member; 5] = [Member::OnBehalfOf, Member::Agent, Member::Host, Member::Task, Member::Zone];

    pub fn as_str(self) -> &'static str {
        match self {
            Member::OnBehalfOf => "on_behalf_of",
            Member::Agent => "agent",
            Member::Host => "host",
            Member::Task => "task",
            Member::Zone => "zone",
        }
    }

    /// How this member is known (`authority.identify.attestation`): the provider
    /// verifies `on_behalf_of`; every other member is asserted by its holder.
    pub fn attestation(self) -> Attestation {
        match self {
            Member::OnBehalfOf => Attestation::Verified,
            _ => Attestation::Asserted,
        }
    }
}

/// How a subject member is known (`authority.identify.attestation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Attestation {
    Verified,
    Asserted,
}

/// The mint path a subject value arrives through (`authority.identify.malformed-value`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MintSurface {
    /// The command-line mint trims a padded value.
    CommandLine,
    /// The automated exchange refuses a padded value.
    Exchange,
}

/// Check one subject value against mint hygiene (`authority.identify.value-hygiene`).
/// The refusal names the member and the failed rule, never the value.
pub fn check_value(member: Member, value: &str) -> Result<(), AuthorityError> {
    let fail = |rule: &str| Err(AuthorityError::AuthoritySubjectMalformed(format!("{} {rule}", member.as_str())));
    if value.is_empty() {
        return fail("is empty");
    }
    if value.len() > SUBJECT_VALUE_BYTES {
        return fail(&format!("exceeds {SUBJECT_VALUE_BYTES} B"));
    }
    if value.chars().any(char::is_control) {
        return fail("carries a control character");
    }
    if value.trim() != value {
        return fail("carries leading or trailing whitespace");
    }
    Ok(())
}

impl Subject {
    fn get(&self, member: Member) -> Option<&String> {
        match member {
            Member::OnBehalfOf => self.on_behalf_of.as_ref(),
            Member::Agent => self.agent.as_ref(),
            Member::Host => self.host.as_ref(),
            Member::Task => self.task.as_ref(),
            Member::Zone => self.zone.as_ref(),
        }
    }

    fn slot(&mut self, member: Member) -> &mut Option<String> {
        match member {
            Member::OnBehalfOf => &mut self.on_behalf_of,
            Member::Agent => &mut self.agent,
            Member::Host => &mut self.host,
            Member::Task => &mut self.task,
            Member::Zone => &mut self.zone,
        }
    }

    /// Check every present value at the mint (`authority.identify.malformed-value`),
    /// trimming first on the command line, then normalize.
    pub fn mint(mut self, surface: MintSurface) -> Result<NormalizedSubject, AuthorityError> {
        for member in Member::ALL {
            if let Some(value) = self.slot(member).as_mut() {
                if surface == MintSurface::CommandLine {
                    *value = value.trim().to_string();
                }
                check_value(member, value)?;
            }
        }
        Ok(self.normalize())
    }

    /// Normalize once (`authority.identify.normalization`): trim each value and drop each
    /// blank member. No scheme prefix is interpreted and no case is folded.
    pub fn normalize(mut self) -> NormalizedSubject {
        for member in Member::ALL {
            let slot = self.slot(member);
            *slot = slot.take().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        }
        NormalizedSubject(self)
    }
}

/// A subject after normalization, the only form consumers read. Constructed through
/// [`Subject::normalize`], [`Subject::mint`] or [`NormalizedSubject::derive`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NormalizedSubject(Subject);

impl NormalizedSubject {
    pub fn get(&self, member: Member) -> Option<&str> {
        self.0.get(member).map(String::as_str)
    }
    pub fn on_behalf_of(&self) -> Option<&str> {
        self.get(Member::OnBehalfOf)
    }
    pub fn agent(&self) -> Option<&str> {
        self.get(Member::Agent)
    }
    pub fn host(&self) -> Option<&str> {
        self.get(Member::Host)
    }
    pub fn task(&self) -> Option<&str> {
        self.get(Member::Task)
    }
    pub fn zone(&self) -> Option<&str> {
        self.get(Member::Zone)
    }
    pub fn incognito(&self) -> bool {
        self.0.incognito
    }

    /// The wire shape of this subject.
    pub fn to_subject(&self) -> Subject {
        self.0.clone()
    }

    /// The reader's identity: the verified `on_behalf_of` alone. An asserted member is
    /// never identity (`authority.identify.attestation`).
    pub fn identity(&self) -> Option<&str> {
        self.on_behalf_of()
    }

    /// Each present member's attestation, keyed as the authority block's `att`.
    pub fn attestations(&self) -> BTreeMap<Member, Attestation> {
        Member::ALL.into_iter().filter(|m| self.get(*m).is_some()).map(|m| (m, m.attestation())).collect()
    }

    /// Refuse a subject carrying no member (`authority.identify.subject-missing`), naming
    /// the refusing surface and echoing no credential value.
    pub fn require_member(&self, surface: &str) -> Result<(), AuthorityError> {
        if Member::ALL.into_iter().any(|m| self.get(m).is_some()) {
            Ok(())
        } else {
            Err(AuthorityError::AuthoritySubjectMissing(format!("{surface} admits no credential without a subject member")))
        }
    }

    /// The subject a derivation yields. A member the child names replaces the parent's
    /// and an absent one inherits it; a different `on_behalf_of` refuses
    /// (`authority.identify.subject-rebound`), and incognito turns on and never off
    /// (`authority.identify.incognito`).
    pub fn derive(&self, child: &SubjectDerivation) -> Result<NormalizedSubject, AuthorityError> {
        let proposed = Subject {
            on_behalf_of: child.on_behalf_of.clone(),
            agent: child.agent.clone(),
            host: child.host.clone(),
            task: child.task.clone(),
            zone: child.zone.clone(),
            incognito: false,
        }
        .normalize();
        if let Some(named) = proposed.on_behalf_of() {
            if self.on_behalf_of() != Some(named) {
                return Err(AuthorityError::AuthoritySubjectRebound(
                    "a derivation names an on_behalf_of different from its parent's".to_string(),
                ));
            }
        }
        if self.incognito() && child.incognito == Some(false) {
            return Err(AuthorityError::AttenuationWidens(
                "incognito: a derivation turns incognito off".to_string(),
            ));
        }
        let mut out = self.0.clone();
        for member in Member::ALL {
            if let Some(value) = proposed.get(member) {
                *out.slot(member) = Some(value.to_string());
            }
        }
        out.incognito = self.incognito() || child.incognito == Some(true);
        Ok(NormalizedSubject(out))
    }
}

/// The subject members a derivation names; `None` inherits the parent's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectDerivation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incognito: Option<bool>,
}

/// The method an identity link was established by (`authority.identify.unverified-link`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkMethod {
    ScimEmail,
    OidcSub,
    OperatorAsserted,
}

impl LinkMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            LinkMethod::ScimEmail => "scim_email",
            LinkMethod::OidcSub => "oidc_sub",
            LinkMethod::OperatorAsserted => "operator_asserted",
        }
    }

    /// Whether a link established this way authorizes.
    pub fn authorizes(self) -> bool {
        matches!(self, LinkMethod::ScimEmail | LinkMethod::OidcSub)
    }
}

/// A source principal mapped onto a subject (`authority.identify.identity-link`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityLink {
    pub source: String,
    pub source_principal: String,
    pub on_behalf_of: String,
    pub method: LinkMethod,
    pub confidence: f64,
}

/// Consume one link in an authorization join, yielding the principal it maps onto.
/// Confidence relaxes nothing (`authority.identify.unverified-link`).
pub fn authorize_link(link: &IdentityLink) -> Result<&str, AuthorityError> {
    if link.method.authorizes() {
        Ok(&link.on_behalf_of)
    } else {
        Err(AuthorityError::AuthorityLinkUnverified(format!(
            "a {} link on source {} does not authorize",
            link.method.as_str(),
            link.source
        )))
    }
}

/// The authorization join over a link table: the principal a verified link maps
/// `source_principal` onto; a match through `operator_asserted` links alone refuses; no
/// match resolves to `None`, which reads nothing (`authority.identify.unlinked-principal`).
pub fn resolve_link<'a>(
    links: &'a [IdentityLink],
    source: &str,
    source_principal: &str,
) -> Result<Option<&'a str>, AuthorityError> {
    let mut matching = links.iter().filter(|l| l.source == source && l.source_principal == source_principal).peekable();
    let Some(first) = matching.peek().copied() else {
        return Ok(None);
    };
    match matching.find(|l| l.method.authorizes()) {
        Some(link) => authorize_link(link).map(Some),
        None => authorize_link(first).map(Some),
    }
}
