//! `disclosure.explain`: why a principal does or does not reach a table, what it read over
//! a window, and who reaches a table, answered from the exchange policy's grants, the
//! table's declaration and the audit chain, never from the table's rows.
//!
//! An [`Explanation`] is assembled from those three sources and leaves only through
//! [`Explanation::seal`], which holds the four refusals: no resource content
//! (`disclosure.explain.no-row`), no negative answer without its [`Coverage`]
//! (`disclosure.explain.unqualified-assurance`), and no member identity of a group on the
//! path (`disclosure.explain.groups-not-members`). A replay window holding no chain
//! entry raises before any claim forms (`disclosure.explain.empty-window`).

use crate::audit::{attr, AuditEntry};
use crate::enforce::policy::TablePolicy;
use crate::enforce::PolicyError;
use contextful_core::disclosure::declare::Binding;
use contextful_core::disclosure::VisibilityError;
use contextful_core::exchange::ExchangePolicy;
use contextful_core::grant::{Action, Grant};
use contextful_core::store::declare::TableDecl;
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use time::format_description::well_known::Rfc3339;
pub use time::OffsetDateTime;

/// The answer to an access question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Admit,
    Deny,
}

/// One step of a decision's path: the layer and what it names there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Step {
    pub layer: &'static str,
    pub name: String,
}

impl Step {
    fn new(layer: &'static str, name: impl Into<String>) -> Step {
        Step { layer, name: name.into() }
    }
}

/// The layer a role names: a group on the path, rendered by name alone.
pub const ROLE: &str = "role";

/// An access decision and the path that produced it (`disclosure.explain.decision`,
/// `disclosure.explain.path`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Decision {
    pub verdict: Verdict,
    pub path: Vec<Step>,
}

/// A closed replay window, both ends RFC 3339.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub from: OffsetDateTime,
    pub to: OffsetDateTime,
}

impl Window {
    /// Parse `<from>..<to>`, two RFC 3339 instants with `from` not after `to`.
    pub fn parse(text: &str) -> Result<Window, String> {
        let (from, to) = text.split_once("..").ok_or_else(|| format!("window `{text}` is not <from>..<to>"))?;
        let instant =
            |s: &str| OffsetDateTime::parse(s.trim(), &Rfc3339).map_err(|e| format!("window end `{s}` is not RFC 3339: {e}"));
        let (from, to) = (instant(from)?, instant(to)?);
        if from > to {
            return Err(format!("window `{text}` ends before it starts"));
        }
        Ok(Window { from, to })
    }

    fn holds(&self, at: OffsetDateTime) -> bool {
        self.from <= at && at <= self.to
    }
}

/// One replayed chain entry (`disclosure.explain.replay`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Replayed {
    pub seq: u64,
    pub read_at: String,
    pub outcome: Option<String>,
    pub agent: Option<String>,
    /// The entry's recorded `contextful.*` attributes beyond its subject members.
    pub attributes: Map<String, Value>,
}

/// A span of a window the chain does not reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Span {
    pub from: String,
    pub to: String,
}

/// The sweep state of one visibility binding on the decided table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Watermark {
    pub table: String,
    pub source: String,
    pub fidelity: &'static str,
    /// The last completed sweep's watermark; `None` where no sweep recorded one.
    pub watermark_at: Option<String>,
}

/// What an explanation covered (`disclosure.explain.coverage`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Coverage {
    /// Segment files verified.
    pub segments: u64,
    /// Entries verified.
    pub entries: u64,
    /// Entries carrying no read instant, which no window places.
    pub unplaced: u64,
    /// Window spans before the chain's first placed entry or after the call.
    pub gaps: Vec<Span>,
    pub visibility: Vec<Watermark>,
    /// The grant source the decision read, or `None` where the project declares none.
    pub grants: Option<String>,
}

/// Reads of one principal class (`disclosure.explain.audience`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClassReads {
    /// The scheme of `on_behalf_of`, such as `user` or `service`.
    pub class: String,
    pub principals: u64,
    pub reads: u64,
}

/// Who reaches a table: roles by name, the default set, and principal classes by count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Audience {
    pub roles: Vec<String>,
    pub default_grants: bool,
    pub classes: Vec<ClassReads>,
}

