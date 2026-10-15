//! `disclosure.sweep` and `disclosure.mirror`: the `[[acl_sweep]]` block, a `--grants`
//! file read, a `--directory` file read, and the rows one sweep run commits under
//! `_visibility/<source>/`.
//!
//! [`commit`] is pure: given the source's current rows and one run's observation, it
//! returns every row the run lands, or the refusal that commits none of them.

use super::declare::{parse_budget, Binding, Fidelity};
use super::VisibilityError;
use crate::identify::{check_value, resolve_link, IdentityLink, LinkMethod, Member, NormalizedSubject};
use crate::pipeline::declare::ManifestFile;
use crate::store::declare::DeclarationMalformed;
use crate::time::Instant;
use crate::AuthorityError;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

/// The table namespace each source's access tables land under (`disclosure.mirror.grants-table`).
pub const NAMESPACE: &str = "_visibility";

/// The access tables a sweep run lands, in landing order; `freshness` lands last and names
/// the run whose `resources` and `grants` rows are current.
pub const TABLES: [&str; 7] = ["resources", "grants", "principals", "group_members", "identity_links", "tombstones", "freshness"];

/// The store path of `source`'s access table `name`.
pub fn table(source: &str, name: &str) -> String {
    format!("{NAMESPACE}/{source}/{name}")
}

#[derive(Debug, thiserror::Error)]
pub enum SweepError {
    #[error(transparent)]
    Visibility(#[from] VisibilityError),
    #[error(transparent)]
    Malformed(#[from] DeclarationMalformed),
    /// A `--grants` line that is not a grant object.
    #[error("grants file line {line}: {reason}")]
    GrantLine { line: usize, reason: String },
    /// A `--directory` line that is neither a link nor a membership object.
    #[error("directory file line {line}: {reason}")]
    DirectoryLine { line: usize, reason: String },
}

/// The two scheduled sweep kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SweepKind {
    Full,
    Incremental,
}

impl SweepKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SweepKind::Full => "full",
            SweepKind::Incremental => "incremental",
        }
    }

    pub fn parse(text: &str) -> Option<SweepKind> {
        match text {
            "full" => Some(SweepKind::Full),
            "incremental" => Some(SweepKind::Incremental),
            _ => None,
        }
    }
}

/// A source's event stream, which advances the watermark only when gap-detectable.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Events {
    pub stream: String,
    #[serde(default)]
    pub gap_detectable: bool,
}

