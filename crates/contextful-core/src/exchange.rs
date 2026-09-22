//! The exchange: the declared policy, and the mint a verified external assertion earns.
//!
//! The assertion's signature is checked by the adapter that holds the verifying
//! material; this module takes the decoded claims and applies every other check.

use crate::grant::{Action, AggregateGrant, Grant, TablePattern, TenantScope};
use crate::identify::Subject;
use crate::issue::{IssuancePolicy, Lifetime, MintAuthority, MintContext, MintPlan, MintRequest, PolicyError};
use crate::time::Instant;
use crate::AuthorityError;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Minted lifetime where the policy declares no `ttl_secs`: 900 s (`authority.exchange.lifetime-default`).
pub const EXCHANGE_LIFETIME_DEFAULT_SECS: u64 = 900;

/// Ceiling a configured minted lifetime clamps to: 3600 s (`authority.exchange.lifetime-ceiling`).
pub const EXCHANGE_LIFETIME_CEILING_SECS: u64 = 3600;

/// One subject member drawn from an assertion claim, optionally through a template whose
/// single `{}` receives the claim value.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimMapping {
    pub claim: String,
    #[serde(default)]
    pub template: Option<String>,
}

/// How assertion claims fill the subject tuple. Tenancy is no subject member.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubjectMap {
    pub on_behalf_of: Option<ClaimMapping>,
    pub agent: Option<ClaimMapping>,
    pub host: Option<ClaimMapping>,
    pub task: Option<ClaimMapping>,
    pub zone: Option<ClaimMapping>,
}

/// A project's exchange policy (`authority.exchange.policy`), at [`ExchangePolicy::PATH`].
#[derive(Debug, Clone, PartialEq)]
pub struct ExchangePolicy {
    pub expected_iss: String,
    pub expected_aud: Option<String>,
    pub subject_map: SubjectMap,
    pub tenant_claim: Option<String>,
    pub role_claim: Option<String>,
    pub role_grants: BTreeMap<String, Vec<Grant>>,
    pub default_grants: Vec<Grant>,
    pub ttl_secs: Option<u64>,
    pub minted_iss: Option<String>,
    pub minted_aud: Option<String>,
}

/// Operator-injected verifying material (`authority.exchange.injected-material`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyingMaterial {
    SharedSecret(Vec<u8>),
    Rs256PublicKeyPem(String),
    /// A key-set document; the assertion's `kid` selects the key.
    KeySet(String),
}

impl VerifyingMaterial {
    fn is_empty(&self) -> bool {
        match self {
            VerifyingMaterial::SharedSecret(s) => s.is_empty(),
            VerifyingMaterial::Rs256PublicKeyPem(s) | VerifyingMaterial::KeySet(s) => s.trim().is_empty(),
        }
    }
}

impl ExchangePolicy {
    /// Where a project declares its exchange policy.
    pub const PATH: &'static str = ".contextful/exchange/policy.toml";

    /// Where the operator injects the verifying material.
    pub const VERIFY_KEY_PATH: &'static str = ".contextful/exchange/verify.key";

    /// Decode a policy file. A grant's actions and table patterns refuse as they do at
    /// the mint.
    pub fn parse(text: &str) -> Result<ExchangePolicy, PolicyError> {
        let raw: RawPolicy =
            toml::from_str(text).map_err(|e| PolicyError::Malformed(format!("{}: {}", Self::PATH, e.message())))?;
        let map = &raw.subject_map;
        for m in [&map.on_behalf_of, &map.agent, &map.host, &map.task, &map.zone].into_iter().flatten() {
            if let Some(t) = &m.template {
                if t.matches("{}").count() != 1 {
                    return Err(PolicyError::Malformed(format!(
                        "{}: subject_map template `{t}` holds no single `{{}}`",
                        Self::PATH
                    )));
                }
            }
        }
        let grants = |raw: Vec<RawGrant>| raw.into_iter().map(RawGrant::into_grant).collect::<Result<Vec<_>, _>>();
        let mut role_grants = BTreeMap::new();
        for (role, raw_grants) in raw.role_grants {
            role_grants.insert(role, grants(raw_grants)?);
        }
        Ok(ExchangePolicy {
            expected_iss: raw.expected_iss,
            expected_aud: raw.expected_aud,
            subject_map: raw.subject_map,
            tenant_claim: raw.tenant_claim,
            role_claim: raw.role_claim,
            role_grants,
            default_grants: grants(raw.default_grants)?,
            ttl_secs: raw.ttl_secs,
            minted_iss: raw.minted_iss,
            minted_aud: raw.minted_aud,
        })
    }

    /// The mint request a verified assertion's claims earn at `now`.
    pub fn mint_request(
        &self,
        material: Option<&VerifyingMaterial>,
        claims: &Map<String, Value>,
        now: Instant,
    ) -> Result<MintRequest, AuthorityError> {
        if material.is_none_or(VerifyingMaterial::is_empty) {
            return Err(AuthorityError::ExchangeMaterialMissing(format!(
                "no verifying material at {}",
                Self::VERIFY_KEY_PATH
            )));
        }
        self.check_assertion(claims, now)?;

        let map = &self.subject_map;
        let subject = Subject {
            on_behalf_of: map_member(claims, &map.on_behalf_of)?,
            agent: map_member(claims, &map.agent)?,
            host: map_member(claims, &map.host)?,
            task: map_member(claims, &map.task)?,
            zone: map_member(claims, &map.zone)?,
            incognito: false,
        };

        let roles = match &self.role_claim {
            Some(name) => roles(claims, name)?,
            None => vec![],
        };
        let matched: Vec<&Grant> = roles.iter().filter_map(|r| self.role_grants.get(r)).flatten().collect();
        let grants: Vec<&Grant> = if matched.is_empty() { self.default_grants.iter().collect() } else { matched };

        let grants = match &self.tenant_claim {
            None => grants.into_iter().cloned().collect(),
            Some(name) => {
                let value = string_claim(claims, name)?;
                grants.into_iter().flat_map(|g| scope_to_tenant(g, &value)).collect()
            }
        };

        let ttl = self.ttl_secs.unwrap_or(EXCHANGE_LIFETIME_DEFAULT_SECS).min(EXCHANGE_LIFETIME_CEILING_SECS);
        Ok(MintRequest {
            authority: MintAuthority::Exchange,
            subject,
            grants,
            lifetime: Lifetime::Clamped(ttl),
            audience: self.minted_aud.clone(),
            issuer: self.minted_iss.clone(),
            algorithm: None,
        })
    }

