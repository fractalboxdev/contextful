//! `authority.profile`: the versioned delegation profile over the attenuable-credential
//! library.
//!
//! The library owns serialization, signatures, block chaining and evaluation. Profile 1
//! admits facts alone in every block — no rule, check, scope or third-party block — so
//! evaluation runs no recursion, external function or regular expression. The authority
//! block carries the claims of `spec/50-authority.md` Shapes as named facts:
//!
//! ```text
//! profile(1); iss(s); aud(s)?; jti(s); iat(n); exp(n); alg(s); cnf(s)?;
//! sub(member, value)*; incognito(b)?; att(member, attestation)*; grant(i, json)*; rev(id, epoch);
//! ```
//!
//! An attenuation block carries only `grant(i, json)*`, `exp(n)`, `sub(member, value)*`,
//! `incognito(b)` and `cnf(s)`; each proposes a narrowing its parent is checked against,
//! and none contributes an authority fact of its own.

use biscuit_auth::builder::{boolean, fact, int, string, Fact};
use biscuit_auth::datalog::SymbolTable;
use biscuit_auth::format::schema;
use biscuit_auth::{AuthorizerBuilder, AuthorizerLimits, Biscuit};
use contextful_core::attenuate::{Authority, Proposal};
use contextful_core::claims::{AuthorityBlock, Confirmation, Revocation};
use contextful_core::grant::{Action, Grant, TablePattern};
use contextful_core::identify::{Attestation, Member, Subject, SubjectDerivation};
use contextful_core::time::Instant;
use contextful_core::AuthorityError;
use prost::Message;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::{Duration, UNIX_EPOCH};

/// The profile version this engine mints.
pub const PROFILE_VERSION: i64 = 1;

/// The profile versions a checkpoint admits by default (`authority.profile.version-unsupported`).
pub const SUPPORTED_PROFILE_VERSIONS: &[i64] = &[PROFILE_VERSION];

/// Facts one authorization holds, token and engine facts together (`authority.profile.fact-ceiling`).
pub const EVALUATOR_FACT_CEILING: u64 = 1000;

/// Facts the engine supplies to every authorization: the current time.
pub const ENGINE_FACTS: usize = 1;

/// Evaluator iterations per authorization (`authority.profile.iteration-ceiling`).
pub const EVALUATOR_ITERATION_CEILING: u64 = 100;

/// Predicates only the engine supplies: current time, audience, resolved resources and
/// authenticated request identity (`authority.profile.reserved-fact`).
pub const RESERVED_PREDICATES: &[&str] = &["time", "audience", "resource", "request"];

/// Datalog block versions profile 1 names: the version the library writes for a block of
/// string, integer and boolean facts.
pub const BLOCK_VERSIONS: &[u32] = &[3];

/// Grant fields profile 1 names.
const GRANT_FIELDS: &[&str] = &["actions", "tables", "tenant", "aggregate", "templates", "max_rows", "max_duration_ms", "max_response_bytes"];

/// Restriction fields profile 1 declares and parses, and this engine refuses: no read
/// evaluator exists for a row restriction or an aggregate bound
/// (`authority.profile.declared-field`, `authority.profile.unevaluated-restriction`).
const UNEVALUATED_RESTRICTIONS: &[&str] = &["row_restriction", "aggregate"];

/// The evaluator's bounds. No wall clock bounds admission: the time limit sits far past
/// any evaluation the fact and iteration ceilings admit.
pub fn evaluator_limits() -> AuthorizerLimits {
    AuthorizerLimits {
        max_facts: EVALUATOR_FACT_CEILING,
        max_iterations: EVALUATOR_ITERATION_CEILING,
        max_time: Duration::from_secs(60 * 60),
    }
}

fn unrecognized(what: String) -> AuthorityError {
    AuthorityError::ProfileElementUnrecognized(what)
}

/// One term of a fact, as the profile reads it.
#[derive(Debug, Clone, PartialEq)]
enum Term {
    Str(String),
    Int(i64),
    Bool(bool),
    Other,
}

