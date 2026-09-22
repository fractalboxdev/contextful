//! The subject tuple, attestation, mint hygiene, normalization and identity links.

use serde::{Deserialize, Serialize};

/// A subject: the combination a credential binds and admits under
/// (`authority.identify.subject-tuple`). An absent member is `None`.
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

/// How a subject member is known (`authority.identify.attestation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Attestation {
    Verified,
    Asserted,
}

/// The method an identity link was established by (`authority.identify.unverified-link`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkMethod {
    ScimEmail,
    OidcSub,
    OperatorAsserted,
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
