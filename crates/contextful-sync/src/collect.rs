//! `store.collect`: delete the bucket objects the manifest no longer lists and no retained
//! generation names, and the generation manifests past `[sync] retain_generations`. Each key a
//! collect deletes is recorded as collected in a committed manifest before its object goes, so
//! no push lists a deleted object (`store.collect.drop-first`).

use crate::sync::{Result, Syncer};
use crate::transfer::each;
use contextful_core::store::sync::{generation_key, BucketManifest, Collected, PUSH_ROUND_CEILING};
use contextful_core::store::StoreError;
use contextful_core::time::Instant;
use std::collections::{BTreeMap, BTreeSet};

/// What a collect did, or under `--dry-run` does, to one class of objects
/// (`store.collect.command`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct CollectClass {
    /// Keys the collect records as collected.
    pub dropped: usize,
    /// Objects the collect deletes.
    pub deleted: usize,
    /// Bytes of the deleted objects whose size a manifest records.
    pub bytes: u64,
    /// Deleted objects no manifest the collect read records a size for.
    pub unknown_size: usize,
}

/// What one collect did per class (`store.collect.command`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct CollectReport {
    pub dry_run: bool,
    /// The bucket manifest's generation once the collect commits, or would commit.
    pub generation: u64,
    /// Objects the bucket manifest no longer lists (`store.collect.unlisted`).
    pub unlisted: CollectClass,
    /// Generation manifests past `[sync] retain_generations` (`store.collect.generations`).
    pub generations: CollectClass,
    /// The prefix holds another project's keys, so every generation manifest stays
    /// (`store.collect.shared-prefix`).
    pub shared_prefix: bool,
}

/// What one pass over the generation manifests leaves in memory: key sets, never manifests.
#[derive(Default)]
struct Generations {
    /// Keys of this project a generation past the count names (`store.collect.unlisted`).
    evidence: BTreeSet<String>,
    /// Keys of this project a retained generation lists (`store.collect.generation-listed`).
    protected: BTreeSet<String>,
    /// Keys the newest generation past the count lists, retained when the collect commits nothing.
    boundary: BTreeSet<String>,
    /// Each listed size of an evidence key.
    sizes: BTreeMap<String, u64>,
    /// Generations past the count, with their byte counts.
    pruned: Vec<(u64, u64)>,
}

impl Syncer {
    /// Collect the bucket (`store.collect`): record each unlisted object no retained generation
    /// lists as collected in a committed manifest, delete it, then delete the generation
    /// manifests past `[sync] retain_generations`. `dry_run` computes the same report and
    /// writes nothing.
    pub fn collect(&self, now: Instant, dry_run: bool) -> Result<CollectReport> {
        if !dry_run {
            self.admit_coordination()?;
        }
        let in_project = |k: &str| k.starts_with(&format!("{}/", self.project));
        // A collect heals an absent copy of the read generation before it lists the generations,
        // so the healed copy protects what it lists (`store.push.generation-heal`).
        let current = if dry_run { self.manifest()?.0 } else { self.read_for_commit()?.0 };
        // Generation manifests are the prefix's, so one project's count prunes them only while
        // the prefix holds that project alone; on a shared prefix every generation is retained
        // (`store.collect.shared-prefix`).
        let shared_prefix = current.entries.keys().chain(current.tombstones.keys()).chain(current.collected.keys()).chain(current.pointers.keys()).any(|k| !in_project(k))
            || current.control_heads.keys().any(|p| *p != self.project);
        let listed = self.generations()?;
        let floor = current.generation.max(listed.last().copied().unwrap_or(0));
        // The count as of the generation a commit writes; the boundary generation stays retained
        // when the collect commits nothing.
        let past = if shared_prefix { 0 } else { (floor + 1).saturating_sub(self.config.retain_generations()) };
        let keeps = |k: &str| in_project(k) && !self.redistribution.withholds(&self.project, k) && !is_group_marker(k);
        let mut g = Generations::default();
        for n in listed {
            let (m, bytes) = self.read_generation(n)?;
            if n > past {
                g.protected.extend(m.entries.into_keys().filter(|k| in_project(k)));
                continue;
            }
            if n == past {
                g.boundary.extend(m.entries.keys().filter(|k| in_project(k)).cloned());
            }
            for (k, e) in m.entries {
                if keeps(&k) && !current.entries.contains_key(&k) {
                    g.sizes.entry(k.clone()).or_insert(e.size);
                    g.evidence.insert(k);
                }
            }
            g.evidence.extend(m.tombstones.into_keys().filter(|k| keeps(k)));
            g.pruned.push((n, bytes));
        }
        let listing: BTreeSet<String> = {
            let strip = format!("{}/", self.prefix);
            self.bucket.list(&self.key(&format!("{}/", self.project))?)?.into_iter().map(|k| k.strip_prefix(&strip).map_or(k.clone(), str::to_string)).collect()
        };
        let deletable = |k: &str, commits: bool| listing.contains(k) && !g.protected.contains(k) && (commits || !g.boundary.contains(k));
        // The keys a commit over `m` records: an unlisted object a tombstone or a generation past
        // the count names, that no retained generation lists (`store.collect.unlisted`).
        let drops = |m: &BucketManifest| -> BTreeSet<String> {
            let standing = |k: &str| m.collected.get(k).is_some_and(|c| c.stands(now));
            m.tombstones.keys().filter(|k| keeps(k)).chain(&g.evidence).filter(|k| !m.entries.contains_key(*k) && !standing(k) && deletable(k, true)).cloned().collect()
        };

        let (committed, dropped) = if dry_run {
            let dropped = drops(&current);
            let next = if dropped.is_empty() { floor } else { floor + 1 };
            (with_collected(&current, &dropped, &self.node, now, next), dropped)
        } else {
            self.commit_collected(now, &drops)?
        };
        let next = committed.generation.max(floor);
        let commits = next > floor;
        // Generations concurrent pushes committed while the collect committed protect what they list.
        let mut later: BTreeSet<String> = BTreeSet::new();
        if !dry_run {
            for n in floor + 1..next {
                later.extend(self.read_generation(n)?.0.entries.into_keys());
            }
        }
        let doomed: BTreeSet<String> = committed
            .collected
            .iter()
            .filter(|(k, c)| c.stands(now) && keeps(k) && !committed.entries.contains_key(*k) && deletable(k, commits) && !later.contains(*k))
            .map(|(k, _)| k.clone())
            .collect();
        let pruned: Vec<(u64, u64)> = if shared_prefix { Vec::new() } else { g.pruned.iter().copied().filter(|(n, _)| commits || *n < past).collect() };
        let mut report = CollectReport { dry_run, generation: next, shared_prefix, ..CollectReport::default() };
        report.unlisted.dropped = dropped.len();
        for k in &doomed {
            report.unlisted.deleted += 1;
            match g.sizes.get(k) {
                Some(size) => report.unlisted.bytes += size,
                None => report.unlisted.unknown_size += 1,
            }
        }
        report.generations.deleted = pruned.len();
        report.generations.bytes = pruned.iter().map(|(_, b)| b).sum();
        if dry_run {
            return Ok(report);
        }
        let keys: Vec<String> = doomed.into_iter().map(|k| self.key(&k)).collect::<Result<_>>()?;
        each(self.config.transfers(), &keys, |k| Ok(self.bucket.delete(k)?))?;
        for (n, _) in pruned {
            self.bucket.delete(&self.key(&generation_key(n))?)?;
        }
        Ok(report)
    }

