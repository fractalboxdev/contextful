//! Coordinated producer frontiers, byte-stable outboxes and atomic receiver activation.

use crate::{storage, stores::Db};
use contextful_core::export::publication::{
    batch, publication, GroupBatch, GroupState, PublicationGroup, PublicationPlan, ReceiverState,
};
use contextful_core::run::Failure;
use contextful_core::store::encrypt::FileCipher;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS coordinated_publication (role TEXT NOT NULL, name TEXT NOT NULL, namespace TEXT NOT NULL, body TEXT NOT NULL, PRIMARY KEY(role,name,namespace));";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourcePublication {
    pub run: String,
    pub input: serde_json::Value,
    pub frontiers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Sender {
    identity: String,
    incarnation: String,
    active: Option<PublicationGroup>,
    state: GroupState,
    pending: Option<GroupBatch>,
    next_state: GroupState,
    offset: usize,
}

pub struct PublicationLedger {
    db: Db,
    path: PathBuf,
    offered: BTreeMap<(String, String), (String, u64)>,
}

fn read<T: serde::de::DeserializeOwned>(
    conn: &Connection,
    path: &Path,
    role: &str,
    name: &str,
    namespace: &str,
) -> Result<Option<T>, Failure> {
    let text: Option<String> = conn
        .query_row(
            "SELECT body FROM coordinated_publication WHERE role=?1 AND name=?2 AND namespace=?3",
            params![role, name, namespace],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| storage(path, e))?;
    text.map(|text| serde_json::from_str(&text).map_err(|e| storage(path, e)))
        .transpose()
}

fn write(
    conn: &Connection,
    path: &Path,
    role: &str,
    name: &str,
    namespace: &str,
    value: &impl Serialize,
) -> Result<(), Failure> {
    let body = serde_json::to_string(value).map_err(|e| storage(path, e))?;
    conn.execute("INSERT INTO coordinated_publication(role,name,namespace,body) VALUES(?1,?2,?3,?4) ON CONFLICT(role,name,namespace) DO UPDATE SET body=excluded.body",params![role,name,namespace,body]).map_err(|e| storage(path,e))?;
    Ok(())
}

