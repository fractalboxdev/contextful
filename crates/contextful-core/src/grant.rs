//! Actions, table patterns, tenant scope, templates, ceilings and aggregate grants.

use serde::{Deserialize, Serialize};

/// The action vocabulary (`authority.grant.actions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Read,
    Write,
    Execute,
    Admin,
}

/// A table pattern: `*`, a prefix ending in `*`, or an exact name
/// (`authority.grant.pattern-forms`). Construct through the parser, which refuses a
/// misplaced star.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum TablePattern {
    All,
    Prefix(String),
    Exact(String),
}

/// A tenant scope: an opaque byte string bound to one table's outermost partition
/// column (`authority.grant.tenant-scope`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantScope {
    pub table: String,
    pub value: String,
}

/// An aggregate grant's constraints (`authority.grant.aggregate`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AggregateGrant {
    pub min_group_size: u64,
    pub max_contributor_share: f64,
    pub functions: Vec<String>,
    pub max_groups: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rows: Option<u64>,
}

/// A grant: what a credential says a holder does (`authority.grant.fields`). An absent
/// constraint leaves its dimension unconstrained; an absent template allowlist confers
/// no template.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    pub actions: Vec<Action>,
    pub tables: Vec<TablePattern>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<TenantScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate: Option<AggregateGrant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub templates: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rows: Option<u64>,
}

impl TablePattern {
    /// Parse a pattern, refusing a star anywhere but its final position.
    pub fn parse(s: &str) -> Result<TablePattern, crate::AuthorityError> {
        todo!("authority.grant.pattern-forms: {s}")
    }
}

impl TryFrom<String> for TablePattern {
    type Error = crate::AuthorityError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        TablePattern::parse(&s)
    }
}

impl From<TablePattern> for String {
    fn from(p: TablePattern) -> String {
        match p {
            TablePattern::All => "*".to_string(),
            TablePattern::Prefix(prefix) => format!("{prefix}*"),
            TablePattern::Exact(name) => name,
        }
    }
}
