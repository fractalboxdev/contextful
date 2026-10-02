//! Evidence into a keyed table: the key a landing write stamps on a citation, and the
//! lookup recall resolves a stamped citation through.

use super::engine::SqlEngine;
use super::face::Face;
use super::fault::ReadFault;
use contextful_core::memory::recall::EvidenceRead;
use contextful_core::memory::synthesize::EvidenceRef;
use contextful_core::read::respond::Cell;
use contextful_core::read::template::{Bindings, Bound};
use contextful_core::store::relation::ident;
use contextful_core::store::reserve::{ROW_SEQ, RUN_ID};
use contextful_policy::enforce::session::Session;
use std::collections::BTreeMap;

/// How a citation stamped: on a keyed table, live or not; on any other table, unkeyed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stamped {
    /// The cited row is its key's live version, and the citation carries its key.
    Live,
    /// The table is keyed and the cited row does not read as its key's live version
    /// through the writer's session.
    NotLive,
    /// The table declares no key, is unknown to the session, or reads masked; the
    /// citation carries no key and recall resolves its run and sequence.
    Unkeyed,
}

impl Face {
    /// The columns a citation into `table` is keyed by: the primary key, beside the
    /// valid-time start on a table declaring one, as the dedup view partitions
    /// (`store.declare.dedup-view`). Empty for an unkeyed table.
    fn key_columns(&self, table: &str) -> Vec<String> {
        let decl = self.decl(table);
        if !decl.is_keyed() {
            return Vec::new();
        }
        let mut cols = decl.primary_key().to_vec();
        if let Some(vt) = &decl.valid_time {
            cols.push(vt.from.clone());
        }
        cols
    }

    /// Whether any cell of `table` reads masked or nulled by zone in `session`.
    pub(crate) fn masked(session: &Session, table: &str) -> bool {
        session.policy(table).is_some_and(|p| {
            p.columns.values().any(|c| c.mask.is_some()) || p.columns.keys().any(|c| !p.column_set(c).admits(session.zone()))
        })
    }

    /// Stamp each citation with the key of the row it cites, read through `session`'s
    /// dedup view, replacing any key the citation carried (`read.recall.evidence-key`).
    /// Returns how each citation stamped, in order.
    pub fn stamp_evidence(&self, session: &Session, evidence: &mut [EvidenceRef]) -> Result<Vec<Stamped>, ReadFault> {
        let engine = self.pool.engine(session)?;
        Ok(evidence.iter_mut().map(|r| self.stamp(&engine, session, r)).collect())
    }

    fn stamp(&self, engine: &SqlEngine, session: &Session, r: &mut EvidenceRef) -> Stamped {
        r.key = None;
        let cols = self.key_columns(&r.table);
        let Some(relation) = session.relation(&r.table) else { return Stamped::Unkeyed };
        if cols.is_empty() || Face::masked(session, &r.table) {
            return Stamped::Unkeyed;
        }
        let select: Vec<String> = cols.iter().map(|c| format!("CAST({} AS VARCHAR)", ident(c))).collect();
        let sql = format!(
            "SELECT {} FROM {} WHERE {} = ? AND {} = ? LIMIT 1",
            select.join(", "),
            ident(relation.name()),
            ident(RUN_ID),
            ident(ROW_SEQ)
        );
        let row = engine
            .run(&sql, &Bindings::positional([Bound::Text(r.run.clone()), Bound::Integer(r.seq)]), None)
            .ok()
            .and_then(|(_, rows)| rows.into_iter().next());
        let Some(row) = row else { return Stamped::NotLive };
        let mut key = BTreeMap::new();
        for (c, v) in cols.iter().zip(row) {
            // A null key cell partitions with every other null; the citation then
            // resolves by its run and sequence alone.
            let Cell::Text(t) = v else { return Stamped::Live };
            key.insert(c.clone(), t);
        }
        r.key = Some(key);
        Stamped::Live
    }

    /// Whether the key a citation carries reads through `session`'s dedup view; `None`
    /// where the citation carries no key or its key names other columns than the table
    /// keys on now.
    pub(crate) fn key_reads(&self, engine: &SqlEngine, session: &Session, r: &EvidenceRef) -> Option<EvidenceRead> {
        let key = r.key.as_ref()?;
        let cols = self.key_columns(&r.table);
        if cols.len() != key.len() || cols.iter().any(|c| !key.contains_key(c)) {
            return None;
        }
        let Some(relation) = session.relation(&r.table) else { return Some(EvidenceRead::UnknownTable) };
        let pred: Vec<String> = cols.iter().map(|c| format!("CAST({} AS VARCHAR) = ?", ident(c))).collect();
        let sql = format!("SELECT count(*) FROM {} WHERE {}", ident(relation.name()), pred.join(" AND "));
        let params = Bindings::positional(cols.iter().map(|c| Bound::Text(key[c].clone())));
        let found = engine
            .run(&sql, &params, None)
            .ok()
            .and_then(|(_, rows)| rows.first().and_then(|row| row.first().cloned()))
            .is_some_and(|c| matches!(c, Cell::Integer { value, .. } if value > 0));
        Some(if found { EvidenceRead::Readable } else { EvidenceRead::Unreadable })
    }
}
