//! Evidence into a keyed table: the key digest a landing write stamps on a citation, and
//! the lookup recall resolves a citation through once its cited row no longer reads.

use super::engine::SqlEngine;
use super::face::Face;
use super::fault::ReadFault;
use contextful_core::memory::recall::EvidenceRead;
use contextful_core::memory::synthesize::EvidenceRef;
use contextful_core::read::respond::Cell;
use contextful_core::read::template::{Bindings, Bound};
use contextful_core::store::declare::TableDecl;
use contextful_core::store::relation::{ident, relation};
use contextful_core::store::reserve::{ROW_SEQ, RUN_ID};
use contextful_policy::enforce::session::Session;
use std::collections::BTreeSet;

/// How a citation stamped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stamped {
    /// The cited row is its key's live version in the writer's session.
    Live,
    /// The table is keyed, and the cited row reads through the writer's session as a
    /// version its key has since replaced.
    NotLive,
    /// The table declares no key or is unknown to the session, or no landed row the
    /// writer reads carries the citation's run and sequence; the citation carries no
    /// digest and recall resolves its run and sequence.
    Unkeyed,
}

/// The digest of a row's key under the secret bound as the statement's first parameter:
/// each key cell as text, length-prefixed so no two keys share an encoding. A null key
/// cell makes the digest null.
fn digest_sql(cols: &[String]) -> String {
    let cells: Vec<String> = cols
        .iter()
        .map(|c| {
            let text = format!("CAST({} AS VARCHAR)", ident(c));
            format!("CAST(length({text}) AS VARCHAR) || ':' || {text}")
        })
        .collect();
    format!("sha256(? || {})", cells.join(" || ',' || "))
}

