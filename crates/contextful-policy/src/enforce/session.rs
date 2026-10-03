//! `authority.compose`: the session value a read carries, and the registered relation it
//! compiles per granted table.
//!
//! A [`Session`] exists only from an [`AdmittedAuthority`], and a [`RegisteredRelation`]
//! only from a session, so a function taking either cannot be reached by a path that
//! skipped admission and enforcement:
//!
//! ```compile_fail
//! let forged = contextful_policy::enforce::session::RegisteredRelation { name: String::new(), sql: String::new() };
//! ```

use super::mask::Pepper;
use super::policy::TablePolicy;
use super::predicate::{SUBJECT_FIELDS, SUBJECT_RELATION};
use super::zone::{session_zone, Zone};
use super::PolicyError;
use crate::verify::AdmittedAuthority;
use contextful_core::grant::{raw_read_covers, Action, Grant};
use contextful_core::identify::Member;
use contextful_core::read::pin::Resolved;
use contextful_core::store::declare::{DeclarationMalformed, TableDecl};
use contextful_core::store::ledger::{ledger_relation, ledger_sql, ledger_table};
use contextful_core::store::reconcile::Column;
use contextful_core::store::relation::{ident, literal};
use std::collections::BTreeMap;

/// Registered relations one session holds (`authority.compose.relations-per-session`).
pub const RELATIONS_PER_SESSION: usize = 1024;

/// The session-scoped relation holding each tenant-scoped table's granted values.
pub const TENANT_RELATION: &str = "__contextful_tenant";

/// One table as the store resolves it for this request: its declaration and policy, the
/// FROM-source over its explicit file list, the files that list names, its schema, and
/// whether any batch has landed.
#[derive(Debug, Clone)]
pub struct TableSource {
    pub decl: TableDecl,
    pub policy: TablePolicy,
    pub base: String,
    /// Absolute paths of the data files `base` reads.
    pub files: Vec<String>,
    pub columns: Vec<Column>,
    /// Whether the table has a landed schema; a quiet table registers over its injected
    /// columns, and its masks meet their columns once they land.
    pub landed: bool,
    /// Absolute paths of the table's request-ledger files.
    pub ledger: Vec<String>,
    /// The published build `base` reads, where it reads one build and nothing beside it
    /// (`read.resolve-pin.resolved-echo`).
    pub resolved: Option<Resolved>,
}

/// The relation one granted table registers as, bound to the table's bare name
/// (`authority.compose.registered-relation`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredRelation {
    name: String,
    sql: String,
    files: Vec<String>,
}

impl RegisteredRelation {
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The absolute paths of the data files the relation reads, and no other.
    pub fn files(&self) -> &[String] {
        &self.files
    }

    pub fn sql(&self) -> &str {
        &self.sql
    }
}

/// What the zone step withholds from one registered relation: the whole relation where
/// its table's effective set omits the session zone, else the columns it nulls
/// (`authority.place.excluded-row`, `authority.place.excluded-cell`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneWithheld {
    pub relation: String,
    pub excluded: bool,
    pub columns_masked: Vec<String>,
    /// For an excluded relation, the count of the rows the zone step removes: every earlier
    /// step applied, over the whole relation, reading no caller text
    /// (`authority.place.excluded-disclosed`).
    pub dropped_sql: Option<String>,
}

/// What a request asserts beside its credential.
#[derive(Debug, Clone, Default)]
pub struct Request<'a> {
    /// The zone the calling process declares for this request.
    pub zone: Option<&'a str>,
}

/// The enforcement admission value: the admitted grants, the subject claims, the session
/// zone and the registered relations compiled at admission
/// (`authority.compose.session-build`).
#[derive(Debug, Clone)]
pub struct Session {
    grants: Vec<Grant>,
    subject: Vec<(String, Option<String>)>,
    zone: Zone,
    incognito: bool,
    tenants: BTreeMap<String, (String, Vec<String>)>,
    relations: BTreeMap<String, RegisteredRelation>,
    ledgers: BTreeMap<String, RegisteredRelation>,
    policies: BTreeMap<String, TablePolicy>,
    sources: BTreeMap<String, TableSource>,
    pepper: Pepper,
}

