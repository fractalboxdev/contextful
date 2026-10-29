//! `store.collect`: unlist, then delete, the bucket objects no reader needs. A collect records
//! each key it unlists as collected in a committed manifest before any object goes
//! (`store.collect.drop-first`), and deletes an unlisted object only once no retained generation
//! lists it (`store.collect.two-phase`). Beside objects the manifest no longer lists, it reclaims
//! a superseded snapshot's parts and sidecars, keeping its `_manifest.json`, and the run
//! directories a snapshot older than the window folds (`store.collect.superseded`,
//! `store.collect.folded-runs`); it then deletes the generation manifests past
//! `[sync] retain_generations`.

use crate::sync::{project_table, Held, Result, SyncError, Syncer};
use crate::transfer::each;
use contextful_context::ContextError;
use contextful_core::pipeline::model::HoldRecord;
use contextful_core::run::journal::sha256_hex;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::{SnapshotManifest, COLLECTED_FILE, MANIFEST_FILE, POINTER_FILE, RUNS_DIR, SNAPSHOTS_DIR};
use contextful_core::store::lease::{BucketLease, BucketPointer};
use contextful_core::store::object::{CasScope, Condition, Put};
use contextful_core::store::sync::{generation_key, BucketManifest, Collected, Entry, PUSH_ROUND_CEILING, TOMBSTONE_TTL_SECS};
use contextful_core::store::StoreError;
use contextful_core::time::Instant;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

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
    /// A superseded snapshot's parts and sidecars (`store.collect.superseded`).
    pub superseded: CollectClass,
    /// Run directories a snapshot older than the window folds (`store.collect.folded-runs`).
    pub folded_runs: CollectClass,
    /// Generation manifests past `[sync] retain_generations` (`store.collect.generations`).
    pub generations: CollectClass,
    /// Tables the collecting store declares no `retain_runs` window for, whose snapshots and
    /// runs stay (`store.collect.declared-window`).
    pub undeclared: Vec<String>,
    /// The prefix holds another project's keys, so every generation manifest stays and the
    /// collect unlists nothing (`store.collect.shared-prefix`).
    pub shared_prefix: bool,
    /// Recorded keys a retained snapshot names, whose deletion the collect skipped
    /// (`store.collect.retained-kept`).
    pub referenced: Vec<String>,
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

/// What the tables' chains let a collect reclaim from one bucket manifest
/// (`store.collect.superseded`, `store.collect.folded-runs`).
#[derive(Default)]
struct Reclaim {
    /// Listed parts and sidecars of chain snapshots superseded longer ago than the window.
    superseded: BTreeSet<String>,
    /// Listed objects of run directories a chain snapshot older than the window folds, and
    /// each such run's `_group.json` no member table still needs.
    folded: BTreeSet<String>,
    /// The directory prefix of each collected snapshot.
    snapshot_dirs: BTreeSet<String>,
    /// The directory prefix of each run a chain snapshot older than the window folds.
    run_dirs: BTreeSet<String>,
    /// Each key, or key prefix, a retained snapshot names: no collect deletes one
    /// (`store.collect.retained-kept`).
    retained: BTreeSet<String>,
    /// The `_collected.json` of each collected snapshot the manifest does not list yet, with
    /// its sealed bytes.
    marks: BTreeMap<String, Vec<u8>>,
    undeclared: Vec<String>,
}

/// The key of the bucket's collect lease under the prefix (`store.collect.serialized`).
pub fn collect_lease_key(project: &str) -> String {
    format!("{project}/leases/collect.json")
}

/// One table's chain as the bucket holds it.
struct Chain {
    table: String,
    cutoff: Instant,
    snapshots: Vec<SnapshotManifest>,
    /// Each run key a chain snapshot folds whose parent collect reclaims: the window has passed
    /// that fold, whatever clock reads it (`store.collect.folded-runs`).
    folded_out: BTreeSet<String>,
    /// The build ids an unexpired hold in the bucket names, read under the collect lease.
    held: BTreeSet<String>,
}

