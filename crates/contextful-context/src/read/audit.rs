//! `audit_reads`: who read which table, under which policy and with which outcome, as a
//! relation computed from the audit chain's entries on each call
//! (`disclosure.record.projection`). The chain is the one record; the projection holds no
//! state between calls and no file backs it.

use super::engine::SqlEngine;
use super::face::{one_statement, respond, ReadOptions};
use super::fault::ReadFault;
use contextful_core::read::respond::Response;
use contextful_core::read::template::Bindings;
use contextful_policy::audit::{attr, AuditEntry};
use serde_json::Value;

/// The relation's name.
pub const AUDIT_READS: &str = "audit_reads";

/// The text-typed table the entries append to before the relation casts them.
const STAGING: &str = "__contextful_audit_staging";

/// One projected row: an entry's read of one table it named.
struct Row<'e> {
    read_at: &'e str,
    on_behalf_of: Option<&'e str>,
    agent: Option<&'e str>,
    table_name: Option<&'e str>,
    policy: Option<&'e str>,
    outcome: Option<&'e str>,
    row_count: Option<i64>,
}

/// The rows of one entry: one per table it names, or one with no table where it names
/// none. An entry carrying no read instant is no read and projects no row.
fn project(entry: &AuditEntry) -> Vec<Row<'_>> {
    let a = &entry.attributes;
    let text = |k: &str| a.get(k).and_then(Value::as_str);
    let Some(read_at) = text(attr::READ_AT) else { return Vec::new() };
    let row = |table_name| Row {
        read_at,
        on_behalf_of: text(attr::ON_BEHALF_OF),
        agent: text(attr::AGENT),
        table_name,
        policy: text(attr::POLICY),
        outcome: text(attr::OUTCOME),
        row_count: a.get(attr::ROWS).and_then(Value::as_i64),
    };
    let tables: Vec<&str> = a.get(attr::TABLES).and_then(Value::as_array).map(|t| t.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    if tables.is_empty() {
        vec![row(None)]
    } else {
        tables.into_iter().map(|t| row(Some(t))).collect()
    }
}

/// Run one operator statement over `audit_reads` projected from `entries`, on a
/// connection that reaches no file. `audit_erasures` projects row-free erasure entries.
pub fn audit_reads(entries: &[AuditEntry], sql: &str, opts: ReadOptions) -> Result<Response, ReadFault> {
    one_statement(sql)?;
    let engine = SqlEngine::projection(
        &format!(
            "CREATE TABLE {STAGING} (kind VARCHAR, read_at VARCHAR, on_behalf_of VARCHAR, agent VARCHAR, table_name VARCHAR, \
             policy VARCHAR, outcome VARCHAR, row_count VARCHAR, transaction_id VARCHAR, subject_hash VARCHAR, affected_counts VARCHAR, executed_at VARCHAR)"
        ),
        STAGING,
        entries.iter().flat_map(project).map(|r| {
            [
                Some("read".to_string()),
                Some(r.read_at.to_string()),
                r.on_behalf_of.map(str::to_string),
                r.agent.map(str::to_string),
                r.table_name.map(str::to_string),
                r.policy.map(str::to_string),
                r.outcome.map(str::to_string),
                r.row_count.map(|n| n.to_string()),
                None, None, None, None,
            ]
        }).chain(entries.iter().filter_map(|entry| {
            let attributes = &entry.attributes;
            if attributes.get("operation").and_then(Value::as_str) != Some("erasure") { return None; }
            let mut row: [Option<String>; 12] = std::array::from_fn(|_| None);
            row[0] = Some("erasure".to_string());
            for (index, name) in [(8, "transaction_id"), (9, "subject_hash"), (11, "executed_at")] {
                row[index] = Some(attributes.get(name)?.as_str()?.to_string());
            }
            let counts = attributes.get("affected_counts")?.as_object()?;
            if counts.values().any(|count| count.as_u64().is_none()) { return None; }
            row[10] = Some(Value::Object(counts.clone()).to_string());
            Some(row)
        })),
        &format!(
            "CREATE MACRO now() AS CAST(get_current_timestamp() AS TIMESTAMP); \
             CREATE TABLE {AUDIT_READS} AS SELECT CAST(CAST(read_at AS TIMESTAMPTZ) AS TIMESTAMP) AS read_at, on_behalf_of, agent, table_name, \
             policy, outcome, CAST(row_count AS BIGINT) AS row_count FROM {STAGING} WHERE kind = 'read'; \
             CREATE TABLE audit_erasures AS SELECT transaction_id, subject_hash, CAST(affected_counts AS JSON) AS affected_counts, \
             CAST(CAST(executed_at AS TIMESTAMPTZ) AS TIMESTAMP) AS executed_at FROM {STAGING} WHERE kind = 'erasure'; DROP TABLE {STAGING}"
        ),
    )?;
    respond(&engine, sql, &Bindings::default(), opts.limit, opts)
}
