//! `read.revise`: claim standing, the dedup key, supersession within one validity line,
//! and the shapes the direct write accepts.

use super::declare::Shape;
use super::synthesize::EvidenceRef;
use super::MemoryError;
use crate::time::Instant;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Validity a lower-standing contradiction lands with: it neither retires its prior nor
/// stands open-ended beside it, in seconds.
pub const CONTRADICTION_VALIDITY_SECS: u64 = 30 * 24 * 60 * 60;

/// A claim's standing, lowest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Researched,
    Derived,
    Curated,
}

impl Tier {
    /// Every tier, lowest standing first; a statement ranking tiers reads its order here.
    pub const ALL: [Tier; 3] = [Tier::Researched, Tier::Derived, Tier::Curated];

    pub fn name(self) -> &'static str {
        match self {
            Tier::Researched => "researched",
            Tier::Derived => "derived",
            Tier::Curated => "curated",
        }
    }

    pub fn parse(s: &str) -> Option<Tier> {
        Some(match s {
            "researched" => Tier::Researched,
            "derived" => Tier::Derived,
            "curated" => Tier::Curated,
            _ => return None,
        })
    }
}

/// How a claim reached the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WritePath {
    /// A synthesis pass over landed items.
    Synthesis,
    /// A principal's direct write.
    Direct,
}

/// The source a claim is grounded in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grounding {
    /// Rows a connector landed.
    Landed,
    /// A result fetched for the question.
    Fetched,
}

/// The tier a claim is stamped with: from its write path and its grounding, never from
/// its payload; mixed grounding takes the lowest (`read.revise.tier`).
pub fn tier(path: WritePath, grounding: &[Grounding]) -> Tier {
    let base = match path {
        WritePath::Synthesis => Tier::Derived,
        WritePath::Direct => Tier::Curated,
    };
    grounding.iter().map(|g| match g {
        Grounding::Landed => base,
        Grounding::Fetched => Tier::Researched,
    })
    .fold(base, Tier::min)
}

/// One claim row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    pub claim_id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub scope: Option<String>,
    pub tier: Tier,
    pub confidence: f64,
    pub valid_from: Instant,
    pub valid_to: Option<Instant>,
    pub evidence: Vec<EvidenceRef>,
    pub superseded_by: Option<String>,
    pub grant_id: String,
    pub agent: Option<String>,
}

/// The dedup key: one claim per subject, predicate, object and scope.
pub fn claim_id(subject: &str, predicate: &str, object: &str, scope: Option<&str>) -> String {
    let mut h = Sha256::new();
    for part in [subject, predicate, object, scope.unwrap_or("")] {
        h.update((part.len() as u64).to_be_bytes());
        h.update(part.as_bytes());
    }
    format!("c-{}", h.finalize().iter().take(12).map(|b| format!("{b:02x}")).collect::<String>())
}

/// The dedup key a writer supplies: one claim per key, subject, predicate, object and
/// scope, so a retried observation restates and the same fact under two keys keeps two
/// validity intervals (`read.revise.dedup-key`).
pub fn keyed_claim_id(dedup_key: &str, subject: &str, predicate: &str, object: &str, scope: Option<&str>) -> String {
    let mut h = Sha256::new();
    h.update(b"dedup_key");
    for part in [dedup_key, subject, predicate, object, scope.unwrap_or("")] {
        h.update((part.len() as u64).to_be_bytes());
        h.update(part.as_bytes());
    }
    format!("c-{}", h.finalize().iter().take(12).map(|b| format!("{b:02x}")).collect::<String>())
}

impl Claim {
    fn same_line(&self, other: &Claim) -> bool {
        self.subject == other.subject && self.predicate == other.predicate && self.scope == other.scope
    }

    /// Whether the claim is live at `at`: not superseded, and its validity not ended.
    pub fn live_at(&self, at: Instant) -> bool {
        self.superseded_by.is_none() && self.valid_to.is_none_or(|end| end > at)
    }
}