impl Syncer {
    /// [`Syncer::collect_declared`] against no declaration: every table's snapshots and runs
    /// stay, and only unlisted objects and generations are collected.
    pub fn collect(&self, now: Instant, dry_run: bool) -> Result<CollectReport> {
        self.collect_declared(now, dry_run, &[])
    }

    /// Collect the bucket (`store.collect`): record as collected each object no reader needs,
    /// in one committed manifest, then delete each recorded object no retained generation
    /// lists, then the generation manifests past `[sync] retain_generations`. `decls` gives
    /// each table's `retain_runs` window. `dry_run` computes the same report and writes nothing.
    pub fn collect_declared(&self, now: Instant, dry_run: bool, decls: &[TableDecl]) -> Result<CollectReport> {
        if dry_run {
            return self.collect_under(now, true, decls, None);
        }
        self.admit_coordination()?;
        // One collect at a time: every read of holds, the record, the commit and the deletes run
        // under the bucket's collect lease (`store.collect.serialized`).
        let Some(held) = self.acquire_collect(now)? else { return self.collect_under(now, false, decls, None) };
        let done = self.collect_under(now, false, decls, Some(&held));
        let released = self.release_collect();
        let report = done?;
        released?;
        Ok(report)
    }

    /// Acquire the bucket's collect lease at `now` (`store.collect.serialized`); `None` where the
    /// bucket's conditional writes hold on one machine only, which admits a single writer.
    pub fn acquire_collect(&self, now: Instant) -> Result<Option<Held>> {
        if let CasScope::Machine(_) = self.bucket.cas_scope() {
            return Ok(None);
        }
        let key = self.key(&collect_lease_key(&self.project))?;
        loop {
            let current = self.bucket.get(&key)?;
            let parsed: Option<(BucketLease, String)> = match &current {
                Some((b, etag)) => Some((serde_json::from_slice(b).map_err(|e| SyncError::Context(ContextError::Invalid(format!("lease `{key}`: {e}"))))?, etag.clone())),
                None => None,
            };
            let (lease, condition) = BucketLease::acquire(parsed.as_ref().map(|(l, e)| (l, e.as_str())), &self.node, now)?;
            let bytes = serde_json::to_vec_pretty(&lease).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?;
            if let Put::Applied(etag) = self.bucket.put(&key, &bytes, condition)? {
                return Ok(Some(Held { key, lease, etag }));
            }
        }
    }

    /// Release the bucket's collect lease this node holds.
    pub fn release_collect(&self) -> Result<()> {
        let key = self.key(&collect_lease_key(&self.project))?;
        loop {
            let Some((b, etag)) = self.bucket.get(&key)? else {
                return Err(StoreError::LeaseNotHeld(format!("no lease `{key}` exists")).into());
            };
            let current: BucketLease = serde_json::from_slice(&b).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?;
            let (released, condition) = BucketLease::release(&current, &etag, &self.node)?;
            let bytes = serde_json::to_vec_pretty(&released).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?;
            if let Put::Applied(_) = self.bucket.put(&key, &bytes, condition)? {
                return Ok(());
            }
        }
    }

    /// Renew the collect lease `held` at `now`; a lease another collect fenced past refuses.
    fn renew_collect(&self, held: &Held, now: Instant) -> Result<()> {
        loop {
            let Some((b, etag)) = self.bucket.get(&held.key)? else {
                return Err(StoreError::LeaseNotHeld(format!("no lease `{}` exists", held.key)).into());
            };
            let current: BucketLease = serde_json::from_slice(&b).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?;
            let (lease, condition) = BucketLease::renew(&current, &etag, &held.lease, now)?;
            let bytes = serde_json::to_vec_pretty(&lease).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?;
            if let Put::Applied(_) = self.bucket.put(&held.key, &bytes, condition)? {
                return Ok(());
            }
        }
    }