#[derive(Debug, Clone)]
struct RawFact {
    name: String,
    terms: Vec<Term>,
}

/// What one attenuation block proposes. An absent dimension inherits its parent's.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Hop {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grants: Option<Vec<Grant>>,
    /// Unix seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exp: Option<i64>,
    pub sub: SubjectDerivation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cnf: Option<Confirmation>,
}

impl Hop {
    /// The narrowing proposal the domain checks against the parent.
    pub fn proposal(&self) -> Proposal {
        Proposal { grants: self.grants.clone(), exp: self.exp, subject: self.sub.clone() }
    }
}

/// A credential's blocks, read under the profile.
#[derive(Debug, Clone, PartialEq)]
pub struct Chain {
    pub version: i64,
    pub authority: AuthorityBlock,
    pub hops: Vec<Hop>,
    /// Facts across every block.
    pub fact_count: usize,
}

impl Chain {
    /// The authority block as the root of narrowing.
    pub fn root(&self) -> Authority {
        Authority { grants: self.authority.grants.clone(), exp: self.authority.exp, subject: self.authority.subject() }
    }

    /// Check every hop against the one before it and return the chain-final authority
    /// (`authority.attenuate.narrowing`).
    pub fn effective(&self) -> Result<Authority, AuthorityError> {
        let hops: Vec<Proposal> = self.hops.iter().map(Hop::proposal).collect();
        contextful_core::attenuate::check_chain(&self.root(), &hops)
    }

    /// The confirmation the chain-final block binds: the last one a block names.
    pub fn confirmation(&self) -> Option<&Confirmation> {
        self.hops.iter().rev().find_map(|h| h.cnf.as_ref()).or(self.authority.cnf.as_ref())
    }
}

/// Read a credential's serialized bytes under the profile, admitting only `supported`
/// versions. The bytes' signatures are the caller's to check; this reads structure.
/// Input past the fact ceiling raises `ProfileEvaluationBudget`
/// (`authority.profile.evaluator-bound`).
pub fn read_chain(bytes: &[u8], supported: &[i64]) -> Result<Chain, AuthorityError> {
    let proto = schema::Biscuit::decode(bytes)
        .map_err(|e| AuthorityError::SignatureInvalid(format!("the bytes decode to no signed chain: {e}")))?;
    let mut symbols = SymbolTable::new();
    let mut blocks = Vec::with_capacity(1 + proto.blocks.len());
    for (i, signed) in std::iter::once(&proto.authority).chain(&proto.blocks).enumerate() {
        if signed.external_signature.is_some() {
            return Err(unrecognized(format!("block {i} is a third-party block")));
        }
        let block = schema::Block::decode(&signed.block[..])
            .map_err(|e| AuthorityError::SignatureInvalid(format!("block {i} decodes to nothing: {e}")))?;
        let own = SymbolTable::from(block.symbols.clone())
            .map_err(|e| unrecognized(format!("block {i} carries an unreadable symbol table: {e}")))?;
        symbols.extend(&own).map_err(|e| unrecognized(format!("block {i} redefines a symbol: {e}")))?;
        blocks.push(read_block(i, &block, &symbols)?);
    }
    let fact_count: usize = blocks.iter().map(Vec::len).sum();
    let total = fact_count + ENGINE_FACTS;
    if total as u64 > EVALUATOR_FACT_CEILING {
        return Err(AuthorityError::ProfileEvaluationBudget(format!(
            "{total} facts exceed the {EVALUATOR_FACT_CEILING}-entry ceiling"
        )));
    }
    let mut blocks = blocks.into_iter();
    let authority = blocks.next().unwrap_or_default();
    let version = read_version(&authority, supported)?;
    let authority = read_authority(&authority)?;
    let hops = blocks.enumerate().map(|(i, b)| read_hop(i + 1, &b)).collect::<Result<_, _>>()?;
    Ok(Chain { version, authority, hops, fact_count })
}

