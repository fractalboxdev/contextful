//! The decision module's case vocabulary: one JSON case in, one decision out, over the
//! grant coverage and narrowing decisions (`assurance.structure-tree.decision-module`).
//!
//! A case decodes through the domain parsers and reaches the domain functions; a case that
//! does not decode is `CaseMalformed`, and a parser's refusal is that refusal.

use crate::attenuate::{attenuate, Authority, Proposal};
use crate::grant::{Action, AggregateGrant, Grant, TablePattern, TenantScope};
use crate::identify::NormalizedSubject;
use crate::AuthorityError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The identifier a case that does not decode as a case carries.
pub const CASE_MALFORMED: &str = "CaseMalformed";

/// The decision fields a comparison reads, in order.
pub const DECISION_FIELDS: [&str; 3] = ["verdict", "error", "dimension"];

/// One decision: a verdict, an error identifier, and the dimension a widening names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub verdict: String,
    pub error: Option<String>,
    pub dimension: Option<String>,
}

impl Decision {
    fn verdict(v: &str) -> Decision {
        Decision { verdict: v.to_string(), error: None, dimension: None }
    }

    /// A refusal naming `error`, and the widened dimension where one applies.
    pub fn refused(error: &str, dimension: Option<&str>) -> Decision {
        Decision { verdict: "refused".into(), error: Some(error.into()), dimension: dimension.map(str::to_string) }
    }

    fn coverage(covered: bool) -> Decision {
        Decision::verdict(if covered { "covered" } else { "not_covered" })
    }

    /// The fields on which two decisions differ, in [`DECISION_FIELDS`] order.
    pub fn differing(&self, other: &Decision) -> Vec<&'static str> {
        let pairs = [self.verdict == other.verdict, self.error == other.error, self.dimension == other.dimension];
        DECISION_FIELDS.iter().zip(pairs).filter(|(_, same)| !same).map(|(f, _)| *f).collect()
    }
}

/// A fault decoding a case: its shape, or a domain parser's refusal.
enum Fault {
    Malformed,
    Refused(AuthorityError),
}

impl From<AuthorityError> for Fault {
    fn from(e: AuthorityError) -> Fault {
        Fault::Refused(e)
    }
}

type Decoded<T> = Result<T, Fault>;

/// The error identifier an [`AuthorityError`] carries at the head of its `Display`.
fn identifier(e: &AuthorityError) -> String {
    e.to_string().split(':').next().unwrap_or_default().to_string()
}

/// The decision on one decoded case.
pub fn decide_value(case: &Value) -> Decision {
    match decide_case(case) {
        Ok(d) => d,
        Err(Fault::Malformed) => Decision::refused(CASE_MALFORMED, None),
        Err(Fault::Refused(e)) => refusal(&e),
    }
}

fn refusal(e: &AuthorityError) -> Decision {
    let id = identifier(e);
    let dimension = match e {
        // The widened dimension heads the message (`authority.attenuate.widens`).
        AuthorityError::AttenuationWidens(msg) => msg.split(':').next().map(str::trim),
        _ => None,
    };
    Decision::refused(&id, dimension)
}

fn decide_case(case: &Value) -> Decoded<Decision> {
    let obj = case.as_object().ok_or(Fault::Malformed)?;
    match string(required(obj, "op")?)? {
        "covers_name" => {
            let pattern = string(required(obj, "pattern")?)?;
            let name = string(required(obj, "name")?)?;
            Ok(Decision::coverage(TablePattern::parse(pattern)?.covers_name(name)))
        }
        "covers_pattern" => {
            let pattern = string(required(obj, "pattern")?)?;
            let other = string(required(obj, "other")?)?;
            let pattern = TablePattern::parse(pattern)?;
            let other = TablePattern::parse(other)?;
            Ok(Decision::coverage(pattern.covers(&other)))
        }
        "narrow" => {
            let parent = required(obj, "parent")?.as_array().ok_or(Fault::Malformed)?;
            let child = required(obj, "child")?.as_array().ok_or(Fault::Malformed)?;
            let parent = parent.iter().map(grant).collect::<Decoded<Vec<_>>>()?;
            let child = child.iter().map(grant).collect::<Decoded<Vec<_>>>()?;
            let authority = Authority { grants: parent, exp: 0, subject: NormalizedSubject::default() };
            let proposal = Proposal { grants: Some(child), ..Proposal::default() };
            Ok(match attenuate(&authority, &proposal) {
                Ok(_) => Decision::verdict("admitted"),
                Err(e) => refusal(&e),
            })
        }
        _ => Err(Fault::Malformed),
    }
}

/// A present, non-null field.
fn field<'a>(obj: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    obj.get(key).filter(|v| !v.is_null())
}

fn required<'a>(obj: &'a Map<String, Value>, key: &str) -> Decoded<&'a Value> {
    field(obj, key).ok_or(Fault::Malformed)
}

fn string(v: &Value) -> Decoded<&str> {
    v.as_str().ok_or(Fault::Malformed)
}

fn strings(v: &Value) -> Decoded<Vec<String>> {
    v.as_array().ok_or(Fault::Malformed)?.iter().map(|s| string(s).map(str::to_string)).collect()
}

fn unsigned(v: &Value) -> Decoded<u64> {
    v.as_u64().ok_or(Fault::Malformed)
}

fn optional<T>(obj: &Map<String, Value>, key: &str, f: impl Fn(&Value) -> Decoded<T>) -> Decoded<Option<T>> {
    field(obj, key).map(f).transpose()
}

fn tenant(v: &Value) -> Decoded<TenantScope> {
    let obj = v.as_object().ok_or(Fault::Malformed)?;
    let table = string(required(obj, "table")?)?.to_string();
    let value = string(required(obj, "value")?)?.to_string();
    Ok(TenantScope { table, value })
}

fn aggregate(v: &Value) -> Decoded<AggregateGrant> {
    let obj = v.as_object().ok_or(Fault::Malformed)?;
    let min_group_size = unsigned(required(obj, "min_group_size")?)?;
    let max_contributor_share = required(obj, "max_contributor_share")?.as_f64().ok_or(Fault::Malformed)?;
    let functions = strings(required(obj, "functions")?)?;
    let max_groups = unsigned(required(obj, "max_groups")?)?;
    let max_rows = optional(obj, "max_rows", unsigned)?;
    Ok(AggregateGrant { min_group_size, max_contributor_share, functions, max_groups, max_rows })
}

/// One grant: its shape first, then each action, then each table pattern.
fn grant(v: &Value) -> Decoded<Grant> {
    let obj = v.as_object().ok_or(Fault::Malformed)?;
    let actions = strings(required(obj, "actions")?)?;
    let tables = strings(required(obj, "tables")?)?;
    let tenant = optional(obj, "tenant", tenant)?;
    let templates = optional(obj, "templates", strings)?;
    let aggregate = optional(obj, "aggregate", aggregate)?;
    let max_rows = optional(obj, "max_rows", unsigned)?;
    let actions = actions.iter().map(|a| Action::parse(a)).collect::<Result<Vec<_>, _>>()?;
    let tables = tables.iter().map(|t| TablePattern::parse(t)).collect::<Result<Vec<_>, _>>()?;
    Ok(Grant { actions, tables, tenant, aggregate, templates, max_rows })
}
