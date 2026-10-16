//! The committed-run subscription over a store root: each poll re-reads the tables' run
//! manifests and commit logs, so a run any process commits reaches a subscriber in another
//! (`read.subscribe.durable-source`), and only a readable run notifies
//! (`read.subscribe.after-commit`).

use crate::{ContextError, Result, Store};
use contextful_core::read::subscribe::{deliver, Cursor, Message, QUEUE_BOUND};
use std::collections::BTreeMap;

/// A subscription to the committed runs of named tables.
pub struct Subscription {
    store: Store,
    cursors: Vec<Cursor>,
    bound: usize,
    ended: bool,
}

impl Subscription {
    /// Open a subscription over `tables`, each resuming after its `after` position when one
    /// is named (`read.subscribe.resume`). An unknown table refuses
    /// (`store.lay-out.unknown-table`); a position naming no committed run refuses
    /// (`read.subscribe.position-unavailable`).
    pub fn open(store: &Store, tables: &[String], after: &BTreeMap<String, String>) -> Result<Subscription> {
        if let Some(table) = after.keys().find(|t| !tables.contains(t)) {
            return Err(ContextError::Invalid(format!("`after` names table `{table}`, which the subscription does not name")));
        }
        let mut cursors = Vec::with_capacity(tables.len());
        for table in tables {
            store.schema(table)?;
            let runs = store.committed_runs(table)?;
            cursors.push(Cursor::start(table, &runs, after.get(table).map(String::as_str))?);
        }
        Ok(Subscription { store: store.clone(), cursors, bound: QUEUE_BOUND, ended: false })
    }

    /// The messages since the last poll: each newly readable run in its table's commit
    /// order, and the terminal overrun message past the bound (`read.subscribe.overrun`).
    /// An ended subscription sends nothing.
    pub fn poll(&mut self) -> Result<Vec<Message>> {
        if self.ended {
            return Ok(Vec::new());
        }
        let runs = self.cursors.iter().map(|c| self.store.committed_runs(c.table())).collect::<Result<Vec<_>>>()?;
        let messages = deliver(&mut self.cursors, &runs, self.bound);
        self.ended = messages.iter().any(|m| matches!(m, Message::Overrun { .. }));
        Ok(messages)
    }

    /// Whether the subscription sent its terminal overrun message.
    pub fn ended(&self) -> bool {
        self.ended
    }

    /// Each table's last delivered position, or its resumed one before any delivery.
    pub fn positions(&self) -> BTreeMap<String, Option<String>> {
        self.cursors.iter().map(|c| (c.table().to_string(), c.last().map(str::to_string))).collect()
    }
}