impl PublicationLedger {
    pub fn open(path: &Path) -> Result<Self, Failure> {
        Ok(Self {
            db: Db::open_schema(path, SCHEMA)?,
            path: path.into(),
            offered: BTreeMap::new(),
        })
    }
    pub fn open_sealed(path: &Path, cipher: Arc<dyn FileCipher>) -> Result<Self, Failure> {
        Ok(Self {
            db: Db::open_sealed_schema(path, cipher, SCHEMA)?,
            path: path.into(),
            offered: BTreeMap::new(),
        })
    }
    /// The store incarnation survives exporter checkpoints and catalog rebuilds.
    pub fn incarnation(&self, candidate: &str) -> Result<String, Failure> {
        self.db.with_connection(true, |tx| {
            if let Some(incarnation) = read::<String>(tx, &self.path, "incarnation", "store", "")? {
                return Ok(incarnation);
            }
            if candidate.is_empty() {
                return Err(storage(
                    &self.path,
                    "PublicationIncompatible: empty store incarnation",
                ));
            }
            write(tx, &self.path, "incarnation", "store", "", &candidate)?;
            Ok(candidate.into())
        })
    }
    /// A job completion pins the entire input frontier and every declared output.
    pub fn record_source(&self, name: &str, source: &SourcePublication) -> Result<(), Failure> {
        self.db.with_connection(true, |tx| {
            if let Some(prior) =
                read::<SourcePublication>(tx, &self.path, "source-history", name, &source.run)?
            {
                if prior != *source {
                    return Err(storage(
                        &self.path,
                        "PublicationIncompatible: producer run carries different frontier bytes",
                    ));
                }
            }
            write(tx, &self.path, "source-history", name, &source.run, source)?;
            write(tx, &self.path, "source", name, "", source)
        })
    }
    pub fn source(&self, name: &str) -> Result<Option<SourcePublication>, Failure> {
        self.db
            .with_connection(false, |tx| read(tx, &self.path, "source", name, ""))
    }
    pub fn active_namespace(&self, name: &str) -> Result<Option<String>, Failure> {
        self.db.with_connection(false, |tx| {
            read(tx, &self.path, "active-namespace", name, "")
        })
    }
    pub fn active(&self, name: &str, namespace: &str) -> Result<Option<PublicationGroup>, Failure> {
        self.db.with_connection(false, |tx| {
            Ok(read::<Sender>(tx, &self.path, "sender", name, namespace)?
                .unwrap_or_default()
                .active)
        })
    }
    pub fn pending_identity(&self, name: &str, namespace: &str) -> Result<Option<String>, Failure> {
        self.db.with_connection(false, |tx| {
            Ok(read::<Sender>(tx, &self.path, "sender", name, namespace)?
                .filter(|s| s.pending.is_some())
                .map(|s| s.identity))
        })
    }
    pub fn stage(
        &mut self,
        plan: &PublicationPlan,
        states: &GroupState,
        identity: &str,
    ) -> Result<(), Failure> {
        self.db.with_connection(true, |tx| {
            let mut sender = read::<Sender>(tx,&self.path,"sender",&plan.name,&plan.namespace)?.unwrap_or_default();
            if plan.read_identity != identity {return Err(storage(&self.path,"PublicationIncompatible: group read identity differs from its admitted binding"));}
            if !sender.incarnation.is_empty() && sender.incarnation != plan.incarnation { return Err(storage(&self.path,"PublicationIncompatible: store incarnation differs")); }
            if sender.active.as_ref().is_some_and(|g| g.plan.destination != plan.destination) {return Err(storage(&self.path,"PublicationIncompatible: target changed; use a fresh namespace rebuild"));}
            if sender.active.as_ref().is_some_and(|g| g.plan.members != plan.members) {return Err(storage(&self.path,"PublicationIncompatible: active membership changed"));}
            if let Some(pending) = &sender.pending {
                if pending.group.plan.members != plan.members || pending.group.plan.destination != plan.destination || sender.identity != identity { return Err(storage(&self.path,"PublicationIncompatible: pending membership, target or read identity differs")); }
                return Ok(());
            }
            if sender.active.as_ref().map(|g| &g.id) != plan.base.as_ref() || sender.active.as_ref().map_or(Some(1), |g| g.plan.order.checked_add(1)) != Some(plan.order) {
                return Err(storage(&self.path,"PublicationIncompatible: sender predecessor or order differs"));
            }
            if sender.active.as_ref().is_some_and(|g| g.plan.input == plan.input && g.plan.members == plan.members) && sender.identity == identity && sender.state == *states { return Ok(()); }
            let next = publication(plan.clone(),&sender.state,states).map_err(|e| storage(&self.path,e))?;
            sender.identity = identity.into();
            sender.incarnation = plan.incarnation.clone();
            sender.pending = Some(next);
            sender.next_state = states.clone();
            sender.offset = 0;
            write(tx,&self.path,"sender",&plan.name,&plan.namespace,&sender)
        })?;
        self.offered
            .remove(&(plan.name.clone(), plan.namespace.clone()));
        Ok(())
    }
    pub fn batch(&mut self, name: &str, namespace: &str) -> Result<Option<GroupBatch>, Failure> {
        self.offered.remove(&(name.into(), namespace.into()));
        let batch = self.db.with_connection(false, |tx| {
            let sender =
                read::<Sender>(tx, &self.path, "sender", name, namespace)?.unwrap_or_default();
            sender
                .pending
                .as_ref()
                .map(|p| batch(p, sender.offset).map_err(|e| storage(&self.path, e)))
                .transpose()
                .map(Option::flatten)
        })?;
        if let Some(batch) = &batch {
            self.offered.insert(
                (name.into(), namespace.into()),
                (
                    batch.group.id.clone(),
                    batch.events.last().expect("nonempty batch").sequence,
                ),
            );
        }
        Ok(batch)
    }
    pub fn acknowledge(
        &mut self,
        name: &str,
        namespace: &str,
        group: &str,
        through: u64,
    ) -> Result<(), Failure> {
        let key = (name.into(), namespace.into());
        if self.offered.get(&key) != Some(&(group.into(), through)) {
            return Err(storage(
                &self.path,
                "PublicationIncompatible: acknowledgement differs from the offered batch",
            ));
        }
        self.db.with_connection(true, |tx| {
            let mut sender = read::<Sender>(tx, &self.path, "sender", name, namespace)?
                .ok_or_else(|| storage(&self.path, "missing publication outbox"))?;
            let pending = sender
                .pending
                .as_ref()
                .ok_or_else(|| storage(&self.path, "missing pending group"))?;
            let offered = batch(pending, sender.offset)
                .map_err(|e| storage(&self.path, e))?
                .ok_or_else(|| storage(&self.path, "missing pending batch"))?;
            if offered.group.id != group
                || offered.events.last().map(|e| e.sequence) != Some(through)
            {
                return Err(storage(
                    &self.path,
                    "PublicationIncompatible: concurrent acknowledgement differs",
                ));
            }
            sender.offset += offered.events.len();
            if sender.offset == pending.events.len() {
                sender.active = Some(pending.group.clone());
                sender.state = std::mem::take(&mut sender.next_state);
                sender.pending = None;
                sender.offset = 0;
                write(tx, &self.path, "active-namespace", name, "", &namespace)?;
            }
            write(tx, &self.path, "sender", name, namespace, &sender)
        })?;
        self.offered.remove(&key);
        Ok(())
    }
    pub fn checkpoint(&self, name: &str, namespace: &str) -> Result<Vec<u8>, Failure> {
        self.db.with_connection(false, |tx| {
            let sender = read::<Sender>(tx, &self.path, "sender", name, namespace)?
                .ok_or_else(|| storage(&self.path, "missing publication checkpoint"))?;
            serde_json::to_vec(&(1, name, namespace, sender)).map_err(|e| storage(&self.path, e))
        })
    }
    pub fn restore(
        &mut self,
        name: &str,
        namespace: &str,
        bytes: &[u8],
        incarnation: &str,
        identity: &str,
    ) -> Result<(), Failure> {
        let (version, saved_name, saved_namespace, sender): (u32, String, String, Sender) =
            serde_json::from_slice(bytes).map_err(|e| storage(&self.path, e))?;
        if version != 1
            || saved_name != name
            || saved_namespace != namespace
            || sender.incarnation != incarnation
            || sender.identity != identity
        {
            return Err(storage(&self.path,"PublicationIncompatible: checkpoint name, namespace, incarnation or read identity differs"));
        }
        if let Some(pending) = &sender.pending {
            let canonical = publication(
                pending.group.plan.clone(),
                &sender.state,
                &sender.next_state,
            )
            .map_err(|e| storage(&self.path, e))?;
            if canonical != *pending || sender.offset >= pending.events.len() {
                return Err(storage(
                    &self.path,
                    "PublicationIncompatible: checkpoint changes immutable delivery bytes",
                ));
            }
        }
        self.db.with_connection(true, |tx| {
            if let Some(current) = read::<Sender>(tx, &self.path, "sender", name, namespace)? {
                if current.incarnation != incarnation
                    || current.identity != identity
                    || current
                        .active
                        .as_ref()
                        .map(|g| &g.plan.members)
                        .or_else(|| current.pending.as_ref().map(|g| &g.group.plan.members))
                        != sender
                            .active
                            .as_ref()
                            .map(|g| &g.plan.members)
                            .or_else(|| sender.pending.as_ref().map(|g| &g.group.plan.members))
                {
                    return Err(storage(
                        &self.path,
                        "PublicationIncompatible: checkpoint binding differs",
                    ));
                }
            }
            write(tx, &self.path, "sender", name, namespace, &sender)
        })?;
        self.offered.remove(&(name.into(), namespace.into()));
        Ok(())
    }
}