/// The first cell of a `count(*)` answer, as a positive count or not.
fn counted(rows: &[Vec<Cell>]) -> bool {
    rows.first().and_then(|row| row.first()).is_some_and(|c| matches!(c, Cell::Integer { value, .. } if *value > 0))
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

    /// Whether one of `cols` of `table` reads masked or nulled by zone in `session`.
    fn key_masked(session: &Session, table: &str, cols: &[String]) -> bool {
        session.policy(table).is_some_and(|p| {
            cols.iter().any(|c| p.columns.get(c).is_some_and(|m| m.mask.is_some() || !p.column_set(c).admits(session.zone())))
        })
    }

    /// Stamp each citation with the digest of the key of the row it cites, read through
    /// `session`'s dedup view, replacing any digest the citation carried
    /// (`read.recall.evidence-key`). Returns how each citation stamped, in order.
    pub fn stamp_evidence(&self, session: &Session, evidence: &mut [EvidenceRef]) -> Result<Vec<Stamped>, ReadFault> {
        let engine = self.pool.engine(session, self.store.parquet_key())?;
        evidence.iter_mut().map(|r| self.stamp(&engine, session, r)).collect()
    }

    fn stamp(&self, engine: &SqlEngine, session: &Session, r: &mut EvidenceRef) -> Result<Stamped, ReadFault> {
        r.key_digest = None;
        let cols = self.key_columns(&r.table);
        let Some(relation) = session.relation(&r.table) else { return Ok(Stamped::Unkeyed) };
        if cols.is_empty() {
            return Ok(Stamped::Unkeyed);
        }
        let secret = session.pepper().evidence_secret(&r.table, &cols);
        let sql = format!(
            "SELECT {} FROM {} WHERE {} = ? AND {} = ? LIMIT 1",
            digest_sql(&cols),
            ident(relation.name()),
            ident(RUN_ID),
            ident(ROW_SEQ)
        );
        let params = Bindings::positional([Bound::Text(secret), Bound::Text(r.run.clone()), Bound::Integer(r.seq)]);
        let (_, rows) = engine.run(&sql, &params, None)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(if self.version_reads(engine, session, &r.table, r)? { Stamped::NotLive } else { Stamped::Unkeyed });
        };
        // A key cell read masked or nulled by zone digests a value no other session reads,
        // and a null key cell partitions with every other null; either citation resolves
        // by its run and sequence alone.
        if !Face::key_masked(session, &r.table, &cols) {
            if let Some(Cell::Text(digest)) = row.into_iter().next() {
                r.key_digest = Some(digest);
            }
        }
        Ok(Stamped::Live)
    }

    /// Whether a landed row carrying the citation's run and sequence reads through
    /// `session` under every policy step, before the dedup view keeps one row per key.
    fn version_reads(&self, engine: &SqlEngine, session: &Session, table: &str, r: &EvidenceRef) -> Result<bool, ReadFault> {
        let Some(registered) = session.relation(table) else { return Ok(false) };
        let files = registered.files().to_vec();
        if files.is_empty() {
            return Ok(false);
        }
        let schema = self.store.schema(table)?;
        let mut carried = BTreeSet::new();
        for f in &files {
            carried.extend(self.store.parquet_columns(std::path::Path::new(f))?);
        }
        let absent: Vec<_> = schema.columns.iter().filter(|c| !carried.contains(&c.name)).cloned().collect();
        let base = relation(&TableDecl::named(table), &files, &schema.columns, &absent, None)?;
        let Some(all) = session.relation_over(table, &base, files) else { return Ok(false) };
        let sql = format!("SELECT count(*) FROM ({}) WHERE {} = ? AND {} = ?", all.sql(), ident(RUN_ID), ident(ROW_SEQ));
        let (_, rows) = engine.run(&sql, &Bindings::positional([Bound::Text(r.run.clone()), Bound::Integer(r.seq)]), None)?;
        Ok(counted(&rows))
    }

    /// Whether a live row of the citation's key reads through `relation`, matched by its
    /// digest; false where the citation carries no digest. A statement fault reads as no
    /// row, so the claim is withheld rather than served.
    pub(crate) fn key_reads(&self, engine: &SqlEngine, session: &Session, relation: &str, r: &EvidenceRef, deadline: Option<(u64, &'static str)>) -> Result<bool, ReadFault> {
        let Some(digest) = r.key_digest.as_ref() else { return Ok(false) };
        let cols = self.key_columns(&r.table);
        if cols.is_empty() {
            return Ok(false);
        }
        let secret = session.pepper().evidence_secret(&r.table, &cols);
        let sql = format!("SELECT count(*) FROM {} WHERE {} = ?", ident(relation), digest_sql(&cols));
        evidence_count(engine, &sql, &Bindings::positional([Bound::Text(secret), Bound::Text(digest.clone())]), deadline)
    }

    /// How one evidence row reads through the caller's session: its table registered, the
    /// cited row visible through the relation or, failing that, a live row of its key
    /// (`read.recall.evidence-key`), and no cell of the table masked or nulled by zone.
    pub(crate) fn evidence_read(&self, engine: &SqlEngine, session: &Session, r: &EvidenceRef, outer: &BTreeSet<String>, request: Option<u64>) -> Result<EvidenceRead, ReadFault> {
        let Some(relation) = session.relation(&r.table) else { return Ok(EvidenceRead::UnknownTable) };
        let mut touched = outer.clone();
        touched.insert(r.table.clone());
        let deadline = self.duration_budget(session, &touched, request);
        let read = if self.row_reads(engine, relation.name(), r, deadline)? {
            EvidenceRead::Readable
        } else if self.key_reads(engine, session, relation.name(), r, deadline)? {
            EvidenceRead::Stale
        } else {
            return Ok(EvidenceRead::Unreadable);
        };
        Ok(if Face::masked(session, &r.table) {
            EvidenceRead::Masked
        } else {
            read
        })
    }

    /// Whether the row a citation's run and sequence name reads through `relation`.
    fn row_reads(&self, engine: &SqlEngine, relation: &str, r: &EvidenceRef, deadline: Option<(u64, &'static str)>) -> Result<bool, ReadFault> {
        let sql = row_reads_sql(relation);
        evidence_count(engine, &sql, &Bindings::positional([Bound::Text(r.run.clone()), Bound::Integer(r.seq)]), deadline)
    }
}

/// The statement reading whether a cited run and sequence row reads through `relation`.
pub(crate) fn row_reads_sql(relation: &str) -> String {
    format!("SELECT count(*) FROM {} WHERE {} = ? AND {} = ?", ident(relation), ident(RUN_ID), ident(ROW_SEQ))
}

fn evidence_count(engine: &SqlEngine, sql: &str, parameters: &Bindings, deadline: Option<(u64, &'static str)>) -> Result<bool, ReadFault> {
    let result = match deadline {
        Some((ms, source)) => engine.run_timed(sql, parameters, None, ms, source),
        None => engine.run(sql, parameters, None),
    };
    match result {
        Ok((_, rows)) => Ok(counted(&rows)),
        Err(error) if error.refusal().is_some_and(|r| r.identifier() == "ReadDurationExceeded") => Err(error),
        Err(_) => Ok(false),
    }
}
