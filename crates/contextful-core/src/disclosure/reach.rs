//! `disclosure.reach`: per-request resolution of a subject to the resources it reaches
//! through one source's current rows, the bounded group closure, and the reachable-set cache.
//!
//! [`resolve`] is pure: the subject's seed principals, the group closure walked breadth-first
//! with a visited set under a [`ClosureBound`], then each known-class resource a current
//! grant names one of those principals, or the `public` principal, on, with the level and
//! expiry of each such grant. A closure past its bound returns no set. [`ReachSet::admitted`]
//! applies the semi-join's level and expiry filter at the instant a read reads as now.

use super::sweep::{self, MemberRow, SourceState, UNKNOWN_CLASS};
use super::VisibilityError;
use crate::identify::NormalizedSubject;
use crate::time::Instant;
use crate::AuthorityError;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

/// Default depth of the group-closure walk, in hops (`disclosure.reach.closure-walk`).
pub const MAX_GROUP_DEPTH: usize = 8;

/// Largest group closure one request walks, in nodes (`disclosure.reach.closure-walk`).
pub const MAX_CLOSURE_NODES: usize = 10_000;

/// Capacity of the reachable-set cache, in entries (`disclosure.reach.cache-capacity`).
pub const REACH_CACHE_ENTRIES: usize = 100_000;

/// A principal: its kind and its name, matched together.
pub type Principal = (String, String);

/// The bounds one closure walks under: `max_group_depth` hops of group edges and
/// `max_nodes` principals in all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClosureBound {
    pub max_group_depth: usize,
    pub max_nodes: usize,
}

impl Default for ClosureBound {
    fn default() -> ClosureBound {
        ClosureBound { max_group_depth: MAX_GROUP_DEPTH, max_nodes: MAX_CLOSURE_NODES }
    }
}

/// What resolving a subject refuses with: an identity-link refusal or a closure bound.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ReachError {
    #[error(transparent)]
    Authority(#[from] AuthorityError),
    #[error(transparent)]
    Visibility(#[from] VisibilityError),
}

/// The group closure of `seeds` over `members`: every principal reachable through
/// membership edges, walked level by level with a visited set, so a cycle ends at its
/// first revisit (`disclosure.reach.closure-walk`). An edge to an unvisited group past
/// `max_group_depth` hops, or a closure holding more than `max_nodes` principals, raises
/// `VisibilityClosureDepth` and returns no set (`disclosure.reach.closure-overflow`).
pub fn closure(
    origin: &str,
    subject: &NormalizedSubject,
    seeds: BTreeSet<Principal>,
    members: &[MemberRow],
    bound: ClosureBound,
) -> Result<BTreeSet<Principal>, VisibilityError> {
    let overflow = |what: String| VisibilityError::ClosureDepth { origin: origin.to_string(), subject: subject.identity().unwrap_or("").to_string(), bound: what };
    let mut edges: HashMap<(&str, &str), Vec<&str>> = HashMap::new();
    for m in members {
        edges.entry((m.member_kind.as_str(), m.member.as_str())).or_default().push(m.group.as_str());
    }
    if seeds.len() > bound.max_nodes {
        return Err(overflow(format!("{} nodes", bound.max_nodes)));
    }
    let mut visited = seeds;
    let mut frontier: Vec<Principal> = visited.iter().cloned().collect();
    let mut depth = 0usize;
    while !frontier.is_empty() {
        let mut next = Vec::new();
        for (kind, name) in &frontier {
            for group in edges.get(&(kind.as_str(), name.as_str())).into_iter().flatten() {
                let principal = ("group".to_string(), (*group).to_string());
                if visited.contains(&principal) {
                    continue;
                }
                if depth >= bound.max_group_depth {
                    return Err(overflow(format!("{} hops", bound.max_group_depth)));
                }
                visited.insert(principal.clone());
                if visited.len() > bound.max_nodes {
                    return Err(overflow(format!("{} nodes", bound.max_nodes)));
                }
                next.push(principal);
            }
        }
        frontier = next;
        depth += 1;
    }
    Ok(visited)
}

/// The principal kind a grant open to anyone names (`disclosure.reach.public-grant`).
pub const PUBLIC_KIND: &str = "public";

/// Whether a grant of the source principal kind `kind` names the `public` principal: its
/// projection through `kinds`, the `principal_kind` table of the packs mapping the source's
/// tables, or the kind verbatim where that table holds no entry for it
/// (`disclosure.reach.public-grant`).
pub fn is_public(kind: &str, kinds: Option<&BTreeMap<String, String>>) -> bool {
    kinds.and_then(|k| k.get(kind)).map_or(kind, String::as_str) == PUBLIC_KIND
}

/// The levels a pack mapping projects onto that confer read (`disclosure.reach.read-level`).
pub const READ_LEVELS: [&str; 3] = ["read", "write", "admin"];

/// Whether a grant at `level` confers read: its projection through `levels`, the `level`
/// table of the pack mapping its table, or the level verbatim where no pack maps that table,
/// is `read`, `write` or `admin` (`disclosure.reach.read-level`).
pub fn confers_read(level: &str, levels: Option<&BTreeMap<String, String>>) -> bool {
    let projected = match levels {
        Some(map) => map.get(level).map(String::as_str),
        None => Some(level),
    };
    projected.is_some_and(|l| READ_LEVELS.contains(&l))
}

/// One grant a reached resource holds: its source level and, on an opening, its expiry.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Held {
    pub level: String,
    pub expires_at: Option<Instant>,
}