/// Structural checks on one block, then its facts.
fn read_block(i: usize, block: &schema::Block, symbols: &SymbolTable) -> Result<Vec<RawFact>, AuthorityError> {
    let version = block.version.unwrap_or(0);
    if !BLOCK_VERSIONS.contains(&version) {
        return Err(unrecognized(format!("block {i} has datalog version {version}")));
    }
    let name = |index: u64| -> Result<String, AuthorityError> {
        symbols.get_symbol(index).map(str::to_string).ok_or_else(|| unrecognized(format!("block {i} names symbol {index}, which no table holds")))
    };
    let mut facts = Vec::with_capacity(block.facts.len());
    for f in &block.facts {
        let name = name(f.predicate.name)?;
        reserved(i, &name)?;
        let terms = f.predicate.terms.iter().map(|t| term(t, symbols)).collect();
        facts.push(RawFact { name, terms });
    }
    for r in &block.rules {
        reserved(i, &name(r.head.name)?)?;
    }
    if !block.rules.is_empty() {
        return Err(unrecognized(format!("block {i} carries a rule")));
    }
    if !block.checks.is_empty() {
        return Err(unrecognized(format!("block {i} carries a check")));
    }
    if !block.scope.is_empty() || !block.public_keys.is_empty() {
        return Err(unrecognized(format!("block {i} declares a trust scope")));
    }
    Ok(facts)
}

fn reserved(i: usize, name: &str) -> Result<(), AuthorityError> {
    if RESERVED_PREDICATES.contains(&name) {
        return Err(AuthorityError::ProfileReservedFact(format!("block {i} introduces the reserved fact `{name}`")));
    }
    Ok(())
}

fn term(t: &schema::Term, symbols: &SymbolTable) -> Term {
    use schema::term::Content;
    match &t.content {
        Some(Content::String(s)) => symbols.get_symbol(*s).map(|s| Term::Str(s.to_string())).unwrap_or(Term::Other),
        Some(Content::Integer(n)) => Term::Int(*n),
        Some(Content::Bool(b)) => Term::Bool(*b),
        _ => Term::Other,
    }
}

fn read_version(facts: &[RawFact], supported: &[i64]) -> Result<i64, AuthorityError> {
    let named: Vec<&RawFact> = facts.iter().filter(|f| f.name == "profile").collect();
    let version = match named.as_slice() {
        [f] => match f.terms.as_slice() {
            [Term::Int(v)] => *v,
            _ => return Err(unrecognized("`profile` takes one integer".into())),
        },
        [] => return Err(AuthorityError::ProfileVersionUnsupported("the authority block names no profile version".into())),
        _ => return Err(unrecognized("the authority block names `profile` twice".into())),
    };
    if !supported.contains(&version) {
        return Err(AuthorityError::ProfileVersionUnsupported(format!(
            "profile {version} is outside the supported set {supported:?}"
        )));
    }
    Ok(version)
}

/// Scalar and keyed facts, each named at most once per block.
#[derive(Default)]
struct Slots {
    scalars: BTreeMap<&'static str, Term>,
    sub: BTreeMap<Member, String>,
    att: BTreeMap<Member, Attestation>,
    grants: BTreeMap<i64, Grant>,
    rev: Option<(String, u64)>,
}

fn member(i: usize, s: &str) -> Result<Member, AuthorityError> {
    Member::ALL.into_iter().find(|m| m.as_str() == s).ok_or_else(|| unrecognized(format!("block {i} names subject member `{s}`")))
}