/// One parsed `[[acl_sweep]]` block (`disclosure.sweep.sweep-block`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepBlock {
    pub source: String,
    /// The table whose rows are the source's governed resources.
    pub enumerate: String,
    pub full_every_secs: u64,
    pub incremental_every_secs: u64,
    pub credential: Option<String>,
    pub events: Option<Events>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCadence {
    every: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBlock {
    source: String,
    enumerate: String,
    full: RawCadence,
    incremental: RawCadence,
    #[serde(default)]
    credential: Option<String>,
    #[serde(default)]
    events: Option<Events>,
}

impl SweepBlock {
    /// Every `[[acl_sweep]]` block `manifest` declares. A block missing a required key, a
    /// cadence outside the staleness-budget grammar, or a second block
    /// for one source refuses (`disclosure.sweep.cadence-grammar`).
    pub fn parse_manifest(manifest: &str) -> Result<Vec<SweepBlock>, SweepError> {
        let value: toml::Value = toml::from_str(manifest).map_err(|e| DeclarationMalformed(e.to_string()))?;
        let Some(blocks) = value.get("acl_sweep") else { return Ok(Vec::new()) };
        let blocks = blocks.as_array().ok_or_else(|| DeclarationMalformed("`acl_sweep` is an array of tables, `[[acl_sweep]]`".into()))?;
        let mut out: Vec<SweepBlock> = Vec::new();
        for (i, block) in blocks.iter().enumerate() {
            let raw: RawBlock = block.clone().try_into().map_err(|e: toml::de::Error| DeclarationMalformed(format!("acl_sweep[{i}]: {e}")))?;
            if out.iter().any(|b| b.source == raw.source) {
                return Err(DeclarationMalformed(format!("acl_sweep[{i}]: source `{}` declares a second sweep", raw.source)).into());
            }
            let cadence = |kind: &str, c: &RawCadence| parse_budget(&format!("acl_sweep `{}` {kind}", raw.source), &c.every);
            out.push(SweepBlock {
                full_every_secs: cadence("full", &raw.full)?,
                incremental_every_secs: cadence("incremental", &raw.incremental)?,
                source: raw.source,
                enumerate: raw.enumerate,
                credential: raw.credential,
                events: raw.events,
            });
        }
        Ok(out)
    }

    /// Every `[[acl_sweep]]` block across a declaration set's `.toml` files — the root
    /// manifest and each `pipelines/*.toml` beside it. A source declared in two files
    /// refuses as a second sweep does within one.
    pub fn parse_set(files: &[ManifestFile]) -> Result<Vec<SweepBlock>, SweepError> {
        let mut out: Vec<SweepBlock> = Vec::new();
        let mut homes: BTreeMap<String, &str> = BTreeMap::new();
        for f in files.iter().filter(|f| f.path.ends_with(".toml")) {
            let blocks = SweepBlock::parse_manifest(&f.text).map_err(|e| match e {
                SweepError::Malformed(m) => SweepError::Malformed(DeclarationMalformed(format!("{}: {}", f.path, m.0))),
                other => other,
            })?;
            for b in blocks {
                if let Some(first) = homes.insert(b.source.clone(), &f.path) {
                    return Err(DeclarationMalformed(format!("{}: acl_sweep source `{}` declares a second sweep (first in {first})", f.path, b.source)).into());
                }
                out.push(b);
            }
        }
        Ok(out)
    }

    /// The binding of the table this block enumerates: a table of this source named
    /// `enumerate`, or `<source>/<enumerate>`.
    pub fn enumerated<'a>(&self, bindings: &'a [Binding]) -> Result<&'a Binding, SweepError> {
        let nested = format!("{}/{}", self.source, self.enumerate);
        bindings
            .iter()
            .find(|b| b.source == self.source && (b.table == self.enumerate || b.table == nested))
            .ok_or_else(|| {
                DeclarationMalformed(format!(
                    "acl_sweep `{}`: enumerate table `{}` carries no visibility block naming source `{}`",
                    self.source, self.enumerate, self.source
                ))
                .into()
            })
    }
}

/// Refuse a binding at a servable level whose source declares no `[[acl_sweep]]` block or
/// which declares no `max_acl_staleness` (`disclosure.mirror.incomplete-binding`).
pub fn check_bindings(bindings: &[Binding], sweeps: &[SweepBlock]) -> Result<(), VisibilityError> {
    for b in bindings.iter().filter(|b| b.fidelity.is_servable()) {
        let missing = match (sweeps.iter().any(|s| s.source == b.source), b.max_acl_staleness_secs.is_some()) {
            (true, true) => continue,
            (false, true) => "no `[[acl_sweep]]` block for its source",
            (true, false) => "no `max_acl_staleness`",
            (false, false) => "no `[[acl_sweep]]` block for its source and no `max_acl_staleness`",
        };
        return Err(VisibilityError::BindingIncomplete { table: b.table.clone(), origin: b.source.clone(), fidelity: b.fidelity.as_str(), missing });
    }
    Ok(())
}

/// One line of a `--grants` file (`disclosure.sweep.grants-file`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantLine {
    pub resource_id: String,
    pub principal: String,
    pub principal_kind: String,
    pub level: String,
}

/// Parse a `--grants` file: one JSON object per non-blank line carrying `resource_id`,
/// `principal`, `principal_kind` and `level`, each a non-empty string.
pub fn parse_grants(text: &str) -> Result<Vec<GrantLine>, SweepError> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let bad = |reason: String| SweepError::GrantLine { line: i + 1, reason };
        let g: GrantLine = serde_json::from_str(line).map_err(|e| bad(e.to_string()))?;
        for (k, v) in [("resource_id", &g.resource_id), ("principal", &g.principal), ("principal_kind", &g.principal_kind), ("level", &g.level)] {
            if v.is_empty() {
                return Err(bad(format!("`{k}` is empty")));
            }
        }
        out.push(g);
    }
    Ok(out)
}

