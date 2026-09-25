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
use contextful_core::store::declare::{DeclarationMalformed, TableDecl};
use contextful_core::store::reconcile::Column;
use contextful_core::store::relation::{ident, literal};
use std::collections::BTreeMap;

/// Registered relations one session holds (`authority.compose.relations-per-session`).
pub const RELATIONS_PER_SESSION: usize = 1024;

/// The session-scoped relation holding each tenant-scoped table's granted values.
pub const TENANT_RELATION: &str = "__contextful_tenant";

/// One table as the store resolves it for this request: its declaration and policy, the
/// FROM-source over its explicit file list, and its schema.
#[derive(Debug, Clone)]
pub struct TableSource {
    pub decl: TableDecl,
    pub policy: TablePolicy,
    pub base: String,
    pub columns: Vec<Column>,
}

/// The relation one granted table registers as, bound to the table's bare name
/// (`authority.compose.registered-relation`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredRelation {
    name: String,
    sql: String,
}

impl RegisteredRelation {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn sql(&self) -> &str {
        &self.sql
    }
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
    policies: BTreeMap<String, TablePolicy>,
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
            policies: BTreeMap::new(),
            pepper: pepper.clone(),
        };
        for t in granted {
            let relation = session.compile(&t)?;
            session.relations.insert(t.decl.name.clone(), relation);
            session.policies.insert(t.decl.name.clone(), t.policy);
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
    fn compile(&mut self, t: &TableSource) -> Result<RegisteredRelation, PolicyError> {
        let name = &t.decl.name;
        t.policy.check_schema(name, &t.columns)?;
        let mut conjuncts = Vec::new();
        if let Some((column, values)) = self.tenant_values(&t.decl)? {
            // The values reach the engine as parameters into the tenant relation; the text
            // names only the column and the table (`authority.filter-rows.tenant-equality`).
            conjuncts.push(format!(
                "CAST({} AS VARCHAR) IN (SELECT \"value\" FROM {} WHERE \"table\" = {})",
                ident(&column),
                ident(TENANT_RELATION),
                literal(name)
            ));
            self.tenants.insert(name.clone(), (column, values));
        }
        if let Some(rows) = &t.policy.rows {
            conjuncts.push(rows.sql());
        }
        let table_set = t.policy.placement.effective();
        if !table_set.admits(&self.zone) {
            conjuncts.push("false".to_string());
        }
        let projection: Vec<String> = t
            .columns
            .iter()
            .map(|c| {
                if !t.policy.column_set(&c.name).admits(&self.zone) {
                    format!("CAST(NULL AS {}) AS {}", c.ty.sql(), ident(&c.name))
                } else if let Some(mask) = t.policy.columns.get(&c.name).and_then(|p| p.mask.as_ref()) {
                    format!("{} AS {}", mask.sql(&c.name, c.ty), ident(&c.name))
                } else {
                    ident(&c.name)
                }
            })
            .collect();
        let projection = if projection.is_empty() { "*".to_string() } else { projection.join(", ") };
        let filter = if conjuncts.is_empty() { "true".to_string() } else { conjuncts.join(" AND ") };
        Ok(RegisteredRelation {
            name: name.clone(),
            sql: format!("SELECT {projection} FROM ({}) AS \"__contextful_base\" WHERE {filter}", t.base),
        })
    }

    /// Whether a table has a registered relation in this session.
    pub fn reads(&self, table: &str) -> bool {
        self.relations.contains_key(table)
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

/// A bare column name: an identifier with no expression around it.
fn is_bare(c: &str) -> bool {
    !c.is_empty() && c.chars().all(|ch| ch.is_alphanumeric() || ch == '_')
}