impl Session {
    /// Open a session for an admitted authority over the tables the store resolves. A
    /// table no read grant covers registers no relation, so a read of it yields nothing
    /// and names nothing (`authority.filter-rows.default-deny`).
    pub fn open(authority: &AdmittedAuthority, request: &Request<'_>, tables: Vec<TableSource>, pepper: &Pepper) -> Result<Session, PolicyError> {
        let subject = authority.subject();
        let zone = session_zone(request.zone, subject.zone(), subject.incognito())?;
        let subject_row = SUBJECT_FIELDS
            .iter()
            .map(|f| {
                let member = match *f {
                    "on_behalf_of" => Member::OnBehalfOf,
                    "agent" => Member::Agent,
                    "host" => Member::Host,
                    "task" => Member::Task,
                    _ => Member::Zone,
                };
                (f.to_string(), subject.get(member).map(str::to_string))
            })
            .collect();
        let grants = authority.grants().to_vec();
        let granted: Vec<TableSource> = tables.into_iter().filter(|t| raw_read_covers(&grants, &t.decl.name)).collect();
        if granted.len() > RELATIONS_PER_SESSION {
            return Err(DeclarationMalformed(format!(
                "the credential reaches {} tables; a session holds at most {RELATIONS_PER_SESSION} relations",
                granted.len()
            ))
            .into());
        }
        let mut session = Session {
            grants,
            subject: subject_row,
            zone,
            incognito: subject.incognito(),
            tenants: BTreeMap::new(),
            relations: BTreeMap::new(),
            ledgers: BTreeMap::new(),
            policies: BTreeMap::new(),
            sources: BTreeMap::new(),
            pepper: pepper.clone(),
        };
        for t in granted {
            if let Some(scope) = session.tenant_values(&t.decl)? {
                session.tenants.insert(t.decl.name.clone(), scope);
            }
            if t.landed {
                t.policy.check_schema(&t.decl.name, &t.columns)?;
            }
            let relation = session.compile(&t, &t.base, t.files.clone());
            session.relations.insert(t.decl.name.clone(), relation);
            session.policies.insert(t.decl.name.clone(), t.policy.clone());
            session.sources.insert(t.decl.name.clone(), t);
        }
        if !session.tenant_scoped() {
            let ledgers: Vec<RegisteredRelation> = session
                .sources
                .values()
                .filter(|t| t.policy.rows.is_none())
                .map(|t| session.ledger(t))
                .filter(|l| !session.relations.contains_key(&l.name))
                .collect();
            session.ledgers = ledgers.into_iter().map(|l| (l.name.clone(), l)).collect();
        }
        Ok(session)
    }

    /// The tenant values the covering read grants bind on `decl`'s table, or `None` where
    /// a covering grant carries no tenant scope for it.
    fn tenant_values(&self, decl: &TableDecl) -> Result<Option<(String, Vec<String>)>, PolicyError> {
        let covering: Vec<&Grant> = self
            .grants
            .iter()
            .filter(|g| g.aggregate.is_none() && g.actions.contains(&Action::Read) && g.tables.iter().any(|p| p.covers_name(&decl.name)))
            .collect();
        let mut values = Vec::new();
        let mut column = None;
        for g in covering {
            match g.tenant.as_ref().filter(|t| t.table == decl.name) {
                None => return Ok(None),
                Some(scope) => {
                    let bare = decl.partition_by().first().map(String::as_str).filter(|c| is_bare(c));
                    let binding = scope.bind(bare)?;
                    column = Some(binding.column.to_string());
                    values.push(binding.value.to_string());
                }
            }
        }
        Ok(column.map(|c| (c, values)))
    }

    /// Compile one granted table's relation. The steps apply in order — tenant equality,
    /// table policy, table zone, then the projection carrying zone nulls and masks
    /// (`authority.compose.relation-order`) — each a conjunct of one `WHERE`, so a step
    /// removes rows and adds none (`authority.compose.conjunctive-narrowing`). Protection
    /// is this rewrite (`authority.compose.protection-is-a-rewrite`).
    fn compile(&self, t: &TableSource, base: &str, files: Vec<String>) -> RegisteredRelation {
        let name = &t.decl.name;
        let mut conjuncts = self.steps_before_zone(t);
        if !self.zone_admits(t) {
            conjuncts.push("false".to_string());
        }
        let projection: Vec<String> = t
            .columns
            .iter()
            .map(|c| {
                if !t.policy.column_set(&c.name).admits(&self.zone) {
                    format!("CAST(NULL AS {}) AS {}", c.ty.sql(), ident(&c.name))
                } else if let Some(mask) = t.policy.columns.get(&c.name).and_then(|p| p.mask.as_ref()) {
                    format!("{} AS {}", mask.sql(&c.name, &c.ty), ident(&c.name))
                } else {
                    ident(&c.name)
                }
            })
            .collect();
        let projection = if projection.is_empty() { "*".to_string() } else { projection.join(", ") };
        RegisteredRelation { name: name.clone(), sql: format!("SELECT {projection} FROM ({base}) AS \"__contextful_base\" WHERE {}", filter(&conjuncts)), files }
    }

