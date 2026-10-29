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
use contextful_core::store::lay_out::{FoldedRow, SnapshotId, SnapshotManifest, COLLECTED_FILE, MANIFEST_FILE, POINTER_FILE, RUNS_DIR, SNAPSHOTS_DIR};
use contextful_core::store::lease::{BucketLease, BucketPointer, CLOCK_SKEW_SECS, RENEWAL_SECS};
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
    /// Where the walk stopped at a reclaimed parent under a valid index: the directory of
    /// each collected snapshot past that parent and the parent's own (`store.collect.index-walk`).
    husks: BTreeSet<String>,
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
        let released = self.release_collect(&held);
        let report = done?;
        released?;
        Ok(report)
    }

    /// Acquire the bucket's collect lease at `now` (`store.collect.serialized`), on every bucket:
    /// a filesystem's conditional writes hold across the processes of one machine.
    pub fn acquire_collect(&self, now: Instant) -> Result<Option<Held>> {
        let key = self.key(&collect_lease_key(&self.project))?;
        loop {
            let current = self.bucket.get(&key)?;
            let parsed: Option<(BucketLease, String)> = match &current {
                Some((b, etag)) => Some((serde_json::from_slice(b).map_err(|e| SyncError::Context(ContextError::Invalid(format!("lease `{key}`: {e}"))))?, etag.clone())),
                None => None,
            };
            let (lease, condition) = BucketLease::acquire(parsed.as_ref().map(|(l, e)| (l, e.as_str())), &self.node, now)?;
            let bytes = serde_json::to_vec_pretty(&lease).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?;
            let put = self.bucket.put(&key, &bytes, condition).map_err(|e| {
                let scope = match self.bucket.cas_scope() {
                    CasScope::Machine(why) => format!("; the bucket's conditional writes hold on one machine only: {why}"),
                    _ => String::new(),
                };
                StoreError::SyncCoordinationUnproven(format!("the collect lease `{key}` could not be taken: {e}{scope}"))
            })?;
            if let Put::Applied(etag) = put {
                return Ok(Some(Held { key, lease, etag }));
            }
        }
    }

    /// Release the bucket's collect lease `held`, the acquisition this node made.
    pub fn release_collect(&self, held: &Held) -> Result<()> {
        let key = self.key(&collect_lease_key(&self.project))?;
        loop {
            let Some((b, etag)) = self.bucket.get(&key)? else {
                return Err(StoreError::LeaseNotHeld(format!("no lease `{key}` exists")).into());
            };
            let current: BucketLease = serde_json::from_slice(&b).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?;
            let (released, condition) = BucketLease::release(&current, &etag, &self.node, Some(held.lease.fence))?;
            let bytes = serde_json::to_vec_pretty(&released).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?;
            if let Put::Applied(_) = self.bucket.put(&key, &bytes, condition)? {
                return Ok(());
            }
        }
    }

    /// Renew the collect lease `held` at `now`; a lease another collect fenced past refuses.
    fn renew_collect(&self, held: &Held, now: Instant) -> Result<Held> {
        loop {
            let Some((b, etag)) = self.bucket.get(&held.key)? else {
                return Err(StoreError::LeaseNotHeld(format!("no lease `{}` exists", held.key)).into());
            };
            let current: BucketLease = serde_json::from_slice(&b).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?;
            let (lease, condition) = BucketLease::renew(&current, &etag, &held.lease, now)?;
            let bytes = serde_json::to_vec_pretty(&lease).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?;
            if let Put::Applied(etag) = self.bucket.put(&held.key, &bytes, condition)? {
                return Ok(Held { key: held.key.clone(), lease, etag });
            }
        }
    }

    fn collect_under(&self, now: Instant, dry_run: bool, decls: &[TableDecl], lease: Option<&Held>) -> Result<CollectReport> {
        let started = std::time::Instant::now();
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
        // The keys a commit over `m` records: an unlisted object a tombstone or a generation past
        // the count names (`store.collect.unlisted`), a listed object the chains reclaim
        // (`store.collect.superseded`, `store.collect.folded-runs`), never a key a retained snapshot
        // names nor a metadata key. A retained generation listing a key delays its deletion, never
        // its record (`store.collect.two-phase`).
        let drops = |m: &BucketManifest, r: &Reclaim| -> BTreeSet<String> {
            if shared_prefix {
                return BTreeSet::new();
            }
            let standing = |k: &str| m.collected.get(k).is_some_and(|c| c.stands(now));
            let unlisted = m.tombstones.keys().filter(|k| keeps(k)).chain(g.evidence.iter().filter(|k| keeps(k))).filter(|k| !m.entries.contains_key(*k));
            let reclaimed = r.superseded.iter().chain(&r.folded).filter(|k| m.entries.contains_key(*k) && !withheld(k));
            unlisted.chain(reclaimed).filter(|k| !standing(k) && !retained(r, k) && !is_metadata(k) && listing.contains(*k)).cloned().collect()
        };
        // A record stands while its object stays: each past half its age is stamped again
        // (`store.collect.record-stands`).
        let restamp = |m: &BucketManifest| -> BTreeSet<String> {
            m.collected.iter().filter(|(k, c)| c.stands(now) && listing.contains(*k) && c.collected_at.secs_until(now) >= TOMBSTONE_TTL_SECS / 2).map(|(k, _)| k.clone()).collect()
        };

        // The reclaim of the round whose manifest the collect committed over, when it is not `current`.
        let round: std::cell::RefCell<Option<Reclaim>> = std::cell::RefCell::new(None);
        let (committed, dropped) = if dry_run {
            let dropped = drops(&current, &reclaim);
            let restamped = restamp(&current);
            let changes = !dropped.is_empty() || !reclaim.marks.is_empty() || !restamped.is_empty();
            let next = if changes { floor + 1 } else { floor };
            (with_collected(&current, &dropped, &restamped, &BTreeMap::new(), &self.node, now, next), dropped)
        } else {
            self.commit_collected(now, &|m: &BucketManifest| -> Result<Commit> {
                if m.generation == current.generation {
                    *round.borrow_mut() = None;
                    return Ok(Commit { dropped: drops(m, &reclaim), restamped: restamp(m), marks: reclaim.marks.clone() });
                }
                let fresh = reclaim_of(m)?;
                let c = Commit { dropped: drops(m, &fresh), restamped: restamp(m), marks: fresh.marks.clone() };
                *round.borrow_mut() = Some(fresh);
                Ok(c)
            })?
        };
        let fresh = round.into_inner();
        let reclaim = fresh.as_ref().unwrap_or(&reclaim);
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
        let (referenced, doomed): (BTreeSet<String>, BTreeSet<String>) = recorded.into_iter().partition(|k| retained(reclaim, k));
        let pruned: Vec<(u64, u64)> = if shared_prefix { Vec::new() } else { g.pruned.iter().copied().filter(|(n, _)| commits || *n < past).collect() };
        let mut report = CollectReport { dry_run, generation: next, shared_prefix, undeclared: reclaim.undeclared.clone(), referenced: referenced.into_iter().collect(), ..CollectReport::default() };
        for k in &dropped {
            class(&mut report, reclaim, k).dropped += 1;
        }
        for k in &doomed {
            let size = g.sizes.get(k).or_else(|| current.entries.get(k).map(|e| &e.size)).copied();
            let c = class(&mut report, reclaim, k);
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
        // The lease stands before any object goes and through the deletes, renewed by the clock
        // reading of each moment; a lease another collect took stops them (`store.collect.serialized`).
        let fresh_now = || -> Result<Instant> {
            let elapsed = i128::try_from(started.elapsed().as_nanos()).unwrap_or(i128::MAX);
            Instant::from_unix_nanos(now.unix_nanos().saturating_add(elapsed)).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))
        };
        let mut held = lease.cloned();
        if let Some(h) = &held {
            held = Some(self.renew_collect(h, fresh_now()?)?);
        }
        let keys: Vec<String> = doomed.into_iter().map(|k| self.key(&k)).collect::<Result<_>>()?;
        let mut renewed = std::time::Instant::now();
        for batch in keys.chunks(self.config.transfers().max(1) * 16) {
            if let Some(h) = &held {
                if renewed.elapsed() >= std::time::Duration::from_secs(RENEWAL_SECS) {
                    held = Some(self.renew_collect(h, fresh_now()?)?);
                    renewed = std::time::Instant::now();
                }
            }
            each(self.config.transfers(), batch, |k| Ok(self.bucket.delete(k)?))?;
        }
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
            let dirs = collected.iter().map(|s| format!("{table_key}/{SNAPSHOTS_DIR}/{}/", s.snapshot_id)).chain(chain.husks.iter().cloned());
            for dir in dirs {
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
            let table_key = format!("{}/tables/{table}", self.project);
            let runs_prefix = format!("{table_key}/{RUNS_DIR}/");
            // The walk stops at the first snapshot created before the oldest candidate run
            // committed: no snapshot older than a run folds it.
            // A candidate whose manifest does not read walks the whole chain.
            let committed: Vec<Option<Instant>> = candidates
                .iter()
                .filter_map(|k| k.strip_prefix(&runs_prefix))
                .map(|rest| {
                    let mut segs = rest.splitn(3, '/');
                    let (run, node) = (segs.next()?, segs.next()?);
                    let rel = format!("tables/{table}/{RUNS_DIR}/{run}/{node}/{MANIFEST_FILE}");
                    let path = self.store.root().join(&rel);
                    let bytes = std::fs::read(&path).ok()?;
                    let clear = self.store.open_metadata_bytes(&path, &bytes).ok()?;
                    serde_json::from_slice::<contextful_core::store::lay_out::RunManifest>(&clear).ok().map(|m| m.committed_at)
                })
                .collect();
            let oldest: Option<Instant> = if committed.iter().any(Option::is_none) { None } else { committed.into_iter().flatten().min() };
            let pointer_key = format!("{table_key}/{POINTER_FILE}");
            let Some((bytes, _)) = self.bucket.get(&self.key(&pointer_key)?)? else { continue };
            let clear = self.store.open_metadata_bytes(Path::new(&pointer_key), &bytes)?;
            let pointer: BucketPointer = serde_json::from_slice(&clear).map_err(|e| SyncError::Context(ContextError::Invalid(format!("`{pointer_key}`: {e}"))))?;
            // Under the pointer's valid index each candidate's row names the snapshot folding its
            // run, and that snapshot's manifest its parent: no walk (`store.collect.index-walk`).
            let head = match &pointer.snapshot_id {
                Some(id) => self.bucket_snapshot(remote, &table_key, id)?,
                None => None,
            };
            if let Some(rows) = head.as_ref().map(|h| self.bucket_index(remote, &table_key, h)).transpose()?.flatten() {
                let mut reclaimed: BTreeMap<SnapshotId, bool> = BTreeMap::new();
                for k in candidates.iter().filter(|k| k.starts_with(&runs_prefix)) {
                    let mut segs = k[runs_prefix.len()..].splitn(3, '/');
                    let (Some(run), Some(node)) = (segs.next(), segs.next()) else { continue };
                    let Some(row) = rows.get(&format!("{run}/{node}")) else { continue };
                    if !reclaimed.contains_key(&row.snapshot_id) {
                        let folding = match head.as_ref().filter(|h| h.snapshot_id == row.snapshot_id) {
                            Some(h) => Some(h.clone()),
                            None => self.bucket_snapshot(remote, &table_key, &row.snapshot_id.to_string())?,
                        };
                        let husk = folding.and_then(|c| c.parent).is_some_and(|p| remote.entries.contains_key(&format!("{table_key}/{SNAPSHOTS_DIR}/{p}/{COLLECTED_FILE}")));
                        reclaimed.insert(row.snapshot_id.clone(), husk);
                    }
                    if reclaimed[&row.snapshot_id] {
                        out.insert((*k).clone());
                    }
                }
                continue;
            }
            let mut dirs: Vec<String> = Vec::new();
            let (mut child, mut seen): (Option<SnapshotManifest>, BTreeSet<String>) = (None, BTreeSet::new());
            let mut at = pointer.snapshot_id;
            while let Some(id) = at.take() {
                if !seen.insert(id.clone()) {
                    break;
                }
                let Some(s) = self.bucket_snapshot(remote, &table_key, &id)? else { break };
                if let Some(c) = &child {
                    if remote.entries.contains_key(&format!("{table_key}/{SNAPSHOTS_DIR}/{id}/{COLLECTED_FILE}")) {
                        dirs.extend(c.includes_runs.iter().map(|run| format!("{runs_prefix}{run}/")));
                    }
                }
                // No snapshot created a clock skew before the oldest candidate committed folds it.
                if oldest.is_some_and(|o| s.created_at.plus_secs(CLOCK_SKEW_SECS) < o) {
                    break;
                }
                at = s.parent.as_ref().map(ToString::to_string);
                child = Some(s);
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
        let mut snapshots: Vec<SnapshotManifest> = Vec::new();
        let mut seen = BTreeSet::new();
        let mut at = pointer.snapshot_id;
        // A table holding a hold record walks whole: a hold can keep a snapshot past any parent.
        let holds = format!("{table_key}/holds/");
        let walks = m.entries.range(holds.clone()..).next().is_some_and(|(k, _)| k.starts_with(&holds));
        let mut index: Option<BTreeMap<String, FoldedRow>> = None;
        let (mut folded_out, mut husks) = (BTreeSet::new(), BTreeSet::new());
        let husk = |id: &str| m.entries.contains_key(&format!("{table_key}/{SNAPSHOTS_DIR}/{id}/{COLLECTED_FILE}"));
        while let Some(id) = at.take() {
            if !seen.insert(id.clone()) {
                break;
            }
            let Some(s) = self.bucket_snapshot(m, &table_key, &id)? else { break };
            if snapshots.is_empty() && !walks {
                index = self.bucket_index(m, &table_key, &s)?;
            }
            at = s.parent.as_ref().map(ToString::to_string);
            // A child created at or before the cutoff, by its clock and by its id, whose parent a
            // collect already reclaimed: every snapshot past it is reclaimed, and the index names
            // the runs they fold (`store.collect.index-walk`).
            let cut = u128::try_from(cutoff.unix_nanos()).unwrap_or(0);
            let stops = index.is_some() && s.created_at <= cutoff && s.snapshot_id.nanos() <= cut && at.as_deref().is_some_and(husk);
            if let (true, Some(rows)) = (stops, &index) {
                // The oldest folding snapshot reads as the root, whose runs no collect reclaims.
                let root = rows.values().map(|r| &r.snapshot_id).min();
                folded_out.extend(s.includes_runs.iter().cloned());
                folded_out.extend(rows.values().filter(|r| r.snapshot_id < s.snapshot_id && Some(&r.snapshot_id) != root).map(|r| r.run_key.clone()));
                let prefix = format!("{table_key}/{SNAPSHOTS_DIR}/");
                // The chain past the stop: its parent, and each snapshot an index row names as folding.
                let chain: BTreeSet<String> = rows.values().map(|r| r.snapshot_id.to_string()).chain(at.clone()).collect();
                let walked: BTreeSet<String> = snapshots.iter().chain(std::iter::once(&s)).map(|w| w.snapshot_id.to_string()).collect();
                husks = chain.iter().filter(|id| !walked.contains(*id) && husk(id)).map(|id| format!("{prefix}{id}/")).collect();
                snapshots.push(s);
                break;
            }
            snapshots.push(s);
        }
        Ok(Some(Chain { table: table.to_string(), cutoff, snapshots, folded_out, held: BTreeSet::new(), husks }))
    }

    /// The rows of the folded-runs index snapshot `head` carries, its segments read at the
    /// digests the bucket manifest `m` lists: the local copy where it matches, else the
    /// bucket's. `None` where the index does not read valid, and the caller walks
    /// (`store.collect.index-walk`).
    fn bucket_index(&self, m: &BucketManifest, table_key: &str, head: &SnapshotManifest) -> Result<Option<BTreeMap<String, FoldedRow>>> {
        if head.folded_runs.is_none() {
            return Ok(None);
        }
        let scratch = crate::sync::sync_dir(self.store.root(), &self.project).join("index.staging");
        let bytes_of = |location: &str| -> Option<Vec<u8>> {
            let key = format!("{table_key}/{location}");
            let listed = m.entries.get(&key)?;
            let local = self.store.root().join(&key[self.project.len() + 1..]);
            if let Ok(bytes) = std::fs::read(&local) {
                if sha256_hex(&bytes) == listed.sha256 {
                    return Some(bytes);
                }
            }
            let (bytes, _) = self.bucket.get(&self.key(&key).ok()?).ok()??;
            (sha256_hex(&bytes) == listed.sha256).then_some(bytes)
        };
        Ok(contextful_context::fold::folded_index_from(&self.store, head, &bytes_of, &scratch))
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
    /// bucket's copy, and commit a manifest listing it (`run.model.hold-bucket-first`). `false`
    /// where a round's manifest records a collect of the build, which commits nothing
    /// (`run.model.hold-collected`).
    pub fn push_hold(&self, model: &str, build_id: &str, now: Instant) -> Result<bool> {
        let key = format!("{}/tables/{model}/holds/{build_id}.json", self.project);
        let dir = format!("{}/tables/{model}/{SNAPSHOTS_DIR}/{build_id}/", self.project);
        let collected = |m: &BucketManifest| {
            m.entries.contains_key(&format!("{dir}{COLLECTED_FILE}")) || m.collected.range(dir.clone()..).take_while(|(k, _)| k.starts_with(&dir)).any(|(_, c)| c.stands(now))
        };
        // A build the bucket already records as collected refuses before any object is written.
        if collected(&self.manifest()?.0) {
            return Ok(false);
        }
        let path = self.store.root().join(&key[self.project.len() + 1..]);
        let local = std::fs::read(&path).map_err(|e| SyncError::Context(ContextError::Io { path: path.clone(), source: e }))?;
        self.put_merged(&key, &local)?;
        let Some((bytes, _)) = self.bucket.get(&self.key(&key)?)? else {
            return Err(StoreError::SyncPullDidNotConverge(format!("`{key}` left the bucket while the hold committed")).into());
        };
        let entry = Entry { sha256: sha256_hex(&bytes), size: bytes.len() as u64, owner: String::new() };
        for _ in 0..PUSH_ROUND_CEILING {
            let (remote, etag, floor) = self.read_for_commit()?;
            let refused = collected(&remote);
            if remote.entries.get(&key) == Some(&entry) {
                return Ok(!refused);
            }
            // A refusal after the write still lists the object at its digest, so no pull meets a
            // listed key whose bytes moved (`run.model.hold-bucket-first`).
            if refused && !remote.entries.contains_key(&key) {
                return Ok(false);
            }
            let mut committed = remote.clone();
            committed.entries.insert(key.clone(), entry.clone());
            committed.generation = floor + 1;
            committed.committed_at = Some(now);
            committed.committed_by = Some(self.node.clone());
            if self.put_commit(&committed, etag)? {
                return Ok(!refused);
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
        // Every folded-runs segment a retained snapshot names stays (`store.fold.folded-index`).
        out.extend(s.folded_runs.iter().flat_map(|f| &f.segments).map(|seg| format!("{table_key}/{}", seg.location(&s.snapshot_id))));
    }
    out
}

/// Whether `key` is metadata no collect records: a snapshot's `_manifest.json` or
/// `_collected.json`, a pointer, a schema, a hold or a run state (`store.collect.manifest-kept`).
fn is_metadata(key: &str) -> bool {
    let name = key.rsplit('/').next().unwrap_or_default();
    (key.contains(&format!("/{SNAPSHOTS_DIR}/")) && (name == MANIFEST_FILE || name == COLLECTED_FILE))
        || name == POINTER_FILE
        || name == "schema.json"
        || key.contains("/holds/")
        || name == "run-state.json"
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