/// One explanation, before [`Explanation::seal`] releases it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Explanation {
    pub table: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<Decision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay: Option<Vec<Replayed>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audience: Option<Audience>,
    pub coverage: Option<Coverage>,
}

impl Explanation {
    /// Whether the answer is negative: a denial, a replay holding no served read, or an
    /// audience naming nobody.
    pub fn negative(&self) -> bool {
        self.decision.as_ref().is_some_and(|d| d.verdict == Verdict::Deny)
            || self.replay.as_ref().is_some_and(|r| !r.iter().any(|e| e.outcome.as_deref() == Some(attr::SERVED)))
            || self.audience.as_ref().is_some_and(|a| a.roles.is_empty() && !a.default_grants && a.classes.is_empty())
    }

    /// The groups on the path: every role the decision or the audience names.
    fn groups(&self) -> BTreeSet<&str> {
        let path = self.decision.iter().flat_map(|d| d.path.iter()).filter(|s| s.layer == ROLE).map(|s| s.name.as_str());
        let audience = self.audience.iter().flat_map(|a| a.roles.iter()).map(String::as_str);
        path.chain(audience).collect()
    }

    /// Release the explanation as JSON, or refuse: a replayed `contextful.result.*`
    /// attribute beyond the row count raises `VisibilityDiagnosticRow`, a negative answer
    /// without coverage `VisibilityUnqualifiedAssurance`, and a rendering carrying a
    /// member identity `members` returns for a group on the path `VisibilityIndividualNamed`.
    pub fn seal(&self, members: &dyn Fn(&str) -> Vec<String>) -> Result<Value, VisibilityError> {
        for entry in self.replay.iter().flatten() {
            if let Some(key) = entry.attributes.keys().find(|k| k.starts_with("contextful.result.") && k.as_str() != attr::ROWS) {
                return Err(VisibilityError::DiagnosticRow(format!(
                    "entry {} carries `{key}`, content from the table `{}` an explanation never returns",
                    entry.seq, self.table
                )));
            }
        }
        if self.negative() && self.coverage.is_none() {
            return Err(VisibilityError::UnqualifiedAssurance(format!(
                "the negative answer about `{}` carries no coverage block",
                self.table
            )));
        }
        let rendered = serde_json::to_value(self).map_err(|e| VisibilityError::DiagnosticRow(e.to_string()))?;
        // The subject is the caller's own input and echoes back (P2); every other string
        // is the explanation's own rendering.
        let mut texts = Vec::new();
        if let Value::Object(o) = &rendered {
            o.iter().filter(|(k, _)| k.as_str() != "subject").for_each(|(_, v)| strings(v, &mut texts));
        }
        for group in self.groups() {
            for member in members(group) {
                if !member.is_empty() && texts.iter().any(|t| t.contains(member.as_str())) {
                    return Err(VisibilityError::IndividualNamed(format!(
                        "the explanation renders a member of the group `{group}` on its path"
                    )));
                }
            }
        }
        Ok(rendered)
    }
}

/// Every string a value holds, keys included.
fn strings<'v>(v: &'v Value, out: &mut Vec<&'v str>) {
    match v {
        Value::String(s) => out.push(s),
        Value::Array(a) => a.iter().for_each(|v| strings(v, out)),
        Value::Object(o) => o.iter().for_each(|(k, v)| {
            out.push(k);
            strings(v, out)
        }),
        _ => {}
    }
}

/// The query-time steps `decl` declares, in relation order: the mirrored-permission
/// semi-join, tenant equality, the table policy, the table zone, then the masked columns.
pub fn steps(decl: &TableDecl, tenant_claim: Option<&str>) -> Result<Vec<Step>, PolicyError> {
    let mut out = Vec::new();
    if let Some(b) = Binding::of(decl)? {
        out.push(Step::new("visibility", format!("{} {}", b.source, b.fidelity.as_str())));
    }
    if let Some(claim) = tenant_claim {
        out.push(Step::new("tenant", claim));
    }
    let policy = TablePolicy::from_decl(decl)?;
    if let Some(rows) = &policy.rows {
        out.push(Step::new("table-policy", format!("row predicate, {} exceptions", rows.exceptions.len())));
    }
    if policy.placement.declared.is_some() || policy.placement.protected {
        out.push(Step::new("zone", policy.placement.effective().labels().join(", ")));
    }
    for (column, c) in &policy.columns {
        if c.mask.is_some() {
            out.push(Step::new("mask", column.clone()));
        }
    }
    Ok(out)
}