/// A group membership a directory read holds: `member`, of `member_kind`, belongs to `group`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Membership {
    pub group: String,
    pub member: String,
    pub member_kind: String,
}

/// A row of `_visibility/<source>/group_members`: a membership and the instant the run that
/// read it began.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MemberRow {
    pub group: String,
    pub member: String,
    pub member_kind: String,
    pub observed_at: Instant,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkLine {
    source_principal: String,
    subject: String,
    method: LinkMethod,
    confidence: f64,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum DirectoryLine {
    Link(LinkLine),
    Member(Membership),
}

/// The identity provider's directory read of one source: the links its provisioning wrote
/// and the group memberships it holds (`authority.identify.directory-link`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Directory {
    pub links: Vec<IdentityLink>,
    pub members: Vec<Membership>,
}

/// Parse a `--directory` file of `source` (`disclosure.sweep.directory-file`): one JSON
/// object per non-blank line, a link carrying `source_principal`, `subject`, `method` and
/// `confidence`, or a membership carrying `group`, `member` and `member_kind`. Every
/// string is non-empty, `confidence` lies in `[0, 1]`, and `subject`, the `on_behalf_of`
/// principal the link maps onto, passes mint hygiene (`authority.identify.value-hygiene`).
pub fn parse_directory(source: &str, text: &str) -> Result<Directory, SweepError> {
    let mut out = Directory::default();
    for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let bad = |reason: String| SweepError::DirectoryLine { line: i + 1, reason };
        let parsed: DirectoryLine = serde_json::from_str(line)
            .map_err(|_| bad("neither a link (`source_principal`, `subject`, `method`, `confidence`) nor a membership (`group`, `member`, `member_kind`)".into()))?;
        match parsed {
            DirectoryLine::Link(l) => {
                if l.source_principal.is_empty() {
                    return Err(bad("`source_principal` is empty".into()));
                }
                check_value(Member::OnBehalfOf, &l.subject).map_err(|e| bad(e.to_string()))?;
                if !(0.0..=1.0).contains(&l.confidence) {
                    return Err(bad(format!("`confidence` {} lies outside [0, 1]", l.confidence)));
                }
                out.links.push(IdentityLink { source: source.into(), source_principal: l.source_principal, on_behalf_of: l.subject, method: l.method, confidence: l.confidence });
            }
            DirectoryLine::Member(m) => {
                for (k, v) in [("group", &m.group), ("member", &m.member), ("member_kind", &m.member_kind)] {
                    if v.is_empty() {
                        return Err(bad(format!("`{k}` is empty")));
                    }
                }
                out.members.push(m);
            }
        }
    }
    Ok(out)
}

/// The class a resource takes from its enumerating table's fidelity; an `excluded` table's
/// resources are of the unknown class.
pub fn visibility_class(fidelity: Fidelity) -> &'static str {
    match fidelity {
        Fidelity::Mirrored => "mirrored",
        Fidelity::Coarse => "coarse",
        Fidelity::Federated => "federated",
        Fidelity::Excluded => UNKNOWN_CLASS,
    }
}

/// The class no grant on a resource may land against (`disclosure.sweep.orphan-grant`).
pub const UNKNOWN_CLASS: &str = "unknown";

/// A row of `_visibility/<source>/resources`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRow {
    pub resource_id: String,
    pub resource_kind: String,
    pub visibility_class: String,
    pub observed_at: Instant,
    pub sweep_kind: String,
    pub sweep_id: String,
}

/// A row of `_visibility/<source>/grants`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantRow {
    pub resource_id: String,
    pub principal: String,
    pub principal_kind: String,
    pub level: String,
    pub observed_at: Instant,
    pub sweep_kind: String,
    pub sweep_id: String,
}

impl GrantRow {
    fn identity(&self) -> (&str, &str, &str, &str) {
        (&self.resource_id, &self.principal, &self.principal_kind, &self.level)
    }
}

