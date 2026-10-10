//! The face's closed tool set, the linked-backend identity its handshake reports, the
//! required-face check, and tool registration on an organization-wide face.

use super::error::ReadError;
use crate::enforce::EnforceError;
use serde::Serialize;

/// The closed tool set (`read.register.tool-set`).
pub const TOOLS: [&str; 8] = [
    "context.describe",
    "context.query",
    "context.reference",
    "context.execute_query",
    "context.files",
    "context.file",
    "corpus.retrieve",
    "memory.recall",
];

/// Prefixes the built-in tools hold; no template identifier takes one
/// (`read.guard.template-relation-shape`).
pub const TOOL_PREFIXES: [&str; 3] = ["context.", "corpus.", "memory."];

/// What this binary links, as the handshake reports it (`read.embed.build-identity`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct BuildIdentity {
    pub backends: Vec<String>,
    pub connectors: Vec<String>,
    pub faces: Vec<String>,
}

impl BuildIdentity {
    fn names(&self) -> impl Iterator<Item = &str> {
        self.backends.iter().chain(&self.connectors).chain(&self.faces).map(String::as_str)
    }
}

/// Refuse a client whose required names fall outside the reported set, ahead of its
/// first read. An engine reporting no set satisfies no requirement
/// (`read.embed.required-face`).
pub fn require(reported: Option<&BuildIdentity>, required: &[String]) -> Result<(), ReadError> {
    let missing: Vec<&str> = required
        .iter()
        .map(String::as_str)
        .filter(|r| !reported.is_some_and(|id| id.names().any(|n| n == *r)))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(ReadError::RequiredFaceAbsent(format!("this binary does not link {}", missing.join(", "))))
    }
}

/// Serialized bytes a table description counts as one estimated token
/// (`read.register.size-estimate`).
pub const BYTES_PER_TOKEN: u64 = 4;

/// The estimated tokens of `bytes` serialized bytes, rounded up (`read.register.size-estimate`).
pub fn estimated_tokens(bytes: u64) -> u64 {
    bytes.div_ceil(BYTES_PER_TOKEN)
}

/// The backend name of the embedded SQL engine every read tool executes on.
pub const SQL_ENGINE: &str = "duckdb";

/// Refuse a read tool on a build whose reported backends lack the embedded SQL engine,
/// rather than answering from a narrower path (`read.embed.absent-read-backend`).
pub fn read_backend(reported: &BuildIdentity, tool: &str) -> Result<(), ReadError> {
    if reported.backends.iter().any(|b| b == SQL_ENGINE) {
        Ok(())
    } else {
        Err(ReadError::ReadBackendAbsent(format!("`{tool}` reads through `{SQL_ENGINE}`, which this binary does not link")))
    }
}

/// Whom a face serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaceScope {
    /// One principal's own face.
    Personal,
    /// A face answering for an organization.
    Organization,
}

/// Whether a tool reads or writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Read,
    Write,
}

/// Register a tool on a face. An organization-wide face serves the read subset, and a
/// write tool registered on one refuses (`authority.resist.read-only-face`,
/// `authority.resist.write-tool`).
pub fn register_tool(scope: FaceScope, name: &str, kind: ToolKind) -> Result<(), EnforceError> {
    match (scope, kind) {
        (FaceScope::Organization, ToolKind::Write) => {
            Err(EnforceError::WriteOnReadOnlyFace(format!("`{name}` writes, and this face answers for an organization")))
        }
        _ => Ok(()),
    }
}
