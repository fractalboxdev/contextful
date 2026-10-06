//! Narrowing legality of a proposed child against its parent, per grant dimension.
//!
//! The checks run in the order the derivation flowchart gives: widening, expiry, tenant
//! scope, then the subject. The same comparison runs at derivation and, through
//! [`check_chain`], over every hop at admission (`authority.attenuate.narrowing`).

use crate::grant::{AggregateGrant, Grant};
use crate::identify::{NormalizedSubject, SubjectDerivation};
use crate::AuthorityError;

/// The effective authority of one credential in a chain. `exp` is Unix seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Authority {
    pub grants: Vec<Grant>,
    pub exp: i64,
    pub subject: NormalizedSubject,
}

/// What a child block proposes. An absent dimension inherits its parent's.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Proposal {
    pub grants: Option<Vec<Grant>>,
    pub exp: Option<i64>,
    pub subject: SubjectDerivation,
}

/// A grant dimension `AttenuationWidens` names (`authority.attenuate.widens`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Dimension {
    Actions,
    Tables,
    Templates,
    Aggregate,
}

impl Dimension {
    pub fn as_str(self) -> &'static str {
        match self {
            Dimension::Actions => "actions",
            Dimension::Tables => "tables",
            Dimension::Templates => "templates",
            Dimension::Aggregate => "aggregate",
        }
    }
}

/// The dimensions on which `child` is broader than `parent`, tenant scope aside.
fn widened(child: &Grant, parent: &Grant) -> Vec<Dimension> {
    let mut out = Vec::new();
    if !child.actions.iter().all(|a| parent.actions.contains(a)) {
        out.push(Dimension::Actions);
    }
    if !child.tables.iter().all(|c| parent.tables.iter().any(|p| p.covers(c))) {
        out.push(Dimension::Tables);
    }
    if !child.templates_within(parent) {
        out.push(Dimension::Templates);
    }
    if let (Some(c), Some(p)) = (&child.aggregate, &parent.aggregate) {
        if !aggregate_within(c, p) {
            out.push(Dimension::Aggregate);
        }
    }
    out
}

/// Every aggregate constraint holds or tightens, the groups-per-query ceilings as they take
/// effect; an absent child row ceiling inherits.
fn aggregate_within(child: &AggregateGrant, parent: &AggregateGrant) -> bool {
    child.min_group_size >= parent.min_group_size
        && child.max_contributor_share <= parent.max_contributor_share
        && child.functions.iter().all(|f| parent.functions.contains(f))
        && child.group_ceiling() <= parent.group_ceiling()
        && match (child.max_rows, parent.max_rows) {
            (Some(c), Some(p)) => c <= p,
            (None, Some(_)) | (_, None) => true,
        }
}

/// Whether `child` keeps `parent`'s tenant scope: the same table and the same bytes.
/// Adding a scope to an unscoped parent narrows.
fn tenant_kept(child: &Grant, parent: &Grant) -> bool {
    match &parent.tenant {
        None => true,
        Some(p) => child.tenant.as_ref() == Some(p),
    }
}

/// The child grant as it takes effect under `parent`: an absent aggregate inherits the
/// parent's, and the row ceiling is the lesser of the two.
fn effective(child: &Grant, parent: &Grant) -> Grant {
    let mut g = child.clone();
    if g.aggregate.is_none() {
        g.aggregate = parent.aggregate.clone();
    } else if let (Some(c), Some(p)) = (&mut g.aggregate, &parent.aggregate) {
        c.max_rows = [c.max_rows, p.max_rows].into_iter().flatten().min();
    }
    g.max_rows = [child.max_rows, parent.max_rows].into_iter().flatten().min();
    g.max_duration_ms = [child.max_duration_ms, parent.max_duration_ms].into_iter().flatten().min();
    g.max_response_bytes = [child.max_response_bytes, parent.max_response_bytes].into_iter().flatten().min();
    g
}

/// Check a proposed child against its parent and return its effective authority.
pub fn attenuate(parent: &Authority, child: &Proposal) -> Result<Authority, AuthorityError> {
    let grants = match &child.grants {
        None => parent.grants.clone(),
        Some(proposed) => {
            // Each child grant lies within one parent grant on every widening dimension.
            let mut candidates = Vec::with_capacity(proposed.len());
            for (i, c) in proposed.iter().enumerate() {
                let per_parent: Vec<Vec<Dimension>> = parent.grants.iter().map(|p| widened(c, p)).collect();
                let within: Vec<&Grant> =
                    parent.grants.iter().zip(&per_parent).filter(|(_, w)| w.is_empty()).map(|(p, _)| p).collect();
                if within.is_empty() {
                    let dimension = per_parent
                        .iter()
                        .min_by_key(|w| w.len())
                        .and_then(|w| w.first().copied())
                        .unwrap_or(Dimension::Actions);
                    return Err(AuthorityError::AttenuationWidens(format!(
                        "{}: child grant {i} is broader than every parent grant",
                        dimension.as_str()
                    )));
                }
                candidates.push(within);
            }
            if let Some(exp) = child.exp {
                check_expiry(exp, parent.exp)?;
            }
            let mut out = Vec::with_capacity(proposed.len());
            for (i, (c, within)) in proposed.iter().zip(candidates).enumerate() {
                let Some(p) = within.into_iter().find(|p| tenant_kept(c, p)) else {
                    return Err(AuthorityError::AttenuationTenantDropped(format!(
                        "child grant {i} drops its parent's tenant scope or names another tenant"
                    )));
                };
                out.push(effective(c, p));
            }
            out
        }
    };
    let exp = match child.exp {
        Some(exp) => {
            check_expiry(exp, parent.exp)?;
            exp
        }
        None => parent.exp,
    };
    let subject = parent.subject.derive(&child.subject)?;
    Ok(Authority { grants, exp, subject })
}

fn check_expiry(child: i64, parent: i64) -> Result<(), AuthorityError> {
    if child > parent {
        Err(AuthorityError::AttenuationExpiryExtended(format!(
            "child expiry {child} falls past its parent's {parent}"
        )))
    } else {
        Ok(())
    }
}

/// Check every hop of a chain against the authority before it, as admission does, and
/// return the final hop's effective authority.
pub fn check_chain(root: &Authority, hops: &[Proposal]) -> Result<Authority, AuthorityError> {
    hops.iter().try_fold(root.clone(), |parent, hop| attenuate(&parent, hop))
}