fn slots(i: usize, facts: &[RawFact], allowed: &[&'static str]) -> Result<Slots, AuthorityError> {
    let mut out = Slots::default();
    for f in facts {
        let Some(&name) = allowed.iter().find(|a| **a == f.name) else {
            return Err(unrecognized(format!("block {i} carries the predicate `{}`", f.name)));
        };
        let twice = || unrecognized(format!("block {i} names `{name}` twice"));
        let shape = || unrecognized(format!("block {i}: `{name}` has an unrecognized shape"));
        match (name, f.terms.as_slice()) {
            ("sub", [Term::Str(m), Term::Str(v)]) => {
                if out.sub.insert(member(i, m)?, v.clone()).is_some() {
                    return Err(twice());
                }
            }
            ("att", [Term::Str(m), Term::Str(a)]) => {
                let a = match a.as_str() {
                    "verified" => Attestation::Verified,
                    "asserted" => Attestation::Asserted,
                    _ => return Err(shape()),
                };
                if out.att.insert(member(i, m)?, a).is_some() {
                    return Err(twice());
                }
            }
            ("grant", [Term::Int(n), Term::Str(json)]) => {
                if out.grants.insert(*n, decode_grant(json)?).is_some() {
                    return Err(twice());
                }
            }
            ("rev", [Term::Str(id), Term::Int(epoch)]) => {
                let epoch = u64::try_from(*epoch).map_err(|_| shape())?;
                if out.rev.replace((id.clone(), epoch)).is_some() {
                    return Err(twice());
                }
            }
            ("iat" | "exp", [t]) => {
                if !matches!(t, Term::Int(_)) {
                    return Err(AuthorityError::TimestampMalformed(format!("block {i}: `{name}` is not seconds since the epoch")));
                }
                if out.scalars.insert(name, t.clone()).is_some() {
                    return Err(twice());
                }
            }
            ("incognito", [t @ Term::Bool(_)]) | ("profile", [t @ Term::Int(_)]) => {
                if out.scalars.insert(name, t.clone()).is_some() {
                    return Err(twice());
                }
            }
            ("iss" | "aud" | "jti" | "alg" | "cnf", [t @ Term::Str(_)]) => {
                if out.scalars.insert(name, t.clone()).is_some() {
                    return Err(twice());
                }
            }
            _ => return Err(shape()),
        }
    }
    let indices: Vec<i64> = out.grants.keys().copied().collect();
    if indices.iter().enumerate().any(|(k, n)| i64::try_from(k).ok() != Some(*n)) {
        return Err(unrecognized(format!("block {i}: grant indices {indices:?} do not run from 0")));
    }
    Ok(out)
}

impl Slots {
    fn str(&self, name: &str) -> Option<String> {
        match self.scalars.get(name) {
            Some(Term::Str(s)) => Some(s.clone()),
            _ => None,
        }
    }

    fn int(&self, name: &str) -> Option<i64> {
        match self.scalars.get(name) {
            Some(Term::Int(n)) => Some(*n),
            _ => None,
        }
    }

    fn bool(&self, name: &str) -> Option<bool> {
        match self.scalars.get(name) {
            Some(Term::Bool(b)) => Some(*b),
            _ => None,
        }
    }

    fn grants(&self) -> Vec<Grant> {
        self.grants.values().cloned().collect()
    }
}

const AUTHORITY_FACTS: &[&str] =
    &["profile", "iss", "aud", "jti", "iat", "exp", "alg", "cnf", "sub", "incognito", "att", "grant", "rev"];
const HOP_FACTS: &[&str] = &["grant", "exp", "sub", "incognito", "cnf"];

fn read_authority(facts: &[RawFact]) -> Result<AuthorityBlock, AuthorityError> {
    let s = slots(0, facts, AUTHORITY_FACTS)?;
    let missing = |name: &str| unrecognized(format!("the authority block names no `{name}`"));
    let timestamp = |name: &str| -> Result<i64, AuthorityError> {
        let secs = s.int(name).ok_or_else(|| missing(name))?;
        Instant::from_unix_secs(secs)?;
        Ok(secs)
    };
    let (rev_id, epoch) = s.rev.clone().ok_or_else(|| missing("rev"))?;
    let sub = Subject {
        on_behalf_of: s.sub.get(&Member::OnBehalfOf).cloned(),
        agent: s.sub.get(&Member::Agent).cloned(),
        host: s.sub.get(&Member::Host).cloned(),
        task: s.sub.get(&Member::Task).cloned(),
        zone: s.sub.get(&Member::Zone).cloned(),
        incognito: s.bool("incognito").unwrap_or(false),
    };
    Ok(AuthorityBlock {
        iss: s.str("iss").ok_or_else(|| missing("iss"))?,
        // A credential naming no audience reads as the empty one, which no declared
        // audience matches.
        aud: s.str("aud").unwrap_or_default(),
        jti: s.str("jti").ok_or_else(|| missing("jti"))?,
        iat: timestamp("iat")?,
        exp: timestamp("exp")?,
        alg: s.str("alg").ok_or_else(|| missing("alg"))?,
        cnf: s.str("cnf").map(|jkt| Confirmation { jkt }),
        sub,
        att: s.att.clone(),
        grants: s.grants(),
        rev: Revocation { id: rev_id, epoch },
    })
}

fn read_hop(i: usize, facts: &[RawFact]) -> Result<Hop, AuthorityError> {
    let s = slots(i, facts, HOP_FACTS)?;
    if let Some(exp) = s.int("exp") {
        Instant::from_unix_secs(exp)?;
    }
    let get = |m: Member| s.sub.get(&m).cloned();
    Ok(Hop {
        grants: if s.grants.is_empty() { None } else { Some(s.grants()) },
        exp: s.int("exp"),
        sub: SubjectDerivation {
            on_behalf_of: get(Member::OnBehalfOf),
            agent: get(Member::Agent),
            host: get(Member::Host),
            task: get(Member::Task),
            zone: get(Member::Zone),
            incognito: s.bool("incognito"),
        },
        cnf: s.str("cnf").map(|jkt| Confirmation { jkt }),
    })
}

/// Decode one grant's JSON: every field named, the restriction fields parsed and refused,
/// actions and tables through the grant parsers.
pub fn decode_grant(json: &str) -> Result<Grant, AuthorityError> {
    let value: Value = serde_json::from_str(json).map_err(|e| unrecognized(format!("a grant is not JSON: {e}")))?;
    let Value::Object(fields) = &value else {
        return Err(unrecognized("a grant is not an object".into()));
    };
    for key in fields.keys() {
        if !GRANT_FIELDS.contains(&key.as_str()) && !UNEVALUATED_RESTRICTIONS.contains(&key.as_str()) {
            return Err(unrecognized(format!("a grant carries the field `{key}`")));
        }
    }
    if let Some(key) = UNEVALUATED_RESTRICTIONS.iter().find(|k| fields.contains_key(**k)) {
        return Err(AuthorityError::ProfileRestrictionUnevaluated(format!(
            "a grant carries `{key}`, and no read evaluator exists for it"
        )));
    }
    let words = |key: &str| -> Result<Vec<String>, AuthorityError> {
        match fields.get(key) {
            None => Ok(Vec::new()),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| v.as_str().map(str::to_string).ok_or_else(|| unrecognized(format!("`{key}` holds a non-string"))))
                .collect(),
            Some(_) => Err(unrecognized(format!("`{key}` is not a list"))),
        }
    };
    let actions = words("actions")?.iter().map(|a| Action::parse(a)).collect::<Result<Vec<_>, _>>()?;
    let tables = words("tables")?.iter().map(|t| TablePattern::parse(t)).collect::<Result<Vec<_>, _>>()?;
    let mut rest = fields.clone();
    rest.insert("actions".into(), Value::Array(Vec::new()));
    rest.insert("tables".into(), Value::Array(Vec::new()));
    let mut grant: Grant =
        serde_json::from_value(Value::Object(rest)).map_err(|e| unrecognized(format!("a grant field is malformed: {e}")))?;
    grant.actions = actions;
    grant.tables = tables;
    Ok(grant)
}

