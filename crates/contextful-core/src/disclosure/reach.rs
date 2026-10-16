//! `disclosure.reach`: per-request resolution of a subject to the resources it reaches
//! through one source's current rows, the bounded group closure, and the reachable-set cache.
//!
//! [`resolve`] is pure: the subject's seed principals, the group closure walked breadth-first
//! with a visited set under a [`ClosureBound`], then each known-class resource a current
//! grant names one of those principals on. A closure past its bound returns no set.

use super::sweep::{self, MemberRow, SourceState, UNKNOWN_CLASS};
use super::VisibilityError;
use crate::identify::NormalizedSubject;
use crate::AuthorityError;
use std::collections::{BTreeMap, BTreeSet, HashMap};

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

/// One resolved reachable set, and whether it was produced past a budget or under the
/// narrowing posture.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReachSet {
    pub resources: BTreeSet<String>,
    pub degraded: bool,
}

/// The resources of `origin` that `subject` reaches through its current rows: the seed
/// principals (`disclosure.reach.subject-principals`), their bounded group closure, then
/// each resource of a known class a current grant names a closure principal on. A grant
/// the run withdrew is absent from the current rows, so it confers nothing.
pub fn resolve(origin: &str, subject: &NormalizedSubject, state: &SourceState, bound: ClosureBound) -> Result<ReachSet, ReachError> {
    let principals = closure(origin, subject, sweep::seeds(subject, state)?, &state.members, bound)?;
    let known: BTreeSet<&str> = state.resources.iter().filter(|r| r.visibility_class != UNKNOWN_CLASS).map(|r| r.resource_id.as_str()).collect();
    let resources = state
        .grants
        .iter()
        .filter(|g| known.contains(g.resource_id.as_str()) && principals.contains(&(g.principal_kind.clone(), g.principal.clone())))
        .map(|g| g.resource_id.clone())
        .collect();
    Ok(ReachSet { resources, degraded: false })
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
    entries: HashMap<ReachKey, (u64, BTreeSet<String>)>,
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
    pub fn get(&mut self, key: &ReachKey) -> Option<BTreeSet<String>> {
        if !self.entries.contains_key(key) {
            return None;
        }
        self.touch(key);
        self.entries.get(key).map(|(_, set)| set.clone())
    }

    /// Retain `set` under `key`, evicting the least recently used entry past capacity. A
    /// degraded set raises `VisibilityDegradedCached` and is not retained
    /// (`disclosure.reach.degraded-uncached`).
    pub fn insert(&mut self, key: ReachKey, set: ReachSet) -> Result<(), VisibilityError> {
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
        self.entries.insert(key, (self.tick, set.resources));
        Ok(())
    }
}
