//! `disclosure.suppress`: the group-size floor, the contributor-share ceiling, and the
//! single reason-free sentinel every withheld group collapses into.
//!
//! A group is withheld when its noised distinct-contributor count falls under
//! `min_group_size`, or, under a share constraint, when one contributor holds more than
//! `max_contributor_share` of the group's sign-insensitive metric mass, or when the
//! per-contributor masses needed to check that are unavailable. The caller-facing output
//! carries only the published groups and at most one [`Sentinel`]; the reason per group
//! stays in the [`SuppressTally`] and the recorded refusals.

use serde::{Deserialize, Serialize};

use super::DisclosureError;

/// The group key a withheld group publishes under.
pub const DISCLOSURE_SENTINEL: &str = "__suppressed__";

/// The smallest `min_group_size` a policy declares, in distinct contributors.
pub const MIN_GROUP_SIZE_FLOOR: u64 = 2;

/// A validated suppression policy: at least one threshold, each inside its range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "PolicyDecl", into = "PolicyDecl")]
pub struct SuppressPolicy {
    min_group_size: Option<u64>,
    max_contributor_share: Option<f64>,
}

/// The thresholds as declared, before validation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PolicyDecl {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_group_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_contributor_share: Option<f64>,
}

impl TryFrom<PolicyDecl> for SuppressPolicy {
    type Error = DisclosureError;

    fn try_from(decl: PolicyDecl) -> Result<Self, DisclosureError> {
        SuppressPolicy::new(decl.min_group_size, decl.max_contributor_share)
    }
}

impl From<SuppressPolicy> for PolicyDecl {
    fn from(p: SuppressPolicy) -> Self {
        PolicyDecl { min_group_size: p.min_group_size, max_contributor_share: p.max_contributor_share }
    }
}

impl SuppressPolicy {
    /// Validates the thresholds. `max_contributor_share` is a fraction of the group's
    /// metric mass in `(0, 1]`; `min_group_size` is at least [`MIN_GROUP_SIZE_FLOOR`].
    pub fn new(min_group_size: Option<u64>, max_contributor_share: Option<f64>) -> Result<Self, DisclosureError> {
        if min_group_size.is_none() && max_contributor_share.is_none() {
            return Err(DisclosureError::PolicySuppressesNothing(
                "the policy sets neither min_group_size nor max_contributor_share".into(),
            ));
        }
        if let Some(k) = min_group_size {
            if k < MIN_GROUP_SIZE_FLOOR {
                return Err(DisclosureError::MinGroupSizeBelowFloor(format!(
                    "min_group_size {k} is below {MIN_GROUP_SIZE_FLOOR} distinct contributors"
                )));
            }
        }
        if let Some(share) = max_contributor_share {
            // The negated form also refuses NaN.
            if !(share > 0.0 && share <= 1.0) {
                return Err(DisclosureError::ShareOutOfRange(format!(
                    "max_contributor_share {share} lies outside (0, 1]"
                )));
            }
        }
        Ok(SuppressPolicy { min_group_size, max_contributor_share })
    }

    pub fn min_group_size(&self) -> Option<u64> {
        self.min_group_size
    }

    pub fn max_contributor_share(&self) -> Option<f64> {
        self.max_contributor_share
    }
}

/// One group's input to the decision.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupStats {
    /// The group's noised distinct-contributor count. The noise is applied upstream;
    /// the floor never sees the exact count.
    pub noised_contributors: u64,
    /// Each contributor's metric mass. `None`, an empty list, or any non-finite mass
    /// leaves dominance unverifiable.
    pub contributor_masses: Option<Vec<f64>>,
}

/// Why a group is withheld. Audit-side only: the sentinel carries none of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuppressReason {
    BelowMinGroupSize,
    DominantContributor,
    DominanceUnverifiable,
}

/// The decision over one group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupDecision {
    Publish,
    Suppress(SuppressReason),
}

