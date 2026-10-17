//! D1 coordinated-group state: lease-fenced sender rows whose large bodies live in R2.

use crate::{D1Catalog, Statement, D1};
use contextful_core::coordinate::{Catalog, Lease, LeaseKey};
use contextful_core::export::publication::{
    GroupBatch, GroupSender, GroupState, PublicationGroup, PublicationPlan, SourcePublication,
};
use contextful_core::run::{Failure, FailureTag};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// mirrors: run.export.group-d1-spill
pub const GROUP_ROW_BYTES: usize = 1024 * 1024;
/// mirrors: run.export.group-storage
pub const GROUP_STATE_BYTES: usize = 8 * 1024 * 1024;

const LEASE: &str = " AND EXISTS (SELECT 1 FROM contextful_lease WHERE key = ? AND holder = ? AND fence = ? AND expires_us > CAST((julianday('now') - 2440587.5) * 86400000000 AS INTEGER))";
const WRITTEN: &str = "EXISTS (SELECT 1 FROM contextful_publication WHERE role = ? AND name = ? AND namespace = ? AND token = ?)";

pub struct D1PublicationLedger {
    db: D1,
    catalog: D1Catalog,
    leases: BTreeMap<String, (Lease, u64)>,
    offered: BTreeMap<(String, String), (String, u64)>,
}

fn failure(error: impl std::fmt::Display) -> Failure {
    Failure::new(FailureTag::Storage, format!("D1 publication: {error}"))
}

fn fenced() -> Failure {
    Failure::deterministic(
        FailureTag::Permanent,
        "ExecutionWriterFenced: publication writer no longer owns its durable lease",
    )
}

struct Row<T> {
    version: Option<i64>,
    value: Option<T>,
}