impl Held {
    /// Whether this grant reads at `now`: read-conferring under `levels` and unexpired
    /// (`disclosure.reach.semi-join`).
    pub fn reads(&self, levels: Option<&BTreeMap<String, String>>, now: Instant) -> bool {
        confers_read(&self.level, levels) && self.expires_at.is_none_or(|e| now < e)
    }
}

/// One resolved reachable set — each reached resource with the grants reaching it — and
/// whether it was produced past a budget or under the narrowing posture.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReachSet {
    pub held: BTreeMap<String, BTreeSet<Held>>,
    pub degraded: bool,
}

impl ReachSet {
    /// Every reached resource, whatever its grants' levels and expiry.
    pub fn resources(&self) -> BTreeSet<&str> {
        self.held.keys().map(String::as_str).collect()
    }

    /// The resources the semi-join admits at `now`: each holding a read-conferring,
    /// unexpired grant (`disclosure.reach.semi-join`). Withdrawn grants are absent from the
    /// current rows, so a tombstoned grant admits nothing.
    pub fn admitted(&self, levels: Option<&BTreeMap<String, String>>, now: Instant) -> BTreeSet<String> {
        self.held.iter().filter(|(_, grants)| grants.iter().any(|g| g.reads(levels, now))).map(|(r, _)| r.clone()).collect()
    }

    /// How many held grants have expired at `now`. It rises exactly when an opening lapses,
    /// so a session keyed on it retires with the opening.
    pub fn expired(&self, now: Instant) -> usize {
        self.held.values().flatten().filter(|g| g.expires_at.is_some_and(|e| e <= now)).count()
    }
}

/// The resources of `origin` that `subject` reaches through its current rows: the seed
/// principals (`disclosure.reach.subject-principals`), their bounded group closure, then
/// each resource of a known class a current grant names a closure principal, or the
/// `public` principal, on (`disclosure.reach.public-grant`). A grant the run withdrew is
/// absent from the current rows, so it confers nothing. [`resolve_mapped`] under no pack's
/// kind mapping.
pub fn resolve(origin: &str, subject: &NormalizedSubject, state: &SourceState, bound: ClosureBound) -> Result<ReachSet, ReachError> {
    resolve_mapped(origin, subject, state, bound, None)
}

/// [`resolve`], a grant naming the `public` principal when its kind projects onto `public`
/// through `kinds` ([`is_public`]).
pub fn resolve_mapped(origin: &str, subject: &NormalizedSubject, state: &SourceState, bound: ClosureBound, kinds: Option<&BTreeMap<String, String>>) -> Result<ReachSet, ReachError> {
    let principals = closure(origin, subject, sweep::seeds(subject, state)?, &state.members, bound)?;
    let known: BTreeSet<&str> = state.resources.iter().filter(|r| r.visibility_class != UNKNOWN_CLASS).map(|r| r.resource_id.as_str()).collect();
    let mut held: BTreeMap<String, BTreeSet<Held>> = BTreeMap::new();
    for g in state.grants.iter().filter(|g| known.contains(g.resource_id.as_str())) {
        if is_public(&g.principal_kind, kinds) || principals.contains(&(g.principal_kind.clone(), g.principal.clone())) {
            held.entry(g.resource_id.clone()).or_default().insert(Held { level: g.level.clone(), expires_at: g.expires_at });
        }
    }
    Ok(ReachSet { held, degraded: false })
}