    fn collect_under(&self, now: Instant, dry_run: bool, decls: &[TableDecl], lease: Option<&Held>) -> Result<CollectReport> {
        let in_project = |k: &str| k.starts_with(&format!("{}/", self.project));
        // A collect heals an absent copy of the read generation before it lists the generations,
        // so the healed copy protects what it lists (`store.push.generation-heal`).
        let current = if dry_run { self.manifest()?.0 } else { self.read_for_commit()?.0 };
        // Generation manifests are the prefix's, so one project's count prunes them only while
        // the prefix holds that project alone; on a shared prefix every generation is retained
        // and nothing is unlisted (`store.collect.shared-prefix`).
        let shared_prefix = current.entries.keys().chain(current.tombstones.keys()).chain(current.collected.keys()).chain(current.pointers.keys()).any(|k| !in_project(k))
            || current.control_heads.keys().any(|p| *p != self.project);
        let listed = self.generations()?;
        let floor = current.generation.max(listed.last().copied().unwrap_or(0));
        // The count as of the generation a commit writes; the boundary generation stays retained
        // when the collect commits nothing.
        let past = if shared_prefix { 0 } else { (floor + 1).saturating_sub(self.config.retain_generations()) };
        let withheld = |k: &str| self.redistribution.withholds(&self.project, k);
        let keeps = |k: &str| in_project(k) && !withheld(k) && !is_group_marker(k);
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
                if in_project(&k) && !withheld(&k) && !current.entries.contains_key(&k) {
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
        let reclaim_of = |m: &BucketManifest| -> Result<Reclaim> { if shared_prefix { Ok(Reclaim::default()) } else { self.reclaim(m, now, decls) } };
        let reclaim = reclaim_of(&current)?;
        let retained = |r: &Reclaim, k: &str| r.retained.iter().any(|p| k == p || k.starts_with(&format!("{p}/")));
        // A key the manifest read before the listing names, whose object the listing lacks, is
        // recorded so the bucket recovers from a listed key with no object (`store.collect.repair`).
        let repair: BTreeSet<String> = current.entries.keys().filter(|k| in_project(k) && !withheld(k) && !listing.contains(*k)).cloned().collect();
        // The keys a commit over `m` records: an unlisted object a tombstone or a generation past
        // the count names (`store.collect.unlisted`), a listed object the chains reclaim
        // (`store.collect.superseded`, `store.collect.folded-runs`), and a listed key with no
        // object. A retained generation listing a key delays its deletion, never its record
        // (`store.collect.two-phase`).
        let drops = |m: &BucketManifest, r: &Reclaim| -> BTreeSet<String> {
            if shared_prefix {
                return BTreeSet::new();
            }
            let standing = |k: &str| m.collected.get(k).is_some_and(|c| c.stands(now));
            let unlisted = m.tombstones.keys().filter(|k| keeps(k)).chain(g.evidence.iter().filter(|k| keeps(k))).filter(|k| !m.entries.contains_key(*k));
            let reclaimed = r.superseded.iter().chain(&r.folded).filter(|k| m.entries.contains_key(*k) && !withheld(k));
            let mut out: BTreeSet<String> = unlisted.chain(reclaimed).filter(|k| !standing(k) && !retained(r, k) && listing.contains(*k)).cloned().collect();
            out.extend(repair.iter().filter(|k| m.entries.contains_key(*k)).cloned());
            out
        };
        // A record stands while its object stays: each past half its age is stamped again
        // (`store.collect.record-stands`).
        let restamp = |m: &BucketManifest| -> BTreeSet<String> {
            m.collected.iter().filter(|(k, c)| c.stands(now) && listing.contains(*k) && c.collected_at.secs_until(now) >= TOMBSTONE_TTL_SECS / 2).map(|(k, _)| k.clone()).collect()
        };

        let (committed, dropped) = if dry_run {
            let dropped = drops(&current, &reclaim);
            let restamped = restamp(&current);
            let changes = !dropped.is_empty() || !reclaim.marks.is_empty() || !restamped.is_empty();
            let next = if changes { floor + 1 } else { floor };
            (with_collected(&current, &dropped, &restamped, &BTreeMap::new(), &self.node, now, next), dropped)
        } else {
            self.commit_collected(now, &|m: &BucketManifest| -> Result<Commit> {
                let fresh;
                let r = if m.generation == current.generation { &reclaim } else {
                    fresh = reclaim_of(m)?;
                    &fresh
                };
                Ok(Commit { dropped: drops(m, r), restamped: restamp(m), marks: r.marks.clone() })
            })?
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
        let recorded: BTreeSet<String> = committed
            .collected
            .iter()
            .filter(|(k, c)| c.stands(now) && in_project(k) && !withheld(k) && !committed.entries.contains_key(*k) && deletable(k, commits) && !later.contains(*k))
            .map(|(k, _)| k.clone())
            .collect();
        // A recorded key a retained snapshot names keeps its object, reported: a refusal, not a
        // loss (`store.collect.retained-kept`).
        let (referenced, doomed): (BTreeSet<String>, BTreeSet<String>) = recorded.into_iter().partition(|k| retained(&reclaim, k));
        let pruned: Vec<(u64, u64)> = if shared_prefix { Vec::new() } else { g.pruned.iter().copied().filter(|(n, _)| commits || *n < past).collect() };
        let mut report = CollectReport { dry_run, generation: next, shared_prefix, undeclared: reclaim.undeclared.clone(), referenced: referenced.into_iter().collect(), ..CollectReport::default() };
        for k in &dropped {
            class(&mut report, &reclaim, k).dropped += 1;
        }
        for k in &doomed {
            let size = g.sizes.get(k).or_else(|| current.entries.get(k).map(|e| &e.size)).copied();
            let c = class(&mut report, &reclaim, k);
            c.deleted += 1;
            match size {
                Some(size) => c.bytes += size,
                None => c.unknown_size += 1,
            }
        }
        report.generations.deleted = pruned.len();
        report.generations.bytes = pruned.iter().map(|(_, b)| b).sum();
        if dry_run {
            return Ok(report);
        }
        // The lease still stands before any object goes (`store.collect.serialized`).
        if let Some(held) = lease {
            self.renew_collect(held, now)?;
        }
        let keys: Vec<String> = doomed.into_iter().map(|k| self.key(&k)).collect::<Result<_>>()?;
        each(self.config.transfers(), &keys, |k| Ok(self.bucket.delete(k)?))?;
        for (n, _) in pruned {
            self.bucket.delete(&self.key(&generation_key(n))?)?;
        }
        Ok(report)
    }

    /// What the bucket's table chains under `m` let a collect reclaim at `now`, each table
    /// under its declared `retain_runs` window.
    fn reclaim(&self, m: &BucketManifest, now: Instant, decls: &[TableDecl]) -> Result<Reclaim> {
        let project = format!("{}/", self.project);
        let tables: BTreeSet<String> = m.entries.keys().filter_map(|k| project_table(&self.project, k)).collect();
        let mut out = Reclaim::default();
        let mut chains: BTreeMap<String, Chain> = BTreeMap::new();
        for table in tables {
            let Some(decl) = decls.iter().find(|d| d.name == table) else {
                out.undeclared.push(table);
                continue;
            };
            let window = decl.retain_runs_secs().map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?;
            let Some(mut chain) = self.bucket_chain(m, &table, now.minus_secs(window))? else { continue };
            chain.held = self.bucket_holds(m, &format!("{project}tables/{table}"), now)?;
            for pair in chain.snapshots.windows(2) {
                if pair[0].created_at <= chain.cutoff && !chain.held.contains(&pair[1].snapshot_id.to_string()) {
                    chain.folded_out.extend(pair[0].includes_runs.iter().cloned());
                }
            }
            chains.insert(table, chain);
        }
        let mut markers: Vec<(String, String, String)> = Vec::new();
        for chain in chains.values() {
            let table_key = format!("{}tables/{}", project, chain.table);
            let held = &chain.held;
            // What the retained snapshots name stays: the current one, each superseded inside the
            // window, and each held (`store.fold.reference-retention`).
            let mut retained: Vec<SnapshotManifest> = chain.snapshots.first().cloned().into_iter().collect();
            let mut collected: Vec<&SnapshotManifest> = Vec::new();
            for pair in chain.snapshots.windows(2) {
                if pair[0].created_at > chain.cutoff || held.contains(&pair[1].snapshot_id.to_string()) {
                    retained.push(pair[1].clone());
                } else {
                    collected.push(&pair[1]);
                }
            }
            for id in held.iter().filter(|id| !chain.snapshots.iter().any(|s| s.snapshot_id.to_string() == **id)) {
                if let Some(s) = self.bucket_snapshot(m, &table_key, id)? {
                    retained.push(s);
                }
            }
            let referenced = referenced(&table_key, &retained);
            out.retained.extend(referenced.iter().cloned());
            for s in collected {
                let dir = format!("{table_key}/{SNAPSHOTS_DIR}/{}/", s.snapshot_id);
                for k in m.entries.range(dir.clone()..).map(|(k, _)| k).take_while(|k| k.starts_with(&dir)) {
                    let name = &k[dir.len()..];
                    if name == MANIFEST_FILE || name == COLLECTED_FILE || referenced.iter().any(|r| k == r || k.starts_with(&format!("{r}/"))) {
                        continue;
                    }
                    out.superseded.insert(k.clone());
                }
                let mark = format!("{dir}{COLLECTED_FILE}");
                if !m.entries.contains_key(&mark) {
                    let body = serde_json::json!({ "collected_at": now.to_rfc3339_nanos() }).to_string();
                    out.marks.insert(mark.clone(), self.store.seal_metadata_bytes(Path::new(&mark), body.as_bytes())?);
                }
                out.snapshot_dirs.insert(dir);
            }
            for run in &chain.folded_out {
                let dir = format!("{table_key}/{RUNS_DIR}/{run}/");
                for k in m.entries.range(dir.clone()..).map(|(k, _)| k).take_while(|k| k.starts_with(&dir)) {
                    if is_group_marker(k) {
                        markers.push((chain.table.clone(), run.clone(), k.clone()));
                    } else {
                        out.folded.insert(k.clone());
                    }
                }
                out.run_dirs.insert(dir);
            }
        }
        // A run's `_group.json` goes once every member table holding a copy of the run folds it out
        // as the root table does (`store.collect.group-window`).
        for (table, run, key) in markers {
            let Some((bytes, _)) = self.bucket.get(&self.key(&key)?)? else { continue };
            let members: Vec<String> = serde_json::from_slice(&bytes).map_err(|e| StoreError::StoreManifestUnreadable(format!("`{key}`: group marker: {e}")))?;
            let needed = members.iter().filter(|member| **member != table).any(|member| {
                let copy = format!("{project}tables/{member}/{RUNS_DIR}/{run}/{MANIFEST_FILE}");
                m.entries.contains_key(&copy) && chains.get(member).is_none_or(|c| !c.folded_out.contains(&run))
            });
            if !needed {
                out.folded.insert(key);
            }
        }
        Ok(out)
    }

    /// Each of `keys`, a run file `remote` does not list, whose run a bucket chain snapshot folds
    /// while superseding a snapshot whose `_collected.json` `remote` lists: a collect reclaimed
    /// it, and no merge lists it again (`store.merge.collected-directory`). Only a table holding
    /// such a key has its chain read.
    pub(crate) fn folded_out_keys<'k>(&self, remote: &BucketManifest, keys: impl IntoIterator<Item = &'k String>) -> Result<BTreeSet<String>> {
        let candidates: Vec<&String> = keys.into_iter().filter(|k| k.contains(&format!("/{RUNS_DIR}/")) && !remote.entries.contains_key(*k)).collect();
        let mut out = BTreeSet::new();
        let tables: BTreeSet<String> = candidates.iter().filter_map(|k| project_table(&self.project, k)).collect();
        for table in tables {
            let Some(chain) = self.bucket_chain(remote, &table, Instant::from_unix_nanos(0).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?)? else { continue };
            let table_key = format!("{}/tables/{table}", self.project);
            let mut dirs: Vec<String> = Vec::new();
            for pair in chain.snapshots.windows(2) {
                if remote.entries.contains_key(&format!("{table_key}/{SNAPSHOTS_DIR}/{}/{COLLECTED_FILE}", pair[1].snapshot_id)) {
                    dirs.extend(pair[0].includes_runs.iter().map(|run| format!("{table_key}/{RUNS_DIR}/{run}/")));
                }
            }
            out.extend(candidates.iter().filter(|k| dirs.iter().any(|d| k.starts_with(d.as_str()))).map(|k| (*k).clone()));
        }
        Ok(out)
    }

    /// The chain of `table`'s bucket pointer, read through the manifests the bucket lists, as
    /// far as they reach; `None` for a table without a pointer.
    fn bucket_chain(&self, m: &BucketManifest, table: &str, cutoff: Instant) -> Result<Option<Chain>> {
        let table_key = format!("{}/tables/{table}", self.project);
        let pointer_key = format!("{table_key}/{POINTER_FILE}");
        let Some((bytes, _)) = self.bucket.get(&self.key(&pointer_key)?)? else { return Ok(None) };
        let clear = self.store.open_metadata_bytes(Path::new(&pointer_key), &bytes)?;
        let pointer: BucketPointer = serde_json::from_slice(&clear).map_err(|e| SyncError::Context(ContextError::Invalid(format!("`{pointer_key}`: {e}"))))?;
        let mut snapshots = Vec::new();
        let mut seen = BTreeSet::new();
        let mut at = pointer.snapshot_id;
        while let Some(id) = at.take() {
            if !seen.insert(id.clone()) {
                break;
            }
            let Some(s) = self.bucket_snapshot(m, &table_key, &id)? else { break };
            at = s.parent.as_ref().map(ToString::to_string);
            snapshots.push(s);
        }
        Ok(Some(Chain { table: table.to_string(), cutoff, snapshots, folded_out: BTreeSet::new(), held: BTreeSet::new() }))
    }

    /// Snapshot `id`'s manifest under `table_key`, when the bucket manifest lists it.
    fn bucket_snapshot(&self, m: &BucketManifest, table_key: &str, id: &str) -> Result<Option<SnapshotManifest>> {
        let key = format!("{table_key}/{SNAPSHOTS_DIR}/{id}/{MANIFEST_FILE}");
        if !m.entries.contains_key(&key) {
            return Ok(None);
        }
        let Some((bytes, _)) = self.bucket.get(&self.key(&key)?)? else { return Ok(None) };
        let clear = self.store.open_metadata_bytes(Path::new(&key), &bytes)?;
        let s = serde_json::from_slice(&clear).map_err(|e| StoreError::StoreManifestUnreadable(format!("`{key}`: {e}")))?;
        Ok(Some(s))
    }

    /// The build ids an unexpired hold under `table_key` names (`run.publish.hold`).
    fn bucket_holds(&self, m: &BucketManifest, table_key: &str, now: Instant) -> Result<BTreeSet<String>> {
        let dir = format!("{table_key}/holds/");
        let mut out = BTreeSet::new();
        for key in m.entries.range(dir.clone()..).map(|(k, _)| k).take_while(|k| k.starts_with(&dir)).filter(|k| k.ends_with(".json")) {
            let Some((bytes, _)) = self.bucket.get(&self.key(key)?)? else { continue };
            let clear = self.store.open_metadata_bytes(Path::new(key), &bytes)?;
            let hold: HoldRecord = serde_json::from_slice(&clear).map_err(|e| StoreError::StoreManifestUnreadable(format!("`{key}`: {e}")))?;
            if hold.active(now) {
                out.insert(hold.build_id);
            }
        }
        Ok(out)
    }

    /// Whether the bucket records a collection of build `build_id` of `model`: its directory's
    /// `_collected.json` listed, or a standing collected record naming a key under it
    /// (`run.model.hold-collected`).
    pub fn build_collected(&self, model: &str, build_id: &str, now: Instant) -> Result<bool> {
        let (m, _) = self.manifest()?;
        let dir = format!("{}/tables/{model}/{SNAPSHOTS_DIR}/{build_id}/", self.project);
        Ok(m.entries.contains_key(&format!("{dir}{COLLECTED_FILE}"))
            || m.collected.range(dir.clone()..).take_while(|(k, _)| k.starts_with(&dir)).any(|(_, c)| c.stands(now)))
    }

    /// The bucket's copy of the hold on build `build_id` of `model`, when the manifest lists one.
    fn bucket_hold(&self, m: &BucketManifest, model: &str, build_id: &str) -> Result<Option<HoldRecord>> {
        let key = format!("{}/tables/{model}/holds/{build_id}.json", self.project);
        if !m.entries.contains_key(&key) {
            return Ok(None);
        }
        let Some((bytes, _)) = self.bucket.get(&self.key(&key)?)? else { return Ok(None) };
        let clear = self.store.open_metadata_bytes(Path::new(&key), &bytes)?;
        Ok(Some(serde_json::from_slice(&clear).map_err(|e| StoreError::StoreManifestUnreadable(format!("`{key}`: {e}")))?))
    }

    /// Whether the bucket holds an unexpired hold on build `build_id` of `model` at `now`.
    pub fn bucket_hold_active(&self, model: &str, build_id: &str, now: Instant) -> Result<bool> {
        let (m, _) = self.manifest()?;
        Ok(self.bucket_hold(&m, model, build_id)?.is_some_and(|h| h.active(now)))
    }

    /// Write this store's hold on build `build_id` of `model` to the bucket, merged into the
    /// bucket's copy, and commit a manifest listing it (`run.model.hold-bucket-first`).
    pub fn push_hold(&self, model: &str, build_id: &str, now: Instant) -> Result<()> {
        let key = format!("{}/tables/{model}/holds/{build_id}.json", self.project);
        let path = self.store.root().join(&key[self.project.len() + 1..]);
        let local = std::fs::read(&path).map_err(|e| SyncError::Context(ContextError::Io { path: path.clone(), source: e }))?;
        self.put_merged(&key, &local)?;
        let Some((bytes, _)) = self.bucket.get(&self.key(&key)?)? else {
            return Err(StoreError::SyncPullDidNotConverge(format!("`{key}` left the bucket while the hold committed")).into());
        };
        let entry = Entry { sha256: sha256_hex(&bytes), size: bytes.len() as u64, owner: String::new() };
        for _ in 0..PUSH_ROUND_CEILING {
            let (remote, etag, floor) = self.read_for_commit()?;
            if remote.entries.get(&key) == Some(&entry) {
                return Ok(());
            }
            let mut committed = remote.clone();
            committed.entries.insert(key.clone(), entry.clone());
            committed.generation = floor + 1;
            committed.committed_at = Some(now);
            committed.committed_by = Some(self.node.clone());
            if self.put_commit(&committed, etag)? {
                return Ok(());
            }
        }
        Err(StoreError::SyncManifestRebaseExhausted(format!("the hold `{key}` lost {PUSH_ROUND_CEILING} races; run the hold again")).into())
    }

    /// Commit the bucket manifest with what `commit` names for the manifest as read: each
    /// dropped key recorded as collected, each pending record stamped again and each new
    /// `_collected.json` listed, uploaded first; no commit when it names nothing. Returns the
    /// manifest that stands and the keys it recorded.
    fn commit_collected(&self, now: Instant, commit: &dyn Fn(&BucketManifest) -> Result<Commit>) -> Result<(BucketManifest, BTreeSet<String>)> {
        let mut uploaded: BTreeMap<String, Entry> = BTreeMap::new();
        for _ in 0..PUSH_ROUND_CEILING {
            let (remote, etag, floor) = self.read_for_commit()?;
            let c = commit(&remote)?;
            if c.dropped.is_empty() && c.restamped.is_empty() && c.marks.is_empty() {
                return Ok((remote, c.dropped));
            }
            // A marker reaches the bucket before a manifest lists it (`store.push.transfer-width`).
            for (key, bytes) in &c.marks {
                if uploaded.contains_key(key) {
                    continue;
                }
                // A concurrent collect's marker stands: its listed digest names those bytes
                // (`store.collect.manifest-kept`).
                let held = match self.bucket.put(&self.key(key)?, bytes, Condition::IfNoneMatch)? {
                    Put::Applied(_) => bytes.clone(),
                    Put::ConditionFailed => match self.bucket.get(&self.key(key)?)? {
                        Some((existing, _)) => existing,
                        None => continue,
                    },
                };
                uploaded.insert(key.clone(), Entry { sha256: sha256_hex(&held), size: held.len() as u64, owner: String::new() });
            }
            let marks: BTreeMap<String, Entry> = c.marks.keys().filter_map(|k| uploaded.get(k).map(|e| (k.clone(), e.clone()))).collect();
            let committed = with_collected(&remote, &c.dropped, &c.restamped, &marks, &self.node, now, floor + 1);
            if self.put_commit(&committed, etag)? {
                return Ok((committed, c.dropped));
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

/// The class a key a collect records or deletes reports under (`store.collect.command`).
fn class<'r>(report: &'r mut CollectReport, reclaim: &Reclaim, k: &str) -> &'r mut CollectClass {
    if reclaim.snapshot_dirs.iter().any(|d| k.starts_with(d.as_str())) {
        &mut report.superseded
    } else if reclaim.run_dirs.iter().any(|d| k.starts_with(d.as_str())) {
        &mut report.folded_runs
    } else {
        &mut report.unlisted
    }
}

/// What one manifest commit of a collect writes.
struct Commit {
    dropped: BTreeSet<String>,
    restamped: BTreeSet<String>,
    marks: BTreeMap<String, Vec<u8>>,
}

/// Each key under `table_key` the snapshots in `retained` name: every part, a part carried by
/// reference at its origin, and every sidecar segment directory (`store.fold.reference-retention`).
fn referenced(table_key: &str, retained: &[SnapshotManifest]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for s in retained {
        out.extend(s.part_files().into_iter().map(|p| format!("{table_key}/{p}")));
        for index in &s.indexes {
            let Some(path) = index.path() else { continue };
            for segment in index.segments() {
                out.insert(format!("{table_key}/{SNAPSHOTS_DIR}/{}/{path}", segment.snapshot_id));
            }
        }
    }
    out
}

/// Whether `key` is a grouped run's marker, which a member run needs to read as committed
/// (`store.collect.unlisted`).
fn is_group_marker(key: &str) -> bool {
    key.ends_with("/_group.json")
}

/// `m` with every key of `dropped` recorded as collected by `node` at `now`, each of `restamped`
/// stamped again, each of `marks` listed, and records past their age left out, committed as
/// generation `next`.
fn with_collected(m: &BucketManifest, dropped: &BTreeSet<String>, restamped: &BTreeSet<String>, marks: &BTreeMap<String, Entry>, node: &str, now: Instant, next: u64) -> BucketManifest {
    let mut out = m.clone();
    out.collected.retain(|_, c| c.stands(now));
    for key in restamped {
        if let Some(c) = out.collected.get_mut(key) {
            c.collected_at = now;
        }
    }
    for key in dropped {
        out.entries.remove(key);
        out.collected.insert(key.clone(), Collected { collected_at: now, collected_by: node.to_string() });
    }
    for (key, entry) in marks {
        out.entries.insert(key.clone(), entry.clone());
    }
    out.generation = next;
    out.committed_at = Some(now);
    out.committed_by = Some(node.to_string());
    out
}