/// Encode one grant, refusing a restriction no read evaluator exists for.
pub fn encode_grant(grant: &Grant) -> Result<String, AuthorityError> {
    if grant.aggregate.is_some() {
        return Err(AuthorityError::ProfileRestrictionUnevaluated(
            "a grant carries `aggregate`, and no read evaluator exists for it".into(),
        ));
    }
    Ok(serde_json::to_string(grant).expect("a grant serializes"))
}

fn sub_facts(out: &mut Vec<Fact>, members: impl IntoIterator<Item = (Member, Option<String>)>) {
    for (m, v) in members {
        if let Some(v) = v {
            out.push(fact("sub", &[string(m.as_str()), string(&v)]));
        }
    }
}

fn grant_facts(out: &mut Vec<Fact>, grants: &[Grant]) -> Result<(), AuthorityError> {
    for (i, g) in grants.iter().enumerate() {
        out.push(fact("grant", &[int(i64::try_from(i).unwrap_or(i64::MAX)), string(&encode_grant(g)?)]));
    }
    Ok(())
}

/// The authority block's facts.
pub fn authority_facts(block: &AuthorityBlock) -> Result<Vec<Fact>, AuthorityError> {
    let mut out = vec![
        fact("profile", &[int(PROFILE_VERSION)]),
        fact("iss", &[string(&block.iss)]),
        fact("aud", &[string(&block.aud)]),
        fact("jti", &[string(&block.jti)]),
        fact("iat", &[int(block.iat)]),
        fact("exp", &[int(block.exp)]),
        fact("alg", &[string(&block.alg)]),
    ];
    if let Some(cnf) = &block.cnf {
        out.push(fact("cnf", &[string(&cnf.jkt)]));
    }
    let subject = block.subject();
    sub_facts(&mut out, Member::ALL.map(|m| (m, subject.get(m).map(str::to_string))));
    out.push(fact("incognito", &[boolean(subject.incognito())]));
    for (m, a) in &block.att {
        let a = match a {
            Attestation::Verified => "verified",
            Attestation::Asserted => "asserted",
        };
        out.push(fact("att", &[string(m.as_str()), string(a)]));
    }
    grant_facts(&mut out, &block.grants)?;
    out.push(fact("rev", &[string(&block.rev.id), int(i64::try_from(block.rev.epoch).unwrap_or(i64::MAX))]));
    Ok(out)
}