/// The resources of a known class holding a read-conferring grant to the `public`
/// principal, under the level mapping `levels` and the kind mapping `kinds`, that is
/// unexpired at `now`, each with the instant the sweep observed it.
pub fn public_resources(state: &SourceState, levels: Option<&BTreeMap<String, String>>, kinds: Option<&BTreeMap<String, String>>, now: Instant) -> BTreeMap<String, Instant> {
    let public: BTreeSet<&str> = state
        .grants
        .iter()
        .filter(|g| is_public(&g.principal_kind, kinds) && Held { level: g.level.clone(), expires_at: g.expires_at }.reads(levels, now))
        .map(|g| g.resource_id.as_str())
        .collect();
    state
        .resources
        .iter()
        .filter(|r| r.visibility_class != UNKNOWN_CLASS && public.contains(r.resource_id.as_str()))
        .map(|r| (r.resource_id.clone(), r.observed_at))
        .collect()
}

/// How many `public` openings of `state`, under the kind mapping `kinds`, have expired at
/// `now`. It rises exactly when one lapses, so a narrowed session keyed on it retires with it.
pub fn expired_public(state: &SourceState, kinds: Option<&BTreeMap<String, String>>, now: Instant) -> usize {
    state.grants.iter().filter(|g| is_public(&g.principal_kind, kinds) && g.expires_at.is_some_and(|e| e <= now)).count()
}

/// The key a reachable set is cached under: the subject, the directory epoch, and the epoch
/// of each source the read touches (`disclosure.reach.cache-epochs`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReachKey {
    pub subject: String,
    pub directory_epoch: u64,
    pub sources: BTreeMap<String, u64>,
}

/// Reachable sets by [`ReachKey`], holding at most its capacity and evicting the least
/// recently used (`disclosure.reach.cache-capacity`). A sweep advances its source's epoch,
/// so a key naming that source no longer matches, and a key naming only other sources still
/// does.
#[derive(Debug, Clone)]
pub struct ReachCache {
    capacity: usize,
    tick: u64,
    entries: HashMap<ReachKey, (u64, Arc<ReachSet>)>,
    recency: BTreeMap<u64, ReachKey>,
}

impl Default for ReachCache {
    fn default() -> ReachCache {
        ReachCache::with_capacity(REACH_CACHE_ENTRIES)
    }
}

impl ReachCache {
    pub fn with_capacity(capacity: usize) -> ReachCache {
        ReachCache { capacity, tick: 0, entries: HashMap::new(), recency: BTreeMap::new() }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn touch(&mut self, key: &ReachKey) -> u64 {
        self.tick += 1;
        if let Some((used, _)) = self.entries.get_mut(key) {
            self.recency.remove(used);
            *used = self.tick;
            self.recency.insert(self.tick, key.clone());
        }
        self.tick
    }

    /// The set cached under `key`, marking it the most recently used.
    pub fn get(&mut self, key: &ReachKey) -> Option<Arc<ReachSet>> {
        if !self.entries.contains_key(key) {
            return None;
        }
        self.touch(key);
        self.entries.get(key).map(|(_, set)| set.clone())
    }

    /// Retain `set` under `key`, evicting the least recently used entry past capacity, and
    /// return the retained set. A degraded set raises `VisibilityDegradedCached` and is not
    /// retained (`disclosure.reach.degraded-uncached`).
    pub fn insert(&mut self, key: ReachKey, set: ReachSet) -> Result<Arc<ReachSet>, VisibilityError> {
        if set.degraded {
            return Err(VisibilityError::DegradedCached { what: format!("the reachable set of `{}`", key.subject) });
        }
        if let Some((used, _)) = self.entries.remove(&key) {
            self.recency.remove(&used);
        }
        while self.entries.len() >= self.capacity.max(1) {
            let Some((_, oldest)) = self.recency.pop_first() else { break };
            self.entries.remove(&oldest);
        }
        self.tick += 1;
        self.recency.insert(self.tick, key.clone());
        let set = Arc::new(set);
        self.entries.insert(key, (self.tick, set.clone()));
        Ok(set)
    }
}