    /// Whether the table's effective set admits the session zone
    /// (`authority.place.excluded-row`).
    fn zone_admits(&self, t: &TableSource) -> bool {
        t.policy.placement.effective().admits(&self.zone)
    }

    /// The relation's steps ahead of the zone step: tenant equality, then the table policy.
    fn steps_before_zone(&self, t: &TableSource) -> Vec<String> {
        let name = &t.decl.name;
        let mut conjuncts = Vec::new();
        if let Some((column, _)) = self.tenants.get(name) {
            // The values reach the engine as parameters into the tenant relation; the text
            // names only the column and the table (`authority.filter-rows.tenant-equality`).
            conjuncts.push(format!(
                "CAST({} AS VARCHAR) IN (SELECT \"value\" FROM {} WHERE \"table\" = {})",
                ident(column),
                ident(TENANT_RELATION),
                literal(name)
            ));
        }
        if let Some(rows) = &t.policy.rows {
            conjuncts.push(rows.sql());
        }
        conjuncts
    }

    /// What the zone step withholds from the registered relation `name` — a granted table
    /// or a request ledger — or `None` where it withholds nothing. An excluded relation
    /// names no column: none of its rows arrive.
    pub fn zone_withheld(&self, name: &str) -> Option<ZoneWithheld> {
        let (t, ledger) = match self.sources.get(name) {
            Some(t) => (t, false),
            None => (self.sources.get(ledger_table(name).filter(|_| self.ledgers.contains_key(name))?)?, true),
        };
        if !self.zone_admits(t) {
            let (base, steps) = if ledger {
                (ledger_sql(&t.ledger), Vec::new())
            } else {
                (t.base.clone(), self.steps_before_zone(t))
            };
            let sql = format!("SELECT count(*) FROM ({base}) AS \"__contextful_base\" WHERE {}", filter(&steps));
            return Some(ZoneWithheld { relation: name.to_string(), excluded: true, columns_masked: Vec::new(), dropped_sql: Some(sql) });
        }
        if ledger {
            return None;
        }
        let columns_masked: Vec<String> =
            t.columns.iter().filter(|c| !t.policy.column_set(&c.name).admits(&self.zone)).map(|c| c.name.clone()).collect();
        (!columns_masked.is_empty()).then(|| ZoneWithheld { relation: name.to_string(), excluded: false, columns_masked, dropped_sql: None })
    }

    /// The published build the table `name`'s relation reads, as the session registered
    /// it; `None` for a table outside the session or one reading no single build.
    pub fn resolved(&self, name: &str) -> Option<&Resolved> {
        self.sources.get(name)?.resolved.as_ref()
    }

    /// The columns the table `name` registered under, which a pinned build's own
    /// publication fixes; `None` for a table outside the session.
    pub fn columns(&self, name: &str) -> Option<&[Column]> {
        self.sources.get(name).map(|t| t.columns.as_slice())
    }

    /// Whether the session zone admits the table `name`, which the session reads
    /// (`read.register.describe-zone`).
    pub fn zone_admitted(&self, name: &str) -> Option<bool> {
        self.sources.get(name).map(|t| self.zone_admits(t))
    }

    /// Whether any grant this session holds carries a tenant scope. Such a session is not
    /// an owner read and registers no request ledger: ledger rows carry no tenant column to
    /// narrow on, and a table under a row policy registers none either, since its ledger
    /// carries the calls behind rows the policy withholds (`read.register.scoped-ledger`).
    pub fn tenant_scoped(&self) -> bool {
        self.grants.iter().any(|g| g.tenant.is_some())
    }

    /// A table's request-ledger relation `<table>__requests`, registered on an owner read
    /// alone (`read.register.scoped-ledger`). The table's zone gate applies to it as to the
    /// table.
    fn ledger(&self, t: &TableSource) -> RegisteredRelation {
        let filter = if self.zone_admits(t) { "true" } else { "false" };
        RegisteredRelation {
            name: ledger_relation(&t.decl.name),
            sql: format!("SELECT * FROM ({}) AS \"__contextful_base\" WHERE {filter}", ledger_sql(&t.ledger)),
            files: t.ledger.clone(),
        }
    }