/// A row write chained after the sender update by its write token.
enum Follow<'a> {
    ActiveNamespace(&'a str),
    History(&'a str, String),
}

impl D1PublicationLedger {
    pub fn connect(endpoint: &str) -> Result<Self, Failure> {
        let db = D1::connect(endpoint).map_err(failure)?;
        db.health().map_err(failure)?;
        Ok(Self {
            db,
            catalog: D1Catalog::connect(endpoint)?,
            leases: BTreeMap::new(),
            offered: BTreeMap::new(),
        })
    }

    /// One durable writer per publication across replaced containers.
    pub fn claim(&mut self, name: &str, holder: &str, ttl_secs: u64) -> Result<(), Failure> {
        let lease = self
            .catalog
            .acquire(&LeaseKey::Cadence(format!("publication:{name}")), holder, ttl_secs)?
            .ok_or_else(|| {
                Failure::new(
                    FailureTag::Transient,
                    format!("publication `{name}` has an active durable writer"),
                )
            })?;
        self.leases.insert(name.into(), (lease, ttl_secs));
        Ok(())
    }

    pub fn release(&mut self) -> Result<(), Failure> {
        for (_, (lease, _)) in std::mem::take(&mut self.leases) {
            self.catalog.release(&lease)?;
        }
        Ok(())
    }

    /// Every write renews the publication lease, or refuses once another writer holds it.
    fn lease(&self, name: &str) -> Result<&Lease, Failure> {
        let (lease, ttl_secs) = self.leases.get(name).ok_or_else(fenced)?;
        if self.catalog.renew(lease, *ttl_secs)?.is_none() {
            return Err(fenced());
        }
        Ok(lease)
    }

    fn load<T: DeserializeOwned>(&self, role: &str, name: &str, namespace: &str) -> Result<Row<T>, Failure> {
        let rows = self
            .db
            .query(
                "SELECT version, body, blob FROM contextful_publication WHERE role = ? AND name = ? AND namespace = ?",
                vec![json!(role), json!(name), json!(namespace)],
            )
            .map_err(failure)?
            .rows;
        let Some(row) = rows.first() else {
            return Ok(Row { version: None, value: None });
        };
        let version = row.get("version").and_then(Value::as_i64).ok_or_else(|| failure("missing row version"))?;
        let bytes = match row.get("blob").and_then(Value::as_str) {
            Some(digest) => self
                .db
                .get_blob(digest)
                .map_err(failure)?
                .ok_or_else(|| failure(format!("ExecutionRecoveryMismatch: publication body `{digest}` has no R2 object")))?,
            None => row
                .get("body")
                .and_then(Value::as_str)
                .ok_or_else(|| failure("missing row body"))?
                .as_bytes()
                .to_vec(),
        };
        let value = serde_json::from_slice(&bytes).map_err(failure)?;
        Ok(Row { version: Some(version), value: Some(value) })
    }

    fn sender(&self, name: &str, namespace: &str) -> Result<Row<GroupSender>, Failure> {
        self.load("sender", name, namespace)
    }

    /// Commit one row under its prior version and the writer lease; chained rows commit with it or not at all.
    fn commit(
        &self,
        (role, name, namespace): (&str, &str, &str),
        version: Option<i64>,
        body: &str,
        follow: Option<Follow<'_>>,
    ) -> Result<bool, Failure> {
        let lease = self.lease(name)?;
        if body.len() > GROUP_STATE_BYTES {
            return Err(Failure::deterministic(
                FailureTag::Permanent,
                format!("PublicationStorageUnsupported: `{name}` coordinated-group-outbox state of {} B exceeds the D1 bound of {GROUP_STATE_BYTES} B", body.len()),
            ));
        }
        let token = format!("{:x}", Sha256::digest(format!("{}:{}:{}", lease.holder, lease.fence, uuid_like())));
        let (inline, blob) = if body.len() > GROUP_ROW_BYTES {
            let digest = format!("{:x}", Sha256::digest(body.as_bytes()));
            self.db.put_blob(&digest, body.as_bytes()).map_err(failure)?;
            (String::new(), Some(digest))
        } else {
            (body.to_owned(), None)
        };
        let lease_args = [json!(lease.key), json!(lease.holder), json!(lease.fence)];
        let mut first = match version {
            Some(version) => (
                format!("UPDATE contextful_publication SET version = version + 1, body = ?, blob = ?, token = ? WHERE role = ? AND name = ? AND namespace = ? AND version = ?{LEASE}"),
                vec![json!(inline), json!(blob), json!(token), json!(role), json!(name), json!(namespace), json!(version)],
            ),
            None => (
                format!("INSERT INTO contextful_publication (role, name, namespace, version, body, blob, token) SELECT ?, ?, ?, 0, ?, ?, ? WHERE NOT EXISTS (SELECT 1 FROM contextful_publication WHERE role = ? AND name = ? AND namespace = ?){LEASE}"),
                vec![json!(role), json!(name), json!(namespace), json!(inline), json!(blob), json!(token), json!(role), json!(name), json!(namespace)],
            ),
        };
        first.1.extend(lease_args);
        let written = || vec![json!(role), json!(name), json!(namespace), json!(token)];
        let mut chained: Vec<(String, Vec<Value>)> = vec![];
        if let Some(digest) = &blob {
            let mut args = vec![json!(digest)];
            args.extend(written());
            chained.push((format!("INSERT INTO contextful_blob (sha256, put_at) SELECT ?, CAST(strftime('%s','now') AS INTEGER) WHERE {WRITTEN} ON CONFLICT(sha256) DO UPDATE SET put_at = excluded.put_at"), args));
        }
        match follow {
            Some(Follow::ActiveNamespace(active)) => {
                let mut args = vec![json!(name), json!(serde_json::to_string(active).map_err(failure)?)];
                args.extend(written());
                chained.push((format!("INSERT INTO contextful_publication (role, name, namespace, version, body) SELECT 'active-namespace', ?, '', 0, ? WHERE {WRITTEN} ON CONFLICT(role, name, namespace) DO UPDATE SET version = version + 1, body = excluded.body"), args));
            }
            Some(Follow::History(run, history)) => {
                // The ledger keeps the latest completion alone (`run.export.group-source-latest`).
                let mut prune = vec![json!(name), json!(run)];
                prune.extend(written());
                chained.push((format!("DELETE FROM contextful_publication WHERE role = 'source-history' AND name = ? AND namespace <> ? AND {WRITTEN}"), prune));
                let mut args = vec![json!(name), json!(run), json!(history)];
                args.extend(written());
                chained.push((format!("INSERT OR IGNORE INTO contextful_publication (role, name, namespace, version, body) SELECT 'source-history', ?, ?, 0, ? WHERE {WRITTEN}"), args));
            }
            None => {}
        }
        let statements: Vec<Statement<'_>> = std::iter::once(&first)
            .chain(chained.iter())
            .map(|(sql, params)| Statement { sql, params: params.clone() })
            .collect();
        let results = self.db.batch(&statements).map_err(failure)?;
        Ok(results.first().is_some_and(|r| r.changes == 1))
    }

    /// Re-read, re-apply and commit until the row version holds or the lease moves.
    fn edit<T>(
        &self,
        name: &str,
        namespace: &str,
        mut f: impl FnMut(&mut GroupSender) -> Result<(T, bool, bool), Failure>,
    ) -> Result<T, Failure> {
        for _ in 0..16 {
            let row = self.sender(name, namespace)?;
            let mut sender = row.value.unwrap_or_default();
            let (result, changed, completed) = f(&mut sender)?;
            if !changed {
                self.lease(name)?;
                return Ok(result);
            }
            let body = serde_json::to_string(&sender).map_err(failure)?;
            let follow = completed.then_some(Follow::ActiveNamespace(namespace));
            if self.commit(("sender", name, namespace), row.version, &body, follow)? {
                return Ok(result);
            }
        }
        Err(Failure::new(
            FailureTag::Transient,
            "D1 publication state changed repeatedly or writer lease moved",
        ))
    }

    /// The store incarnation survives exporter checkpoints and container replacement.
    pub fn incarnation(&self, candidate: &str) -> Result<String, Failure> {
        if let Some(incarnation) = self.load::<String>("incarnation", "store", "")?.value {
            return Ok(incarnation);
        }
        if candidate.is_empty() {
            return Err(failure("PublicationIncompatible: empty store incarnation"));
        }
        self.db
            .query(
                "INSERT OR IGNORE INTO contextful_publication (role, name, namespace, version, body) VALUES ('incarnation', 'store', '', 0, ?)",
                vec![json!(serde_json::to_string(candidate).map_err(failure)?)],
            )
            .map_err(failure)?;
        self.load::<String>("incarnation", "store", "")?
            .value
            .ok_or_else(|| failure("missing store incarnation"))
    }

    /// A job completion pins the entire input frontier and every declared output.
    pub fn record_source(&self, name: &str, source: &SourcePublication) -> Result<(), Failure> {
        let body = serde_json::to_string(source).map_err(failure)?;
        for _ in 0..16 {
            let current = self.load::<SourcePublication>("source", name, "")?;
            source.check_history(current.value.as_ref()).map_err(failure)?;
            if current.value.as_ref() == Some(source) {
                return Ok(());
            }
            let follow = Follow::History(&source.run, body.clone());
            if self.commit(("source", name, ""), current.version, &body, Some(follow))? {
                return Ok(());
            }
        }
        Err(Failure::new(FailureTag::Transient, "D1 producer frontier changed repeatedly"))
    }

    pub fn source(&self, name: &str) -> Result<Option<SourcePublication>, Failure> {
        Ok(self.load("source", name, "")?.value)
    }

    pub fn active_namespace(&self, name: &str) -> Result<Option<String>, Failure> {
        Ok(self.load("active-namespace", name, "")?.value)
    }

    pub fn active(&self, name: &str, namespace: &str) -> Result<Option<PublicationGroup>, Failure> {
        Ok(self.sender(name, namespace)?.value.and_then(|s| s.active().cloned()))
    }

    pub fn pending_identity(&self, name: &str, namespace: &str) -> Result<Option<String>, Failure> {
        Ok(self
            .sender(name, namespace)?
            .value
            .and_then(|s| s.pending_identity().map(str::to_owned)))
    }

    pub fn stage(&mut self, plan: &PublicationPlan, states: &GroupState, identity: &str) -> Result<(), Failure> {
        self.edit(&plan.name, &plan.namespace, |sender| {
            let changed = sender.stage(plan, states, identity).map_err(failure)?;
            Ok(((), changed, false))
        })?;
        self.offered.remove(&(plan.name.clone(), plan.namespace.clone()));
        Ok(())
    }

    pub fn batch(&mut self, name: &str, namespace: &str) -> Result<Option<GroupBatch>, Failure> {
        let key = (name.to_owned(), namespace.to_owned());
        self.offered.remove(&key);
        let batch = match self.sender(name, namespace)?.value {
            Some(sender) => sender.batch().map_err(failure)?,
            None => None,
        };
        if let Some(batch) = &batch {
            self.offered.insert(
                key,
                (batch.group.id.clone(), batch.events.last().expect("nonempty batch").sequence),
            );
        }
        Ok(batch)
    }

    /// A completing acknowledgement promotes state and the active namespace in one D1 batch.
    pub fn acknowledge(&mut self, name: &str, namespace: &str, group: &str, through: u64) -> Result<(), Failure> {
        let key = (name.to_owned(), namespace.to_owned());
        if self.offered.get(&key) != Some(&(group.to_owned(), through)) {
            return Err(failure("PublicationIncompatible: acknowledgement differs from the offered batch"));
        }
        self.edit(name, namespace, |sender| {
            let completed = sender.acknowledge(group, through).map_err(failure)?;
            Ok(((), true, completed))
        })?;
        self.offered.remove(&key);
        Ok(())
    }

    pub fn checkpoint(&self, name: &str, namespace: &str) -> Result<Vec<u8>, Failure> {
        self.sender(name, namespace)?
            .value
            .ok_or_else(|| failure("missing publication checkpoint"))?
            .checkpoint(name, namespace)
            .map_err(failure)
    }

    pub fn restore(
        &mut self,
        name: &str,
        namespace: &str,
        bytes: &[u8],
        incarnation: &str,
        identity: &str,
    ) -> Result<(), Failure> {
        self.edit(name, namespace, |sender| {
            let current = (*sender != GroupSender::default()).then_some(&*sender);
            *sender = GroupSender::restore(bytes, name, namespace, incarnation, identity, current)
                .map_err(failure)?;
            Ok(((), true, false))
        })?;
        self.offered.remove(&(name.to_owned(), namespace.to_owned()));
        Ok(())
    }
}

/// A write token unique to this attempt; it only distinguishes concurrent writers' batches.
fn uuid_like() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{}:{now}:{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))
}

impl Drop for D1PublicationLedger {
    fn drop(&mut self) {
        let _ = self.release();
    }
}