    /// The whole exchange: the assertion's mint request, checked against the persisted
    /// issuance policy.
    pub fn mint(
        &self,
        material: Option<&VerifyingMaterial>,
        claims: &Map<String, Value>,
        issuance: &IssuancePolicy,
        ctx: &MintContext<'_>,
    ) -> Result<MintPlan, AuthorityError> {
        let request = self.mint_request(material, claims, ctx.clock.now())?;
        issuance.check(&request, ctx)
    }

    /// Issuer, audience and expiry (`authority.exchange.assertion-invalid`).
    fn check_assertion(&self, claims: &Map<String, Value>, now: Instant) -> Result<(), AuthorityError> {
        if claims.get("iss").and_then(Value::as_str) != Some(self.expected_iss.as_str()) {
            return Err(invalid(format!("issuer is not the trusted `{}`", self.expected_iss)));
        }
        if let Some(aud) = &self.expected_aud {
            let trusted = match claims.get("aud") {
                Some(Value::String(s)) => s == aud,
                Some(Value::Array(list)) => list.iter().any(|v| v.as_str() == Some(aud)),
                _ => false,
            };
            if !trusted {
                return Err(invalid(format!("audience is not the trusted `{aud}`")));
            }
        }
        let exp = claims.get("exp").and_then(Value::as_i64).ok_or_else(|| invalid("no numeric `exp`".into()))?;
        let exp = Instant::from_unix_secs(exp).map_err(|e| invalid(e.to_string()))?;
        if exp <= now {
            return Err(invalid(format!("lapsed at {exp}")));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPolicy {
    expected_iss: String,
    #[serde(default)]
    expected_aud: Option<String>,
    #[serde(default)]
    subject_map: SubjectMap,
    #[serde(default)]
    tenant_claim: Option<String>,
    #[serde(default)]
    role_claim: Option<String>,
    #[serde(default)]
    role_grants: BTreeMap<String, Vec<RawGrant>>,
    #[serde(default)]
    default_grants: Vec<RawGrant>,
    #[serde(default)]
    ttl_secs: Option<u64>,
    #[serde(default)]
    minted_iss: Option<String>,
    #[serde(default)]
    minted_aud: Option<String>,
}

/// A policy grant as written. No tenant: tenancy comes from the assertion alone.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGrant {
    #[serde(default)]
    actions: Vec<String>,
    tables: Vec<String>,
    #[serde(default)]
    aggregate: Option<AggregateGrant>,
    #[serde(default)]
    templates: Option<Vec<String>>,
    #[serde(default)]
    max_rows: Option<u64>,
}

impl RawGrant {
    fn into_grant(self) -> Result<Grant, PolicyError> {
        Ok(Grant {
            actions: self.actions.iter().map(|a| Action::parse(a)).collect::<Result<_, _>>()?,
            tables: self.tables.iter().map(|t| TablePattern::parse(t)).collect::<Result<_, _>>()?,
            tenant: None,
            aggregate: self.aggregate,
            templates: self.templates,
            max_rows: self.max_rows,
        })
    }
}

/// One grant per table pattern, each scoped to the tenant value on that pattern.
fn scope_to_tenant(g: &Grant, value: &str) -> Vec<Grant> {
    g.tables
        .iter()
        .map(|t| Grant {
            tables: vec![t.clone()],
            tenant: Some(TenantScope { table: String::from(t.clone()), value: value.to_string() }),
            ..g.clone()
        })
        .collect()
}

fn map_member(claims: &Map<String, Value>, mapping: &Option<ClaimMapping>) -> Result<Option<String>, AuthorityError> {
    let Some(m) = mapping else { return Ok(None) };
    let value = string_claim(claims, &m.claim)?;
    if value.trim() != value {
        return Err(AuthorityError::AuthoritySubjectMalformed(format!("claim `{}` arrives padded", m.claim)));
    }
    Ok(Some(match &m.template {
        Some(t) => t.replacen("{}", &value, 1),
        None => value,
    }))
}

fn string_claim(claims: &Map<String, Value>, name: &str) -> Result<String, AuthorityError> {
    match claims.get(name) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(invalid(format!("mapped claim `{name}` is not a string"))),
        None => Err(invalid(format!("mapped claim `{name}` is absent"))),
    }
}

/// The verified roles: a string or a list of strings; an absent claim names none.
fn roles(claims: &Map<String, Value>, name: &str) -> Result<Vec<String>, AuthorityError> {
    match claims.get(name) {
        None => Ok(vec![]),
        Some(Value::String(s)) => Ok(vec![s.clone()]),
        Some(Value::Array(list)) => list
            .iter()
            .map(|v| v.as_str().map(str::to_string).ok_or_else(|| invalid(format!("role claim `{name}` holds a non-string"))))
            .collect(),
        Some(_) => Err(invalid(format!("role claim `{name}` is neither a string nor a list"))),
    }
}

fn invalid(reason: String) -> AuthorityError {
    AuthorityError::ExchangeAssertionInvalid(reason)
}
