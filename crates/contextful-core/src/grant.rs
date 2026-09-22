//! Actions, table patterns, tenant scope, templates, ceilings and aggregate grants.

use crate::AuthorityError;
use serde::{Deserialize, Serialize};

/// Least groups-per-query ceiling an aggregate grant carries (`authority.grant.group-ceiling`).
pub const AGGREGATE_GROUP_CEILING_FLOOR: u64 = 1;

/// Lifetime of a per-query tenant-scoped child, in minutes (`authority.grant.tenant-child-lifetime`).
pub const TENANT_CHILD_LIFETIME_MINUTES: i64 = 15;

/// The action vocabulary (`authority.grant.actions`). Decoding goes through
/// [`Action::parse`], so the mint and admission refuse an unknown action alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", try_from = "String")]
pub enum Action {
    Read,
    Write,
    Execute,
    Admin,
}

impl Action {
    /// Parse one action word exactly (`authority.grant.unknown-action`).
    pub fn parse(s: &str) -> Result<Action, AuthorityError> {
        match s {
            "read" => Ok(Action::Read),
            "write" => Ok(Action::Write),
            "execute" => Ok(Action::Execute),
            "admin" => Ok(Action::Admin),
            other => Err(AuthorityError::GrantActionUnknown(format!(
                "`{other}` is not one of read, write, execute, admin"
            ))),
        }
    }
}

impl TryFrom<String> for Action {
    type Error = AuthorityError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Action::parse(&s)
    }
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

impl TablePattern {
    /// Parse a pattern, refusing a star anywhere but its final position
    /// (`authority.grant.malformed-pattern`).
    pub fn parse(s: &str) -> Result<TablePattern, AuthorityError> {
        let (head, starred) = match s.strip_suffix('*') {
            Some(head) => (head, true),
            None => (s, false),
        };
        if head.contains('*') {
            return Err(AuthorityError::GrantPatternMalformed(format!("`{s}` has `*` before its final position")));
        }
        Ok(match (starred, head.is_empty()) {
            (true, true) => TablePattern::All,
            (true, false) => TablePattern::Prefix(head.to_string()),
            (false, _) => TablePattern::Exact(head.to_string()),
        })
    }

    /// Whether this pattern covers a table name: the one matcher every consumer uses
    /// (`authority.grant.one-matcher`).
    pub fn covers_name(&self, name: &str) -> bool {
        match self {
            TablePattern::All => true,
            TablePattern::Prefix(prefix) => name.starts_with(prefix.as_str()),
            TablePattern::Exact(exact) => name == exact,
        }
    }

    /// Whether this pattern covers every name `other` covers. A concrete pattern never
    /// covers `*`.
    pub fn covers(&self, other: &TablePattern) -> bool {
        match other {
            TablePattern::All => matches!(self, TablePattern::All),
            TablePattern::Prefix(q) => match self {
                TablePattern::All => true,
                TablePattern::Prefix(p) => q.starts_with(p.as_str()),
                TablePattern::Exact(_) => false,
            },
            TablePattern::Exact(name) => self.covers_name(name),
        }
    }
}

impl TryFrom<String> for TablePattern {
    type Error = AuthorityError;
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

/// A tenant scope: an opaque byte string bound to one table's outermost partition
/// column (`authority.grant.tenant-scope`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantScope {
    pub table: String,
    pub value: String,
}

/// A tenant scope bound to the column it filters on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TenantBinding<'a> {
    pub table: &'a str,
    pub column: &'a str,
    pub value: &'a str,
}

impl TenantScope {
    /// Whether a partition key or consumer tenant identifier is this scope's value:
    /// byte equality, with no trimming, case folding, collation or Unicode normalization
    /// (`authority.grant.tenant-bytes`).
    pub fn admits(&self, key: &[u8]) -> bool {
        self.value.as_bytes() == key
    }

    /// Bind the scope to its table's bare outermost partition column, which the caller
    /// resolves from the table's declaration; none refuses
    /// (`authority.grant.tenant-unbindable`).
    pub fn bind<'a>(&'a self, bare_outermost_partition: Option<&'a str>) -> Result<TenantBinding<'a>, AuthorityError> {
        match bare_outermost_partition {
            Some(column) => Ok(TenantBinding { table: &self.table, column, value: &self.value }),
            None => Err(AuthorityError::GrantTenantUnbindable(format!(
                "table `{}` declares no bare outermost partition column",
                self.table
            ))),
        }
    }
}