/// An attenuation block's facts.
pub fn hop_facts(hop: &Hop) -> Result<Vec<Fact>, AuthorityError> {
    let mut out = Vec::new();
    if let Some(grants) = &hop.grants {
        grant_facts(&mut out, grants)?;
    }
    if let Some(exp) = hop.exp {
        out.push(fact("exp", &[int(exp)]));
    }
    let s = &hop.sub;
    sub_facts(
        &mut out,
        [
            (Member::OnBehalfOf, s.on_behalf_of.clone()),
            (Member::Agent, s.agent.clone()),
            (Member::Host, s.host.clone()),
            (Member::Task, s.task.clone()),
            (Member::Zone, s.zone.clone()),
        ],
    );
    if let Some(incognito) = s.incognito {
        out.push(fact("incognito", &[boolean(incognito)]));
    }
    if let Some(cnf) = &hop.cnf {
        out.push(fact("cnf", &[string(&cnf.jkt)]));
    }
    Ok(out)
}

/// Run the library's evaluator over a verified token under the profile's bounds, the
/// engine supplying the current time. Evaluation past either ceiling raises `ProfileEvaluationBudget` (`authority.profile.evaluator-bound`).
pub fn evaluate(token: &Biscuit, now: Instant) -> Result<(), AuthorityError> {
    let secs = u64::try_from(now.unix_secs()).unwrap_or(0);
    let budget = |e: biscuit_auth::error::Token| match e {
        biscuit_auth::error::Token::RunLimit(limit) => AuthorityError::ProfileEvaluationBudget(limit.to_string()),
        other => unrecognized(format!("the evaluator refuses the credential: {other}")),
    };
    let mut authorizer = AuthorizerBuilder::new()
        .fact(fact("time", &[biscuit_auth::builder::date(&(UNIX_EPOCH + Duration::from_secs(secs)))]))
        .and_then(|b| b.policy("allow if true"))
        .map_err(budget)?
        .set_limits(evaluator_limits())
        .build(token)
        .map_err(budget)?;
    authorizer.authorize().map(|_| ()).map_err(budget)
}
