//! A store-driven job's input read (`run.journal.store-input`): its statement admitted and
//! run under the job's session at the job's `as_of`, beside the snapshot id of every table
//! the statement touches.

use super::engine::SqlEngine;
use super::face::{admit_in, Face, ReadOptions};
use super::fault::ReadFault;
use contextful_core::read::respond::Response;
use contextful_core::run::journal::sha256_hex;
use contextful_core::store::bound_time::Bounds;
use contextful_policy::enforce::session::Session;
use serde_json::Map;
use std::collections::BTreeMap;

/// The snapshot id of a table with no committed file under the bound.
pub const EMPTY_SNAPSHOT: &str = "empty";

impl Face {
    /// Run `sql` as a store-driven input through the guard, under `session` and `bounds`,
    /// answering the response and, per touched table, the sha256 of the committed file list
    /// the bound resolves: the snapshot the rows came from. A session
    /// [`Face::session_carried`] opened registers each carried table at its carried
    /// `as_of`, so its snapshot id is the one that origin read.
    pub fn input(&self, session: &Session, sql: &str, bounds: Bounds) -> Result<(Response, BTreeMap<String, String>), ReadFault> {
        let response = self.query_with(session, sql, &Map::new(), ReadOptions { bounds, ..ReadOptions::default() })?;
        let tree = SqlEngine::bare()?.serialize(sql)?;
        let admitted = admit_in(session, &tree)?;
        let snapshots = self.snapshot_ids(session, admitted.relations.iter().map(String::as_str))?;
        Ok((response, snapshots))
    }

    /// Per table, the sha256 of the file list registered in `session`, or
    /// [`EMPTY_SNAPSHOT`] for a table holding no schema: the snapshot a read reached
    /// (`run.journal.store-input`, `disclosure.record.bound-entry`).
    pub fn snapshot_ids<'t>(&self, session: &Session, tables: impl IntoIterator<Item = &'t str>) -> Result<BTreeMap<String, String>, ReadFault> {
        let mut snapshots = BTreeMap::new();
        for table in tables {
            let id = match session.landed(table) {
                Some(true) => sha256_hex(session.relation(table).expect("a landed source has a relation").files().join("\n").as_bytes()),
                Some(false) => EMPTY_SNAPSHOT.to_string(),
                None => return Err(contextful_core::enforce::EnforceError::UnknownRelation(format!("`{table}`")).into()),
            };
            snapshots.insert(table.to_string(), id);
        }
        Ok(snapshots)
    }
}
