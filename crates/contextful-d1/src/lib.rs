//! The private HTTP transport to a Worker's D1 and R2 outbound bindings.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::Duration;
use url::Url;

mod journal;
pub use journal::D1JournalStore;
mod awakeable;
pub use awakeable::D1AwakeableStore;
mod catalog;
pub use catalog::D1Catalog;
mod blob;
pub use blob::D1BlobStore;
mod export;
pub use export::D1ExportLedger;

#[derive(Debug, Clone, Serialize)]
pub struct Statement<'a> {
    pub sql: &'a str,
    pub params: Vec<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QueryResult {
    pub rows: Vec<Value>,
    pub changes: u64,
}

/// A container has no D1 credential; the Worker intercepts this virtual host.
pub struct D1 {
    pub(crate) endpoint: String,
    agent: ureq::Agent,
}

/// A complete backup carries these D1 tables and its R2 objects together.
pub const EXECUTION_SCHEMA_VERSION: i64 = 2;
const TABLES: &[(&str, &str)] = &[
    ("contextful_execution_schema", "CREATE TABLE IF NOT EXISTS contextful_execution_schema (id INTEGER PRIMARY KEY CHECK (id = 1), version INTEGER NOT NULL)"),
    ("contextful_recovery", "CREATE TABLE IF NOT EXISTS contextful_recovery (id INTEGER PRIMARY KEY CHECK (id = 1), project TEXT NOT NULL, generation TEXT NOT NULL, frontier_sha256 TEXT NOT NULL)"),
    ("contextful_lease", "CREATE TABLE IF NOT EXISTS contextful_lease (key TEXT PRIMARY KEY, holder TEXT, expires_us INTEGER, fence INTEGER NOT NULL)"),
    ("contextful_scope", "CREATE TABLE IF NOT EXISTS contextful_scope (scope TEXT PRIMARY KEY, version INTEGER NOT NULL, cursor TEXT NOT NULL, owner TEXT, retired TEXT)"),
    ("contextful_run", "CREATE TABLE IF NOT EXISTS contextful_run (run_id TEXT PRIMARY KEY, pipeline_id TEXT NOT NULL, host_scope TEXT, started_ns INTEGER NOT NULL, row TEXT NOT NULL)"),
    ("contextful_journal", "CREATE TABLE IF NOT EXISTS contextful_journal (execution_id TEXT NOT NULL, step_label TEXT NOT NULL, input_hash TEXT NOT NULL, kind TEXT NOT NULL, holder TEXT, body TEXT NOT NULL, PRIMARY KEY (execution_id, step_label, input_hash))"),
    ("contextful_blob", "CREATE TABLE IF NOT EXISTS contextful_blob (sha256 TEXT PRIMARY KEY, put_at INTEGER NOT NULL)"),
    ("contextful_sweep", "CREATE TABLE IF NOT EXISTS contextful_sweep (id INTEGER PRIMARY KEY CHECK (id = 1), swept_at INTEGER NOT NULL)"),
    ("contextful_awakeable", "CREATE TABLE IF NOT EXISTS contextful_awakeable (token TEXT PRIMARY KEY, version INTEGER NOT NULL, body TEXT NOT NULL)"),
    ("contextful_export", "CREATE TABLE IF NOT EXISTS contextful_export (name TEXT PRIMARY KEY, version INTEGER NOT NULL, body TEXT NOT NULL)"),
    ("contextful_guard", "CREATE TABLE IF NOT EXISTS contextful_guard (scope TEXT PRIMARY KEY, token TEXT NOT NULL, admitted_at INTEGER NOT NULL)"),
    ("contextful_reconciliation", "CREATE TABLE IF NOT EXISTS contextful_reconciliation (token TEXT PRIMARY KEY, evidence TEXT NOT NULL, reconciled_at INTEGER NOT NULL)"),
];

