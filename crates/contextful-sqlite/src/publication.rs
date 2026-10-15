//! Coordinated producer frontiers, byte-stable outboxes and atomic receiver activation.

use crate::{storage, stores::Db};
use contextful_core::export::publication::GroupState;
pub use contextful_core::export::publication::SourcePublication;
use contextful_core::export::publication::{
    GroupBatch, GroupSender, PublicationGroup, PublicationPlan, ReceiverState,
};
use contextful_core::run::Failure;
use contextful_core::store::encrypt::FileCipher;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS coordinated_publication (role TEXT NOT NULL, name TEXT NOT NULL, namespace TEXT NOT NULL, body TEXT NOT NULL, PRIMARY KEY(role,name,namespace));";

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
    fn sender(
        &self,
        conn: &Connection,
        name: &str,
        namespace: &str,
    ) -> Result<GroupSender, Failure> {
        Ok(read(conn, &self.path, "sender", name, namespace)?.unwrap_or_default())
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
            let prior =
                read::<SourcePublication>(tx, &self.path, "source-history", name, &source.run)?;
            source
                .check_history(prior.as_ref())
                .map_err(|e| storage(&self.path, e))?;
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
            Ok(self.sender(tx, name, namespace)?.active().cloned())
        })
    }
    pub fn pending_identity(&self, name: &str, namespace: &str) -> Result<Option<String>, Failure> {
        self.db.with_connection(false, |tx| {
            Ok(self
                .sender(tx, name, namespace)?
                .pending_identity()
                .map(str::to_owned))
        })
    }
    pub fn stage(
        &mut self,
        plan: &PublicationPlan,
        states: &GroupState,
        identity: &str,
    ) -> Result<(), Failure> {
        self.db.with_connection(true, |tx| {
            let mut sender = self.sender(tx, &plan.name, &plan.namespace)?;
            if sender
                .stage(plan, states, identity)
                .map_err(|e| storage(&self.path, e))?
            {
                write(
                    tx,
                    &self.path,
                    "sender",
                    &plan.name,
                    &plan.namespace,
                    &sender,
                )?;
            }
            Ok(())
        })?;
        self.offered
            .remove(&(plan.name.clone(), plan.namespace.clone()));
        Ok(())
    }
    pub fn batch(&mut self, name: &str, namespace: &str) -> Result<Option<GroupBatch>, Failure> {
        self.offered.remove(&(name.into(), namespace.into()));
        let batch = self.db.with_connection(false, |tx| {
            self.sender(tx, name, namespace)?
                .batch()
                .map_err(|e| storage(&self.path, e))
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
            let mut sender = read::<GroupSender>(tx, &self.path, "sender", name, namespace)?
                .ok_or_else(|| storage(&self.path, "missing publication outbox"))?;
            if sender
                .acknowledge(group, through)
                .map_err(|e| storage(&self.path, e))?
            {
                write(tx, &self.path, "active-namespace", name, "", &namespace)?;
            }
            write(tx, &self.path, "sender", name, namespace, &sender)
        })?;
        self.offered.remove(&key);
        Ok(())
    }
    pub fn checkpoint(&self, name: &str, namespace: &str) -> Result<Vec<u8>, Failure> {
        self.db.with_connection(false, |tx| {
            read::<GroupSender>(tx, &self.path, "sender", name, namespace)?
                .ok_or_else(|| storage(&self.path, "missing publication checkpoint"))?
                .checkpoint(name, namespace)
                .map_err(|e| storage(&self.path, e))
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
        self.db.with_connection(true, |tx| {
            let current = read::<GroupSender>(tx, &self.path, "sender", name, namespace)?;
            let sender = GroupSender::restore(
                bytes,
                name,
                namespace,
                incarnation,
                identity,
                current.as_ref(),
            )
            .map_err(|e| storage(&self.path, e))?;
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