/// How one grant reads in a path.
fn grant_name(g: &Grant) -> String {
    let actions: Vec<&str> = g
        .actions
        .iter()
        .map(|a| match a {
            Action::Read => "read",
            Action::Write => "write",
            Action::Execute => "execute",
            Action::Admin => "admin",
            Action::Forget => "forget",
        })
        .collect();
    let tables: Vec<String> = g.tables.iter().cloned().map(String::from).collect();
    format!("{} {}", actions.join(","), tables.join(","))
}

/// The grant among `grants` carrying `read` over a pattern covering `table`.
fn covering<'g>(grants: impl IntoIterator<Item = &'g Grant>, table: &str) -> Option<&'g Grant> {
    grants.into_iter().find(|g| g.actions.contains(&Action::Read) && g.tables.iter().any(|p| p.covers_name(table)))
}

/// Decide whether the grants `policy` issues for `roles` read `table`: the matching roles'
/// grants, or the default set where no role earns a grant, as the exchange mints them. An
/// admitting path continues through `steps`; a denial ends at the default deny.
pub fn decide(policy: Option<&ExchangePolicy>, roles: &[String], table: &str, steps: Vec<Step>) -> Decision {
    let mut path = Vec::new();
    // A role declared with no grant earns nothing, so it matches nothing, as in
    // `ExchangePolicy::mint_request`.
    let matched: Vec<&String> = policy
        .map(|p| roles.iter().filter(|r| p.role_grants.get(*r).is_some_and(|g| !g.is_empty())).collect())
        .unwrap_or_default();
    let mut found = None;
    if let Some(p) = policy {
        if matched.is_empty() {
            path.push(Step::new("default-grants", format!("{} grants", p.default_grants.len())));
            found = covering(&p.default_grants, table);
        } else {
            for role in &matched {
                path.push(Step::new(ROLE, role.as_str()));
                if found.is_none() {
                    found = covering(&p.role_grants[*role], table);
                }
            }
        }
    }
    match found {
        Some(g) => {
            path.push(Step::new("grant", grant_name(g)));
            path.extend(steps);
            Decision { verdict: Verdict::Admit, path }
        }
        None => {
            path.push(Step::new("default-deny", format!("no grant reads `{table}`")));
            Decision { verdict: Verdict::Deny, path }
        }
    }
}

/// The read instant an entry records.
fn read_at(e: &AuditEntry) -> Option<OffsetDateTime> {
    e.attributes.get(attr::READ_AT).and_then(Value::as_str).and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok())
}

fn text<'e>(e: &'e AuditEntry, key: &str) -> Option<&'e str> {
    e.attributes.get(key).and_then(Value::as_str)
}

fn names_table(e: &AuditEntry, table: &str) -> bool {
    e.attributes.get(attr::TABLES).and_then(Value::as_array).is_some_and(|t| t.iter().any(|v| v.as_str() == Some(table)))
}

/// The members [`Explanation::seal`] holds a group on the path to: every principal the
/// chain records reading `table`, under any outcome. Role membership stays outside the
/// store, so the table's recorded readers stand in for each group's members.
pub fn readers(entries: &[AuditEntry], table: &str) -> Vec<String> {
    let who: BTreeSet<&str> = entries
        .iter()
        .filter(|e| names_table(e, table))
        .filter_map(|e| text(e, attr::ON_BEHALF_OF))
        .filter(|p| !p.is_empty())
        .collect();
    who.into_iter().map(str::to_string).collect()
}

/// Refuse a window no chain entry falls in.
fn observed(entries: &[AuditEntry], window: &Window) -> Result<(), VisibilityError> {
    if entries.iter().filter_map(read_at).any(|at| window.holds(at)) {
        return Ok(());
    }
    Err(VisibilityError::NoObservations(format!(
        "the chain holds no entry between {} and {}; no claim is available",
        fmt(window.from),
        fmt(window.to)
    )))
}