/// The expiry of a per-query tenant-scoped child minted at `now`: 15 min on, and never
/// past its parent's (`authority.grant.tenant-child-lifetime`). Unix seconds.
pub fn tenant_child_expiry(now: i64, parent_exp: i64) -> i64 {
    (now + TENANT_CHILD_LIFETIME_MINUTES * 60).min(parent_exp)
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

impl AggregateGrant {
    /// The groups-per-query ceiling, never below the floor (`authority.grant.group-ceiling`).
    pub fn group_ceiling(&self) -> u64 {
        self.max_groups.max(AGGREGATE_GROUP_CEILING_FLOOR)
    }

    /// Cut a query's groups at the ceiling.
    pub fn cut_groups<T>(&self, mut groups: Vec<T>) -> Vec<T> {
        groups.truncate(usize::try_from(self.group_ceiling()).unwrap_or(usize::MAX));
        groups
    }
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

/// The allowlist entry authorizing every declared template.
const ALL_TEMPLATES: &str = "*";

impl Grant {
    fn covers(&self, action: Action, name: &str) -> bool {
        self.actions.contains(&action) && self.tables.iter().any(|p| p.covers_name(name))
    }

    /// Whether this grant authorizes a template the manifest declares
    /// (`authority.grant.template-allowlist`): absent authorizes none, `*` every declared
    /// template, otherwise the named identifiers.
    pub fn allows_template(&self, id: &str, declared: &[String]) -> bool {
        let Some(allowlist) = &self.templates else {
            return false;
        };
        declared.iter().any(|d| d == id) && allowlist.iter().any(|a| a == ALL_TEMPLATES || a == id)
    }

    /// Whether this grant's allowlist names only identifiers `parent`'s named or covered.
    pub fn templates_within(&self, parent: &Grant) -> bool {
        let Some(child) = &self.templates else {
            return true;
        };
        let Some(parent) = &parent.templates else {
            return child.is_empty();
        };
        parent.iter().any(|p| p == ALL_TEMPLATES) || child.iter().all(|c| parent.contains(c))
    }
}

/// Whether any grant confers a raw read of `table`: a read grant carrying no aggregate
/// constraint (`authority.grant.aggregate`).
pub fn raw_read_covers(grants: &[Grant], table: &str) -> bool {
    grants.iter().any(|g| g.aggregate.is_none() && g.covers(Action::Read, table))
}

/// Invoke a template (`authority.grant.template-not-allowed`).
pub fn authorize_template(grants: &[Grant], id: &str, declared: &[String]) -> Result<(), AuthorityError> {
    if grants.iter().any(|g| g.allows_template(id, declared)) {
        Ok(())
    } else {
        Err(AuthorityError::GrantTemplateNotAllowed(format!("template `{id}` is outside the credential's allowlist")))
    }
}

/// The projected template listing: the declared templates some grant covers.
pub fn list_templates<'a>(grants: &[Grant], declared: &'a [String]) -> Vec<&'a str> {
    declared.iter().filter(|d| grants.iter().any(|g| g.allows_template(d, declared))).map(String::as_str).collect()
}

/// Whether a read grant covers a pipeline identifier by name; a grant over the tables it
/// lands confers none (`authority.grant.pipeline-resource`).
fn covers_pipeline(grants: &[Grant], pipeline: &str) -> bool {
    raw_read_covers(grants, pipeline)
}

/// Describe a pipeline (`authority.grant.pipeline-not-covered`).
pub fn describe_pipeline(grants: &[Grant], pipeline: &str) -> Result<(), AuthorityError> {
    if covers_pipeline(grants, pipeline) {
        Ok(())
    } else {
        Err(AuthorityError::GrantPipelineNotCovered(format!("no read grant covers pipeline `{pipeline}`")))
    }
}

/// List the declared pipelines a grant covers; none covered refuses, and a project
/// declaring none lists empty.
pub fn list_pipelines<'a>(grants: &[Grant], declared: &'a [String]) -> Result<Vec<&'a str>, AuthorityError> {
    let covered: Vec<&str> = declared.iter().filter(|p| covers_pipeline(grants, p)).map(String::as_str).collect();
    if covered.is_empty() && !declared.is_empty() {
        return Err(AuthorityError::GrantPipelineNotCovered("no read grant covers any declared pipeline".to_string()));
    }
    Ok(covered)
}

/// Trace a run, gating on the pipeline resolved from its run record; the refusal names
/// no pipeline (`authority.grant.run-trace-denied`).
pub fn trace_run(grants: &[Grant], resolved_pipeline: &str) -> Result<(), AuthorityError> {
    if covers_pipeline(grants, resolved_pipeline) {
        Ok(())
    } else {
        Err(AuthorityError::GrantRunTraceDenied("no read grant covers this run's pipeline".to_string()))
    }
}

/// A read's row ceiling: the least of the grant's, the request's, the template's and the
/// serving face's; an undeclared component imposes none (`authority.grant.row-ceiling`).
pub fn least_row_ceiling(components: [Option<u64>; 4]) -> Option<u64> {
    components.into_iter().flatten().min()
}
