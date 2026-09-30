//! The decision module's case vocabulary: one case text in, one decision out, over grant
//! coverage, grant narrowing, zone admission and session-zone resolution
//! (`assurance.structure-tree.decision-module`).
//!
//! A case is UTF-8 JSON. It decodes through the domain parsers and reaches the domain
//! functions; a case that does not decode is `CaseMalformed`, and a parser's refusal is
//! that refusal. The same source builds native and, for `wasm32-unknown-unknown`, as a
//! module exporting [`abi`]'s three functions, which a gateway and the differential
//! harness call.

use crate::attenuate::{attenuate, Authority, Proposal};
use crate::enforce::EnforceError;
use crate::grant::{Action, AggregateGrant, Grant, TablePattern, TenantScope};
use crate::identify::NormalizedSubject;
use crate::place::{session_zone, AllowSet, Entry, Zone};
use crate::AuthorityError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The identifier a case that does not decode as a case carries.
pub const CASE_MALFORMED: &str = "CaseMalformed";

/// The decision fields a comparison reads, in order.
pub const DECISION_FIELDS: [&str; 4] = ["verdict", "error", "dimension", "zone"];

/// One decision: a verdict, an error identifier, the dimension a widening names, and the
/// zone a placement case resolves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub verdict: String,
    pub error: Option<String>,
    pub dimension: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<String>,
}

impl Decision {
    fn verdict(v: &str) -> Decision {
        Decision { verdict: v.to_string(), error: None, dimension: None, zone: None }
    }

    /// A refusal naming `error`, and the widened dimension where one applies.
    pub fn refused(error: &str, dimension: Option<&str>) -> Decision {
        Decision {
            verdict: "refused".into(),
            error: Some(error.into()),
            dimension: dimension.map(str::to_string),
            zone: None,
        }
    }

    fn coverage(covered: bool) -> Decision {
        Decision::verdict(if covered { "covered" } else { "not_covered" })
    }

    fn placed(verdict: &str, zone: &Zone) -> Decision {
        Decision { zone: Some(zone.label()), ..Decision::verdict(verdict) }
    }

    /// The fields on which two decisions differ, in [`DECISION_FIELDS`] order.
    pub fn differing(&self, other: &Decision) -> Vec<&'static str> {
        let pairs = [
            self.verdict == other.verdict,
            self.error == other.error,
            self.dimension == other.dimension,
            self.zone == other.zone,
        ];
        DECISION_FIELDS.iter().zip(pairs).filter(|(_, same)| !same).map(|(f, _)| *f).collect()
    }
}

/// The decision on one case text: text that is not UTF-8 or not JSON is malformed.
pub fn decide(case: &[u8]) -> Decision {
    match std::str::from_utf8(case).ok().and_then(|text| serde_json::from_str::<Value>(text).ok()) {
        Some(value) => decide_value(&value),
        None => Decision::refused(CASE_MALFORMED, None),
    }
}

/// A fault decoding a case: its shape, or a domain parser's refusal.
enum Fault {
    Malformed,
    Refused(AuthorityError),
    Enforce(EnforceError),
}

impl From<EnforceError> for Fault {
    fn from(e: EnforceError) -> Fault {
        Fault::Enforce(e)
    }
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
        Err(Fault::Enforce(e)) => Decision::refused(e.identifier(), None),
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
        "zone_admits" => {
            let zone = string(required(obj, "zone")?)?;
            let allow = strings(required(obj, "allow")?)?;
            let entries = allow.iter().map(|e| Entry::parse(e)).collect::<Result<Vec<_>, _>>()?;
            let zone = Zone::parse(zone);
            let admitted = AllowSet::of(entries).admits(&zone);
            Ok(Decision::placed(if admitted { "admitted" } else { "excluded" }, &zone))
        }
        "session_zone" => {
            let asserted = optional(obj, "asserted", string)?;
            let signed = optional(obj, "signed", string)?;
            let incognito = required(obj, "incognito")?.as_bool().ok_or(Fault::Malformed)?;
            Ok(Decision::placed("placed", &session_zone(asserted, signed, incognito)?))
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

fn optional<'a, T>(obj: &'a Map<String, Value>, key: &str, f: impl Fn(&'a Value) -> Decoded<T>) -> Decoded<Option<T>> {
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

/// The module's exports on `wasm32-unknown-unknown`. The host copies a case into memory
/// from `contextful_alloc(len)`, calls `contextful_decide(ptr, len)`, which returns the
/// decision JSON's address in the high 32 bits and its length in the low 32, reads it, and
/// hands both regions back through `contextful_free(ptr, len)`.
#[cfg(target_arch = "wasm32")]
pub mod abi {
    /// A region of `len` bytes the host fills with a case.
    #[no_mangle]
    pub extern "C" fn contextful_alloc(len: u32) -> u32 {
        let region = vec![0u8; len as usize].into_boxed_slice();
        Box::into_raw(region) as *mut u8 as u32
    }

    /// Release a region this module handed out, input or output.
    ///
    /// # Safety
    /// `ptr` and `len` are one region `contextful_alloc` or `contextful_decide` returned.
    #[no_mangle]
    pub unsafe extern "C" fn contextful_free(ptr: u32, len: u32) {
        let region = std::ptr::slice_from_raw_parts_mut(ptr as *mut u8, len as usize);
        drop(Box::from_raw(region));
    }

    /// Decide the case at `ptr..ptr+len`.
    ///
    /// # Safety
    /// `ptr` and `len` are a region `contextful_alloc` returned, filled by the host.
    #[no_mangle]
    pub unsafe extern "C" fn contextful_decide(ptr: u32, len: u32) -> u64 {
        let case = std::slice::from_raw_parts(ptr as *const u8, len as usize);
        let out = serde_json::to_vec(&super::decide(case)).unwrap_or_default().into_boxed_slice();
        let len = out.len() as u64;
        let at = Box::into_raw(out) as *mut u8 as u64;
        (at << 32) | len
    }
}