/// Replay each entry naming `subject`, as principal or agent, and `table`, whose read
/// instant falls in `window`; a window holding no entry at all raises
/// `VisibilityNoObservations`.
pub fn replay(entries: &[AuditEntry], subject: &str, table: &str, window: &Window) -> Result<Vec<Replayed>, VisibilityError> {
    observed(entries, window)?;
    Ok(entries
        .iter()
        .filter(|e| text(e, attr::ON_BEHALF_OF) == Some(subject) || text(e, attr::AGENT) == Some(subject))
        .filter(|e| names_table(e, table))
        .filter_map(|e| read_at(e).filter(|at| window.holds(*at)).map(|_| e))
        .map(|e| Replayed {
            seq: e.seq,
            read_at: text(e, attr::READ_AT).unwrap_or_default().to_string(),
            outcome: text(e, attr::OUTCOME).map(str::to_string),
            agent: text(e, attr::AGENT).map(str::to_string),
            attributes: e
                .attributes
                .as_object()
                .map(|o| {
                    o.iter()
                        .filter(|(k, _)| k.starts_with("contextful.") && !k.starts_with("contextful.subject."))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect())
}

fn fmt(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}

/// The coverage of an explanation over `entries` read from `segments` segment files: the
/// window spans before the first placed entry or after `now`, and each binding's sweep
/// watermark, none being recorded.
pub fn coverage(
    entries: &[AuditEntry],
    segments: u64,
    window: Option<&Window>,
    now: OffsetDateTime,
    bindings: &[Binding],
    grants: Option<String>,
) -> Coverage {
    let placed: Vec<OffsetDateTime> = entries.iter().filter_map(read_at).collect();
    let mut gaps = Vec::new();
    if let Some(w) = window {
        let first = placed.iter().min().copied();
        match first {
            Some(f) if w.from < f => gaps.push(Span { from: fmt(w.from), to: fmt(f.min(w.to)) }),
            None => gaps.push(Span { from: fmt(w.from), to: fmt(w.to.min(now)) }),
            _ => {}
        }
        if w.to > now {
            gaps.push(Span { from: fmt(now.max(w.from)), to: fmt(w.to) });
        }
    }
    Coverage {
        segments,
        entries: entries.len() as u64,
        unplaced: (entries.len() - placed.len()) as u64,
        gaps,
        visibility: bindings
            .iter()
            .map(|b| Watermark {
                table: b.table.clone(),
                source: b.source.clone(),
                fidelity: b.fidelity.as_str(),
                watermark_at: None,
            })
            .collect(),
        grants,
    }
}

/// Who reaches `table`: the roles whose grants read it, by name, whether the default set
/// does, and per `on_behalf_of` scheme the principals and served reads in `window`, or
/// across the chain without one.
pub fn audience(
    policy: Option<&ExchangePolicy>,
    table: &str,
    entries: &[AuditEntry],
    window: Option<&Window>,
) -> Result<Audience, VisibilityError> {
    if let Some(w) = window {
        observed(entries, w)?;
    }
    let roles = policy
        .map(|p| p.role_grants.iter().filter(|(_, g)| covering(g.iter(), table).is_some()).map(|(r, _)| r.clone()).collect())
        .unwrap_or_default();
    let default_grants = policy.is_some_and(|p| covering(&p.default_grants, table).is_some());
    let mut classes: BTreeMap<String, (BTreeSet<&str>, u64)> = BTreeMap::new();
    for e in entries {
        if text(e, attr::OUTCOME) != Some(attr::SERVED) || !names_table(e, table) {
            continue;
        }
        if let Some(w) = window {
            if !read_at(e).is_some_and(|at| w.holds(at)) {
                continue;
            }
        }
        let principal = text(e, attr::ON_BEHALF_OF).unwrap_or("");
        let class = principal.split_once("://").map_or("unnamed", |(scheme, _)| scheme);
        let slot = classes.entry(class.to_string()).or_default();
        slot.0.insert(principal);
        slot.1 += 1;
    }
    Ok(Audience {
        roles,
        default_grants,
        classes: classes
            .into_iter()
            .map(|(class, (who, reads))| ClassReads { class, principals: who.len() as u64, reads })
            .collect(),
    })
}