/// A claim observed before a live, unsuperseded claim of its line with another object
/// refuses: retiring that later prior at the earlier instant would invert its interval
/// (`read.revise.observed-order`).
pub fn observed_order(new: &Claim, stored: &[Claim]) -> Result<(), MemoryError> {
    match stored.iter().find(|p| {
        p.superseded_by.is_none() && p.same_line(new) && p.object != new.object && p.valid_from > new.valid_from
    }) {
        Some(later) => Err(MemoryError::ObservationOutOfOrder(format!(
            "`{}` {} is observed at {}, before `{}` holds from {}",
            new.subject,
            new.predicate,
            new.valid_from.to_rfc3339(),
            later.object,
            later.valid_from.to_rfc3339()
        ))),
        None => Ok(()),
    }
}

/// What revising the live claims with one new claim does.
#[derive(Debug, Clone, PartialEq)]
pub struct Revision {
    /// The new claim as it lands, or `None` where it restates a live claim.
    pub landed: Option<Claim>,
    /// Priors retired by the new claim, as they land.
    pub retired: Vec<Claim>,
}

/// Revise `live` with `new`. Only claims live at the new claim's `valid_from` take part.
/// A live prior on the same subject, predicate, object and scope is restated: nothing
/// lands unless the new claim stands higher, which re-lands the claim at its tier. A
/// contradicting prior is on the new claim's validity line when both are open-ended or
/// both start at one instant; there a claim of equal or higher tier retires it — its
/// `valid_to` becomes the new claim's `valid_from` and `superseded_by` names the new
/// claim — and a lower-tier claim lands with a bounded validity end instead
/// (`read.revise.supersede`).
pub fn revise(new: Claim, live: &[Claim]) -> Revision {
    let line: Vec<&Claim> = live.iter().filter(|p| p.live_at(new.valid_from) && p.same_line(&new)).collect();
    if let Some(same) = line.iter().find(|p| p.object == new.object) {
        if line.iter().all(|p| p.object == new.object) {
            let promoted = (new.tier > same.tier).then_some(new);
            return Revision { landed: promoted, retired: Vec::new() };
        }
        if new.claim_id == same.claim_id {
            // A reaffirmation of the existing claim can end competing objects without
            // replacing that claim's earlier validity interval.
            let retired = line
                .iter()
                .filter(|prior| prior.object != new.object && new.tier >= prior.tier && (prior.valid_to.is_none() || prior.valid_from == new.valid_from))
                .map(|prior| Claim { valid_to: Some(new.valid_from), superseded_by: Some(new.claim_id.clone()), ..(*prior).clone() })
                .collect();
            return Revision { landed: None, retired };
        }
    }
    let mut new = new;
    let mut retired = Vec::new();
    for prior in line {
        let on_line = (prior.valid_to.is_none() && new.valid_to.is_none()) || prior.valid_from == new.valid_from;
        if !on_line {
            continue;
        }
        if new.tier >= prior.tier {
            retired.push(Claim { valid_to: Some(new.valid_from), superseded_by: Some(new.claim_id.clone()), ..prior.clone() });
        } else {
            new.valid_to = Some(new.valid_from.plus_secs(CONTRADICTION_VALIDITY_SECS));
        }
    }
    Revision { landed: Some(new), retired }
}

/// The direct write accepts claims alone; any other memory shape refuses
/// (`read.revise.direct-write`).
pub fn direct_write(table: &str, shape: Shape) -> Result<(), MemoryError> {
    match shape {
        Shape::Facts => Ok(()),
        other => Err(MemoryError::DirectWriteShapeRefused(format!(
            "table `{table}` holds `{}`; the direct write accepts `memory_facts` claims alone{}",
            other.name(),
            if other == Shape::Entities { ", and an entity row enters through the entity upsert" } else { "" }
        ))),
    }
}

/// The factor a claim's ranked score carries under a declared half-life: one half per
/// half-life elapsed from `valid_from` to `anchor`. No half-life, no `valid_from`, or a
/// `valid_from` at or after the anchor weighs 1. The factor ranks alone; it never ends,
/// retires or deletes a claim (`read.revise.retention-default`).
pub fn decay_weight(half_life_secs: Option<u64>, valid_from: Option<Instant>, anchor: Instant) -> f64 {
    match (half_life_secs, valid_from) {
        (Some(half_life), Some(from)) if half_life > 0 => 0.5f64.powf(from.secs_until(anchor) as f64 / half_life as f64),
        _ => 1.0,
    }
}