pub struct PublicationReceiver {
    db: Db,
    path: PathBuf,
}
impl PublicationReceiver {
    pub fn open(path: &Path) -> Result<Self, Failure> {
        Ok(Self {
            db: Db::open_schema(path, SCHEMA)?,
            path: path.into(),
        })
    }
    pub fn open_sealed(path: &Path, cipher: Arc<dyn FileCipher>) -> Result<Self, Failure> {
        Ok(Self {
            db: Db::open_sealed_schema(path, cipher, SCHEMA)?,
            path: path.into(),
        })
    }
    /// The transaction persists staging or swaps every active member before returning an acknowledgement.
    pub fn receive(&mut self, batch: &GroupBatch) -> Result<(), Failure> {
        self.db.with_connection(true, |tx| {
            let name = &batch.group.plan.name;
            let mut receiver =
                read::<ReceiverState>(tx, &self.path, "receiver", name, "")?.unwrap_or_default();
            receiver
                .receive(batch)
                .map_err(|e| storage(&self.path, e))?;
            write(tx, &self.path, "receiver", name, "", &receiver)
        })
    }
    pub fn state(&self, name: &str) -> Result<GroupState, Failure> {
        self.db.with_connection(false, |tx| {
            Ok(read::<ReceiverState>(tx, &self.path, "receiver", name, "")?
                .unwrap_or_default()
                .state)
        })
    }
    pub fn active(&self, name: &str) -> Result<Option<PublicationGroup>, Failure> {
        self.db.with_connection(false, |tx| {
            Ok(read::<ReceiverState>(tx, &self.path, "receiver", name, "")?
                .unwrap_or_default()
                .active)
        })
    }
}