impl D1 {
    pub fn connect(endpoint: &str) -> Result<Self, String> {
        let url = Url::parse(endpoint).map_err(|_| "StorageBindingRouteInvalid".to_string())?;
        let host = url.host_str().ok_or("StorageBindingRouteInvalid")?;
        let private = host == "contextful.d1"
            || host == "localhost"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        if url.scheme() != "http"
            || !private
            || url.username() != ""
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("StorageBindingRouteInvalid".into());
        }
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .new_agent();
        Ok(Self {
            endpoint: endpoint.trim_end_matches('/').into(),
            agent,
        })
    }

    pub fn health(&self) -> Result<(), String> {
        let response = self
            .agent
            .get(&format!("{}/health", self.endpoint))
            .call()
            .map_err(|_| "StorageBindingUnavailable")?;
        if response.status() == 200 {
            Ok(())
        } else {
            Err("StorageCapabilityMissing".into())
        }
    }

    /// A fresh D1 database binds to one project and one whole-state backup generation.
    pub fn initialize_recovery(&self, project: &str, generation: &str) -> Result<(), String> {
        self.health()?;
        let existing = self.query(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'contextful_%'",
            vec![],
        )?;
        if !existing.rows.is_empty() {
            return self.verify_recovery(project, generation);
        }
        let frontier =
            serde_json::to_vec(&json!({"project":project,"generation":generation,"objects":[]}))
                .map_err(|_| "ExecutionRecoveryMismatch")?;
        let frontier_sha256 = format!("{:x}", Sha256::digest(&frontier));
        self.put_blob(&frontier_sha256, &frontier)?;
        let mut statements: Vec<Statement<'_>> = TABLES
            .iter()
            .map(|(_, sql)| Statement {
                sql,
                params: vec![],
            })
            .collect();
        statements.push(Statement {
            sql: "INSERT OR IGNORE INTO contextful_execution_schema (id, version) VALUES (1, ?)",
            params: vec![json!(EXECUTION_SCHEMA_VERSION)],
        });
        statements.push(Statement { sql: "INSERT OR IGNORE INTO contextful_recovery (id, project, generation, frontier_sha256) VALUES (1, ?, ?, ?)", params: vec![json!(project), json!(generation), json!(frontier_sha256)] });
        self.batch(&statements)?;
        self.verify_recovery(project, generation)
    }

    /// A restored corpus alone never admits execution against an empty or mismatched D1.
    pub fn verify_recovery(&self, project: &str, generation: &str) -> Result<(), String> {
        self.health()?;
        let result = self.query(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'contextful_%'",
            vec![],
        )?;
        for (name, _) in TABLES {
            if !result
                .rows
                .iter()
                .any(|row| row.get("name").and_then(Value::as_str) == Some(name))
            {
                return Err(format!("ExecutionRecoveryMismatch: missing `{name}`"));
            }
        }
        let version = self.query(
            "SELECT version FROM contextful_execution_schema WHERE id = 1",
            vec![],
        )?;
        if version
            .rows
            .first()
            .and_then(|row| row.get("version"))
            .and_then(Value::as_i64)
            != Some(EXECUTION_SCHEMA_VERSION)
        {
            return Err("ExecutionSchemaIncompatible".into());
        }
        let identity = self.query(
            "SELECT project, generation, frontier_sha256 FROM contextful_recovery WHERE id = 1",
            vec![],
        )?;
        if identity.rows.first().is_none_or(|row| {
            row.get("project").and_then(Value::as_str) != Some(project)
                || row.get("generation").and_then(Value::as_str) != Some(generation)
        }) {
            return Err("ExecutionRecoveryMismatch".into());
        }
        let digest = identity.rows[0]
            .get("frontier_sha256")
            .and_then(Value::as_str)
            .ok_or("ExecutionRecoveryMismatch")?;
        let frontier = self.get_blob(digest)?.ok_or("ExecutionRecoveryMismatch")?;
        let manifest: Value =
            serde_json::from_slice(&frontier).map_err(|_| "ExecutionRecoveryMismatch")?;
        if manifest.get("project").and_then(Value::as_str) != Some(project)
            || manifest.get("generation").and_then(Value::as_str) != Some(generation)
        {
            return Err("ExecutionRecoveryMismatch".into());
        }
        Ok(())
    }

    pub fn query(&self, sql: &str, params: Vec<Value>) -> Result<QueryResult, String> {
        let body = serde_json::to_vec(&Statement { sql, params })
            .map_err(|_| "StorageBindingRequestInvalid")?;
        let mut response = self
            .agent
            .post(&format!("{}/sql", self.endpoint))
            .header("content-type", "application/json")
            .send(body.as_slice())
            .map_err(|_| "StorageBindingUnavailable")?;
        if response.status() != 200 {
            return Err("StorageBindingFailed".into());
        }
        let bytes = response
            .body_mut()
            .read_to_vec()
            .map_err(|_| "StorageBindingUnreadable")?;
        serde_json::from_slice(&bytes).map_err(|_| "StorageBindingMalformed".into())
    }

    pub fn batch(&self, statements: &[Statement<'_>]) -> Result<Vec<QueryResult>, String> {
        let body = serde_json::to_vec(statements).map_err(|_| "StorageBindingRequestInvalid")?;
        let mut response = self
            .agent
            .post(&format!("{}/batch", self.endpoint))
            .header("content-type", "application/json")
            .send(body.as_slice())
            .map_err(|_| "StorageBindingUnavailable")?;
        if response.status() != 200 {
            return Err("StorageBindingFailed".into());
        }
        let bytes = response
            .body_mut()
            .read_to_vec()
            .map_err(|_| "StorageBindingUnreadable")?;
        serde_json::from_slice(&bytes).map_err(|_| "StorageBindingMalformed".into())
    }

    pub fn put_blob(&self, sha256: &str, bytes: &[u8]) -> Result<(), String> {
        Self::digest(sha256, bytes)?;
        let response = self
            .agent
            .put(&format!("{}/blob/{sha256}", self.endpoint))
            .send(bytes)
            .map_err(|_| "StorageBindingUnavailable")?;
        if response.status() == 204 {
            Ok(())
        } else {
            Err("StorageBindingFailed".into())
        }
    }

    pub fn get_blob(&self, sha256: &str) -> Result<Option<Vec<u8>>, String> {
        Self::valid_digest(sha256)?;
        let mut response = self
            .agent
            .get(&format!("{}/blob/{sha256}", self.endpoint))
            .call()
            .map_err(|_| "StorageBindingUnavailable")?;
        if response.status() == 404 {
            return Ok(None);
        }
        if response.status() != 200 {
            return Err("StorageBindingFailed".into());
        }
        let bytes = response
            .body_mut()
            .read_to_vec()
            .map_err(|_| "StorageBindingUnreadable")?;
        Self::digest(sha256, &bytes)?;
        Ok(Some(bytes))
    }

    pub fn delete_blob(&self, sha256: &str) -> Result<(), String> {
        Self::valid_digest(sha256)?;
        let response = self
            .agent
            .delete(&format!("{}/blob/{sha256}", self.endpoint))
            .call()
            .map_err(|_| "StorageBindingUnavailable")?;
        if response.status() == 204 {
            Ok(())
        } else {
            Err("StorageBindingFailed".into())
        }
    }

    fn valid_digest(sha256: &str) -> Result<(), String> {
        if sha256.len() == 64
            && sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            Ok(())
        } else {
            Err("StorageBlobDigestInvalid".into())
        }
    }

    fn digest(sha256: &str, bytes: &[u8]) -> Result<(), String> {
        Self::valid_digest(sha256)?;
        if format!("{:x}", Sha256::digest(bytes)) == sha256 {
            Ok(())
        } else {
            Err("StorageBlobDigestMismatch".into())
        }
    }
}