/// Decides one group. The size floor runs first; the share ceiling runs only when the
/// policy declares one. A top share equal to the ceiling publishes.
pub fn evaluate_group(policy: &SuppressPolicy, stats: &GroupStats) -> GroupDecision {
    if let Some(k) = policy.min_group_size {
        if stats.noised_contributors < k {
            return GroupDecision::Suppress(SuppressReason::BelowMinGroupSize);
        }
    }
    let Some(max_share) = policy.max_contributor_share else {
        return GroupDecision::Publish;
    };
    let masses = match &stats.contributor_masses {
        Some(m) if !m.is_empty() && m.iter().all(|x| x.is_finite()) => m,
        _ => return GroupDecision::Suppress(SuppressReason::DominanceUnverifiable),
    };
    let total: f64 = masses.iter().map(|m| m.abs()).sum();
    // A group of zero total mass discloses no contributor's figure.
    if total > 0.0 {
        let top = masses.iter().map(|m| m.abs()).fold(0.0, f64::max);
        if top / total > max_share {
            return GroupDecision::Suppress(SuppressReason::DominantContributor);
        }
    }
    GroupDecision::Publish
}

/// The single marker every withheld group collapses into. A unit struct: it carries no
/// reason, count or group value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sentinel;

impl Sentinel {
    pub fn key(&self) -> &'static str {
        DISCLOSURE_SENTINEL
    }
}

/// Per-reason counts of withheld groups, for the audit record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SuppressTally {
    pub below_min_group_size: usize,
    pub dominant_contributor: usize,
    pub dominance_unverifiable: usize,
}

impl SuppressTally {
    pub fn total(&self) -> usize {
        self.below_min_group_size + self.dominant_contributor + self.dominance_unverifiable
    }
}

/// A result set after suppression.
#[derive(Debug, Clone, PartialEq)]
pub struct Suppressed<T> {
    /// The groups that publish, in input order.
    pub published: Vec<T>,
    /// Present exactly when at least one group is withheld, whatever the count or reason.
    pub sentinel: Option<Sentinel>,
    /// Audit-side counts per reason.
    pub tally: SuppressTally,
    /// One `DisclosureDominanceUnverifiable` per group withheld for missing masses. The
    /// message names the ceiling, never the group.
    pub refusals: Vec<DisclosureError>,
}

/// Decides every group, collapsing the withheld ones into one sentinel.
pub fn suppress<T>(policy: &SuppressPolicy, groups: Vec<(T, GroupStats)>) -> Suppressed<T> {
    let mut out = Suppressed { published: Vec::new(), sentinel: None, tally: SuppressTally::default(), refusals: Vec::new() };
    for (payload, stats) in groups {
        match evaluate_group(policy, &stats) {
            GroupDecision::Publish => out.published.push(payload),
            GroupDecision::Suppress(SuppressReason::BelowMinGroupSize) => out.tally.below_min_group_size += 1,
            GroupDecision::Suppress(SuppressReason::DominantContributor) => out.tally.dominant_contributor += 1,
            GroupDecision::Suppress(SuppressReason::DominanceUnverifiable) => {
                out.tally.dominance_unverifiable += 1;
                out.refusals.push(DisclosureError::DominanceUnverifiable(format!(
                    "per-contributor masses unavailable under max_contributor_share {}",
                    policy.max_contributor_share.unwrap_or_default()
                )));
            }
        }
    }
    out.sentinel = (out.tally.total() > 0).then_some(Sentinel);
    out
}

/// The attributes a suppression pass appends to the audit record: the thresholds and the
/// per-reason counts, none of which reach the published rows.
pub fn audit_attributes(policy: &SuppressPolicy, tally: &SuppressTally) -> serde_json::Value {
    serde_json::json!({
        "contextful.disclosure.suppressed_groups": tally.total(),
        "contextful.disclosure.below_min_group_size": tally.below_min_group_size,
        "contextful.disclosure.dominant_contributor": tally.dominant_contributor,
        "contextful.disclosure.dominance_unverifiable": tally.dominance_unverifiable,
        "contextful.disclosure.min_group_size": policy.min_group_size,
        "contextful.disclosure.max_contributor_share": policy.max_contributor_share,
    })
}