/// A row of `_visibility/<source>/principals`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PrincipalRow {
    pub principal: String,
    pub principal_kind: String,
}

/// A row of `_visibility/<source>/tombstones`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tombstone {
    /// `resource` or `grant`.
    pub scope: &'static str,
    pub resource_id: String,
    pub principal: Option<String>,
    pub level: Option<String>,
    pub revoked_at: Instant,
}

/// The row of `_visibility/<source>/freshness` one committed run lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Freshness {
    pub source: String,
    pub epoch: u64,
    /// The run whose `resources` and `grants` rows are the source's current state.
    pub sweep_id: String,
    pub sweep_kind: String,
    pub watermark_at: Option<Instant>,
    pub last_full_sweep_at: Option<Instant>,
    pub last_incremental_at: Option<Instant>,
    pub budget_seconds: Option<u64>,
}

impl Freshness {
    /// Seconds from `watermark_at` to `now`; `None` before any run covered the source
    /// (`disclosure.sweep.watermark-advance`).
    pub fn lag_secs(&self, now: Instant) -> Option<u64> {
        self.watermark_at.map(|w| u64::try_from((now.unix_nanos() - w.unix_nanos()).max(0) / 1_000_000_000).unwrap_or(u64::MAX))
    }
}

/// A source's current mirrored state: the rows the latest committed run named current.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourceState {
    pub resources: Vec<ResourceRow>,
    pub grants: Vec<GrantRow>,
    pub members: Vec<MemberRow>,
    pub links: Vec<IdentityLink>,
    pub freshness: Option<Freshness>,
}

/// A governed resource: one row of the enumerating table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Governed {
    pub resource_id: String,
    pub resource_kind: String,
    pub visibility_class: String,
}

/// What one run observed of its source.
#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    pub source: String,
    pub kind: SweepKind,
    pub sweep_id: String,
    /// The instant the run began reading the source.
    pub began_at: Instant,
    /// Every resource the source governs at `began_at`.
    pub governed: Vec<Governed>,
    /// The governed resources the run read; one it failed or skipped is absent.
    pub read: BTreeSet<String>,
    pub grants: Vec<GrantLine>,
    /// The tightest `max_acl_staleness` among the source's bound tables.
    pub budget_seconds: Option<u64>,
    /// The directory read, when the run was given one; absent, the current links and
    /// memberships stand.
    pub directory: Option<Directory>,
}

impl Observation {
    /// A `--grants` run: the file is the read of every resource `governed` names
    /// (`disclosure.sweep.grants-file`).
    pub fn from_grants_file(source: &str, kind: SweepKind, sweep_id: &str, began_at: Instant, governed: Vec<Governed>, grants: Vec<GrantLine>) -> Observation {
        let read = governed.iter().map(|g| g.resource_id.clone()).collect();
        Observation { source: source.into(), kind, sweep_id: sweep_id.into(), began_at, governed, read, grants, budget_seconds: None, directory: None }
    }
}

/// Every row one committed run lands.
#[derive(Debug, Clone, PartialEq)]
pub struct Commit {
    pub resources: Vec<ResourceRow>,
    pub grants: Vec<GrantRow>,
    pub principals: Vec<PrincipalRow>,
    pub members: Vec<MemberRow>,
    pub links: Vec<IdentityLink>,
    pub tombstones: Vec<Tombstone>,
    pub freshness: Freshness,
}