    /// Commit the bucket manifest with every key `drops` names for the manifest as read
    /// recorded as collected, by the conditional replace a push commits by; no commit when it
    /// names none. Returns the manifest that stands and the keys it recorded.
    fn commit_collected(&self, now: Instant, drops: &dyn Fn(&BucketManifest) -> BTreeSet<String>) -> Result<(BucketManifest, BTreeSet<String>)> {
        for _ in 0..PUSH_ROUND_CEILING {
            let (remote, etag, floor) = self.read_for_commit()?;
            let dropped = drops(&remote);
            if dropped.is_empty() {
                return Ok((remote, dropped));
            }
            let committed = with_collected(&remote, &dropped, &self.node, now, floor + 1);
            if self.put_commit(&committed, etag)? {
                return Ok((committed, dropped));
            }
        }
        Err(StoreError::SyncManifestRebaseExhausted(format!("the collect's manifest commit lost {PUSH_ROUND_CEILING} races and deleted nothing; run the collect again")).into())
    }

    /// Generation `n`'s manifest and its byte count; an absent one aborts the collect.
    fn read_generation(&self, n: u64) -> Result<(BucketManifest, u64)> {
        let key = generation_key(n);
        let Some((bytes, _)) = self.bucket.get(&self.key(&key)?)? else {
            return Err(StoreError::SyncGenerationAbsent(format!("`{key}` left the bucket while the collect read the generations; the collect deleted nothing past its commit and runs again")).into());
        };
        Ok((crate::sync::parse_manifest(&key, &bytes)?, bytes.len() as u64))
    }
}

/// Whether `key` is a grouped run's marker, which a member run needs to read as committed
/// (`store.collect.unlisted`).
fn is_group_marker(key: &str) -> bool {
    key.ends_with("/_group.json")
}

/// `m` with every key of `dropped` recorded as collected by `node` at `now` and records past
/// their age left out, committed as generation `next`.
fn with_collected(m: &BucketManifest, dropped: &BTreeSet<String>, node: &str, now: Instant, next: u64) -> BucketManifest {
    let mut out = m.clone();
    out.collected.retain(|_, c| c.stands(now));
    for key in dropped {
        out.entries.remove(key);
        out.collected.insert(key.clone(), Collected { collected_at: now, collected_by: node.to_string() });
    }
    out.generation = next;
    out.committed_at = Some(now);
    out.committed_by = Some(node.to_string());
    out
}
