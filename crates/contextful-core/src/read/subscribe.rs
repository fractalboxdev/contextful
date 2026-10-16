//! The committed-run subscription's pure domain: the notification a committed run sends,
//! the per-table position a subscriber resumes from, and the planning of one delivery over
//! a table's readable runs under the undelivered-notification bound (`read.subscribe`).

use super::error::ReadError;
use crate::store::lay_out::RunManifest;
use crate::time::Instant;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Most notifications a subscription holds undelivered before it sends its terminal
/// overrun message (`read.subscribe.overrun`).
pub const QUEUE_BOUND: usize = 1000;

/// The read frontier including a run: its commit instant, which bounds a read's `as_of`,
/// and the `_commit_seq` value its rows carry (`read.subscribe.notification`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frontier {
    pub as_of: Instant,
    pub commit_seq: i64,
}

/// One committed run of one table. It carries no row content and no row count
/// (`read.subscribe.no-content`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    pub table: String,
    pub run_id: String,
    pub node_id: String,
    pub committed_at: Instant,
    pub frontier: Frontier,
}

impl Notification {
    /// The notification a readable run sends.
    pub fn of(run: &RunManifest) -> Notification {
        Notification {
            table: run.table.clone(),
            run_id: run.run_id.clone(),
            node_id: run.node_id.clone(),
            committed_at: run.committed_at,
            frontier: Frontier { as_of: run.committed_at, commit_seq: run.commit_seq.unwrap_or(0) },
        }
    }

    /// The `<run-id>/<node-id>` position a subscriber resumes after.
    pub fn position(&self) -> String {
        format!("{}/{}", self.run_id, self.node_id)
    }
}

/// What a subscription sends: a notification, or the terminal overrun message naming each
/// table's last delivered position, `null` where none was delivered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Notification(Notification),
    Overrun { positions: BTreeMap<String, Option<String>> },
}

/// A run's place in its table's commit order: `commit_seq`, then commit instant and key for
/// a manifest that records no sequence (`read.subscribe.commit-order`).
type Order = (i64, Instant, String);

fn order(run: &RunManifest) -> Order {
    (run.commit_seq.unwrap_or(0), run.committed_at, run.key())
}

/// One table's position in a subscription: the run it resumed after, and every run
/// delivered since, so a run notifies exactly once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    table: String,
    floor: Option<Order>,
    delivered: BTreeSet<String>,
    last: Option<String>,
}

impl Cursor {
    /// The cursor resuming `table` after `after`, resolved against its readable `runs`. A
    /// position naming no readable run raises `SubscriptionPositionUnavailable`, naming the
    /// oldest resolvable one (`read.subscribe.position-unavailable`).
    pub fn start(table: &str, runs: &[RunManifest], after: Option<&str>) -> Result<Cursor, ReadError> {
        let floor = match after {
            None => None,
            Some(position) => match runs.iter().find(|r| r.key() == position) {
                Some(run) => Some(order(run)),
                None => {
                    let oldest = runs.iter().min_by_key(|r| order(r)).map_or_else(|| "none: the table holds no committed run".to_string(), RunManifest::key);
                    return Err(ReadError::SubscriptionPositionUnavailable(format!(
                        "table `{table}` holds no committed run `{position}`; the oldest resolvable position is {oldest}"
                    )));
                }
            },
        };
        Ok(Cursor { table: table.to_string(), floor, delivered: BTreeSet::new(), last: after.map(str::to_string) })
    }

    /// The table this cursor follows.
    pub fn table(&self) -> &str {
        &self.table
    }

    /// The last delivered position, or the resumed one before any delivery.
    pub fn last(&self) -> Option<&str> {
        self.last.as_deref()
    }

    /// The readable runs after the floor not yet delivered, in commit order.
    fn pending<'a>(&self, runs: &'a [RunManifest]) -> Vec<&'a RunManifest> {
        let mut out: Vec<&RunManifest> = runs
            .iter()
            .filter(|r| self.floor.as_ref().is_none_or(|floor| order(r) > *floor) && !self.delivered.contains(&r.key()))
            .collect();
        out.sort_by_key(|r| order(r));
        out
    }

    fn deliver(&mut self, run: &RunManifest) {
        self.delivered.insert(run.key());
        self.last = Some(run.key());
    }
}

/// Plan one delivery: each cursor's pending runs, in its table's commit order, from
/// `runs[i]` for `cursors[i]`. More than `bound` pending sends `bound`, then the terminal
/// overrun message, which ends the subscription (`read.subscribe.overrun`).
pub fn deliver(cursors: &mut [Cursor], runs: &[Vec<RunManifest>], bound: usize) -> Vec<Message> {
    let pending: Vec<(usize, &RunManifest)> =
        cursors.iter().zip(runs).enumerate().flat_map(|(i, (cursor, runs))| cursor.pending(runs).into_iter().map(move |r| (i, r))).collect();
    let overrun = pending.len() > bound;
    let mut out = Vec::new();
    for (i, run) in pending.into_iter().take(bound) {
        cursors[i].deliver(run);
        out.push(Message::Notification(Notification::of(run)));
    }
    if overrun {
        let positions = cursors.iter().map(|c| (c.table.clone(), c.last.clone())).collect();
        out.push(Message::Overrun { positions });
    }
    out
}