/// The rows a run commits over `prior`, or the refusal that commits none.
///
/// A full run that missed a governed resource raises `VisibilityWatermarkUncovered`
/// (`disclosure.sweep.coverage-watermark`); a grant on a resource with no row, or of the
/// unknown class, raises `VisibilityOrphanGrant` (`disclosure.sweep.orphan-grant`). A full
/// run replaces resources and grants (`disclosure.sweep.full-run`); an incremental run
/// replaces the grants of the resources it read (`disclosure.sweep.incremental-run`). Each
/// withdrawn grant and each dropped resource leaves a tombstone. The watermark moves to
/// `began_at` only when the run read every governed resource
/// (`disclosure.sweep.watermark-advance`), and the epoch rises by one
/// (`disclosure.sweep.sweep-epoch`). The run's directory read replaces the source's links
/// and memberships; a run given none carries the current ones forward
/// (`disclosure.sweep.directory-file`).
pub fn commit(prior: &SourceState, obs: &Observation) -> Result<Commit, VisibilityError> {
    let missed: Vec<&str> = obs.governed.iter().map(|g| g.resource_id.as_str()).filter(|id| !obs.read.contains(*id)).collect();
    let covered = missed.is_empty();
    if obs.kind == SweepKind::Full && !covered {
        return Err(VisibilityError::WatermarkUncovered {
            origin: obs.source.clone(),
            missed: missed.len(),
            first: missed[0].to_string(),
        });
    }
    let kind = obs.kind.as_str().to_string();
    let observed = |g: &Governed| ResourceRow {
        resource_id: g.resource_id.clone(),
        resource_kind: g.resource_kind.clone(),
        visibility_class: g.visibility_class.clone(),
        observed_at: obs.began_at,
        sweep_kind: kind.clone(),
        sweep_id: obs.sweep_id.clone(),
    };
    let read_rows: BTreeMap<&str, ResourceRow> =
        obs.governed.iter().filter(|g| obs.read.contains(&g.resource_id)).map(|g| (g.resource_id.as_str(), observed(g))).collect();

    // The resources after the run: a full run's are those it read; an incremental run
    // replaces those it read and keeps the rest.
    let mut resources: BTreeMap<String, ResourceRow> = match obs.kind {
        SweepKind::Full => BTreeMap::new(),
        SweepKind::Incremental => prior.resources.iter().map(|r| (r.resource_id.clone(), r.clone())).collect(),
    };
    for (id, row) in &read_rows {
        resources.insert(id.to_string(), row.clone());
    }

    for g in &obs.grants {
        match resources.get(&g.resource_id) {
            None => {
                return Err(VisibilityError::OrphanGrant {
                    origin: obs.source.clone(),
                    resource_id: g.resource_id.clone(),
                    reason: "no resource row",
                })
            }
            Some(r) if r.visibility_class == UNKNOWN_CLASS => {
                return Err(VisibilityError::OrphanGrant {
                    origin: obs.source.clone(),
                    resource_id: g.resource_id.clone(),
                    reason: "a resource of the unknown class",
                })
            }
            Some(_) if !read_rows.contains_key(g.resource_id.as_str()) => {
                return Err(VisibilityError::OrphanGrant {
                    origin: obs.source.clone(),
                    resource_id: g.resource_id.clone(),
                    reason: "no resource row this run read",
                })
            }
            Some(_) => {}
        }
    }

    let mut seen = BTreeSet::new();
    let fresh: Vec<GrantRow> = obs
        .grants
        .iter()
        .filter(|g| seen.insert((*g).clone()))
        .map(|g| GrantRow {
            resource_id: g.resource_id.clone(),
            principal: g.principal.clone(),
            principal_kind: g.principal_kind.clone(),
            level: g.level.clone(),
            observed_at: obs.began_at,
            sweep_kind: kind.clone(),
            sweep_id: obs.sweep_id.clone(),
        })
        .collect();
    // The prior grants this run re-observed or withdrew: all of them on a full run, those
    // of the resources it read on an incremental one.
    let replaced = |g: &GrantRow| obs.kind == SweepKind::Full || read_rows.contains_key(g.resource_id.as_str());
    let mut grants: Vec<GrantRow> = prior.grants.iter().filter(|g| !replaced(g)).cloned().collect();
    grants.extend(fresh.iter().cloned());
    grants.sort_by(|a, b| a.identity().cmp(&b.identity()));

    let still: BTreeSet<(&str, &str, &str, &str)> = fresh.iter().map(GrantRow::identity).collect();
    let mut tombstones: Vec<Tombstone> = Vec::new();
    for g in prior.grants.iter().filter(|g| replaced(g) && !still.contains(&g.identity())) {
        tombstones.push(Tombstone {
            scope: "grant",
            resource_id: g.resource_id.clone(),
            principal: Some(g.principal.clone()),
            level: Some(g.level.clone()),
            revoked_at: obs.began_at,
        });
    }
    for r in prior.resources.iter().filter(|r| !resources.contains_key(&r.resource_id)) {
        tombstones.push(Tombstone { scope: "resource", resource_id: r.resource_id.clone(), principal: None, level: None, revoked_at: obs.began_at });
    }

    let principals: BTreeSet<PrincipalRow> =
        grants.iter().map(|g| PrincipalRow { principal: g.principal.clone(), principal_kind: g.principal_kind.clone() }).collect();

    let before = prior.freshness.as_ref();
    let freshness = Freshness {
        source: obs.source.clone(),
        epoch: before.map_or(0, |f| f.epoch) + 1,
        sweep_id: obs.sweep_id.clone(),
        sweep_kind: kind.clone(),
        watermark_at: if covered { Some(obs.began_at) } else { before.and_then(|f| f.watermark_at) },
        last_full_sweep_at: if obs.kind == SweepKind::Full { Some(obs.began_at) } else { before.and_then(|f| f.last_full_sweep_at) },
        last_incremental_at: if obs.kind == SweepKind::Incremental { Some(obs.began_at) } else { before.and_then(|f| f.last_incremental_at) },
        budget_seconds: obs.budget_seconds,
    };
    let (mut members, mut links) = match &obs.directory {
        Some(d) => (
            d.members
                .iter()
                .map(|m| MemberRow { group: m.group.clone(), member: m.member.clone(), member_kind: m.member_kind.clone(), observed_at: obs.began_at })
                .collect(),
            d.links.clone(),
        ),
        None => (prior.members.clone(), prior.links.clone()),
    };
    members.sort();
    members.dedup();
    links.sort_by(|a, b| (&a.source_principal, &a.on_behalf_of, a.method.as_str()).cmp(&(&b.source_principal, &b.on_behalf_of, b.method.as_str())));
    links.dedup_by(|a, b| (&a.source_principal, &a.on_behalf_of, a.method) == (&b.source_principal, &b.on_behalf_of, b.method));
    Ok(Commit { resources: resources.into_values().collect(), grants, principals: principals.into_iter().collect(), members, links, tombstones, freshness })
}