    /// The table whose ledger `name` is, and what closed it, where this session reads the
    /// table but is no owner read of it: the credential carries a tenant scope, or the table
    /// a row policy. A table the session does not read yields `None`, so the refusal names
    /// nothing the credential cannot already see.
    pub fn closed_ledger<'n>(&self, name: &'n str) -> Option<(&'n str, &'static str)> {
        let table = ledger_table(name).filter(|t| self.relations.contains_key(*t) && !self.reads(name))?;
        if self.tenant_scoped() {
            Some((table, "the credential carries a tenant scope"))
        } else if self.policies.get(table).is_some_and(|p| p.rows.is_some()) {
            Some((table, "the table carries a row policy"))
        } else {
            None
        }
    }

    /// The request-ledger relations this session registers
    /// (`read.register.ledger-relation`).
    pub fn ledgers(&self) -> impl Iterator<Item = &RegisteredRelation> {
        self.ledgers.values()
    }

    /// The ledger relations among `names`, which a statement names and a connection then
    /// registers beside the table relations. Each counts toward the relation set, so a
    /// connection past [`RELATIONS_PER_SESSION`] refuses (`authority.compose.relations-per-session`).
    pub fn ledgers_named(&self, names: &std::collections::BTreeSet<String>) -> Result<Vec<&RegisteredRelation>, PolicyError> {
        let named: Vec<&RegisteredRelation> = names.iter().filter_map(|n| self.ledgers.get(n)).collect();
        let total = self.relations.len() + named.len();
        if total > RELATIONS_PER_SESSION {
            return Err(DeclarationMalformed(format!(
                "the statement's request ledgers bring the session to {total} relations; a session holds at most {RELATIONS_PER_SESSION}"
            ))
            .into());
        }
        Ok(named)
    }

    /// The table's relation compiled over another FROM-source of the same table — one of
    /// its committed files — under every step its registered relation applies.
    pub fn relation_over(&self, table: &str, base: &str, files: Vec<String>) -> Option<RegisteredRelation> {
        self.sources.get(table).map(|t| self.compile(t, base, files))
    }

    /// Whether a table has a registered relation in this session.
    pub fn reads(&self, table: &str) -> bool {
        self.relations.contains_key(table) || self.ledgers.contains_key(table)
    }

    pub fn relation(&self, table: &str) -> Option<&RegisteredRelation> {
        self.relations.get(table)
    }

    pub fn relations(&self) -> impl Iterator<Item = &RegisteredRelation> {
        self.relations.values()
    }

    pub fn policy(&self, table: &str) -> Option<&TablePolicy> {
        self.policies.get(table)
    }

    /// The subject relation's one row: each subject member and its claim, which the
    /// adapter inserts as parameters (`authority.filter-rows.subject-relation`).
    pub fn subject_row(&self) -> &[(String, Option<String>)] {
        &self.subject
    }

    /// The name of the relation subject claims are read from.
    pub fn subject_relation(&self) -> &'static str {
        SUBJECT_RELATION
    }

    /// Rows of the tenant relation: `(table, value)` per granted tenant value.
    pub fn tenant_rows(&self) -> Vec<(String, String)> {
        self.tenants.iter().flat_map(|(t, (_, vs))| vs.iter().map(move |v| (t.clone(), v.clone()))).collect()
    }

    /// Tenant-scoped tables: table to its tenant column and granted values.
    pub fn tenant_scopes(&self) -> &BTreeMap<String, (String, Vec<String>)> {
        &self.tenants
    }

    pub fn zone(&self) -> &Zone {
        &self.zone
    }

    pub fn incognito(&self) -> bool {
        self.incognito
    }

    pub fn grants(&self) -> &[Grant] {
        &self.grants
    }

    /// The pepper the query layer's mask functions hold.
    pub fn pepper(&self) -> &Pepper {
        &self.pepper
    }
}

/// The `WHERE` text over conjuncts: each narrows, and none leaves `true`.
fn filter(conjuncts: &[String]) -> String {
    if conjuncts.is_empty() {
        "true".to_string()
    } else {
        conjuncts.join(" AND ")
    }
}

/// A bare column name: an identifier with no expression around it.
fn is_bare(c: &str) -> bool {
    !c.is_empty() && c.chars().all(|ch| ch.is_alphanumeric() || ch == '_')
}