/// The resources of one source `subject` reaches through its current rows.
///
/// The reader's identity is the verified `on_behalf_of` (`authority.identify.on-behalf-of`):
/// `user://<p>` reaches the principal spelled `<p>` and each source principal a verified
/// link maps onto it, then every group those principals belong to, walked with a visited
/// set. A source principal no link maps onto the subject reaches nothing
/// (`authority.identify.unlinked-principal`), and a principal linked through
/// `operator_asserted` links alone refuses (`authority.identify.unverified-link`).
pub fn reachable(subject: &NormalizedSubject, state: &SourceState) -> Result<BTreeSet<String>, AuthorityError> {
    let Some(identity) = subject.identity() else { return Ok(BTreeSet::new()) };
    let mut principals: BTreeSet<String> = BTreeSet::new();
    if let Some(p) = identity.strip_prefix("user://") {
        principals.insert(p.to_string());
    }
    let linked: BTreeSet<(&str, &str)> =
        state.links.iter().filter(|l| l.on_behalf_of == identity).map(|l| (l.source.as_str(), l.source_principal.as_str())).collect();
    for (source, source_principal) in linked {
        let mapped: Vec<IdentityLink> = state.links.iter().filter(|l| l.on_behalf_of == identity).cloned().collect();
        if resolve_link(&mapped, source, source_principal)?.is_some() {
            principals.insert(source_principal.to_string());
        }
    }
    let mut frontier: Vec<String> = principals.iter().cloned().collect();
    while let Some(p) = frontier.pop() {
        for m in state.members.iter().filter(|m| m.member == p) {
            if principals.insert(m.group.clone()) {
                frontier.push(m.group.clone());
            }
        }
    }
    Ok(state.grants.iter().filter(|g| principals.contains(&g.principal)).map(|g| g.resource_id.clone()).collect())
}
