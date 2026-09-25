//! `authority.place`: the zone grammar, allow-sets and their symbolic algebra, floors,
//! incognito, and the serve-time outcome for one row.

use super::PolicyError;
use contextful_core::enforce::EnforceError;
use contextful_core::store::declare::DeclarationMalformed;
use serde::Serialize;

/// Patterns one zone allow-set holds (`authority.place.allow-set-entries`).
pub const ALLOW_SET_ENTRIES: usize = 32;

/// Characters in a zone identifier (`authority.place.identifier-length`).
pub const ZONE_IDENTIFIER_LENGTH: usize = 128;

/// The categories carrying an identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    OnPrem,
    PrivateCloud,
    PublicCloud,
}

impl Category {
    fn parse(s: &str) -> Option<Category> {
        Some(match s {
            "on-prem" => Category::OnPrem,
            "private-cloud" => Category::PrivateCloud,
            "public-cloud" => Category::PublicCloud,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Category::OnPrem => "on-prem",
            Category::PrivateCloud => "private-cloud",
            Category::PublicCloud => "public-cloud",
        }
    }
}

/// A zone a calling process declares. Text outside the grammar is undeclared.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Zone {
    LocalDevice,
    In(Category, String),
    Undeclared,
}

/// An identifier: 1 to 128 chars of `[A-Za-z0-9._-]`.
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars().count() <= ZONE_IDENTIFIER_LENGTH
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

impl Zone {
    /// Parse a zone string after trimming (`authority.place.zone-string`): `local:device`,
    /// or a category and its identifier, which is part of the value. Anything else is
    /// undeclared.
    pub fn parse(s: &str) -> Zone {
        let s = s.trim();
        if s == "local:device" {
            return Zone::LocalDevice;
        }
        match s.split_once(':') {
            Some((cat, id)) => match Category::parse(cat) {
                Some(cat) if identifier(id) => Zone::In(cat, id.to_string()),
                _ => Zone::Undeclared,
            },
            None => Zone::Undeclared,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Zone::LocalDevice => "local:device".into(),
            Zone::In(c, id) => format!("{}:{id}", c.name()),
            Zone::Undeclared => "undeclared".into(),
        }
    }
}

/// One allow-set entry (`authority.place.allow-set-entry`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Entry {
    Any,
    LocalDevice,
    Category(Category),
    Exact(Category, String),
}

impl Entry {
    /// Parse an entry; one matching no entry form refuses, naming it
    /// (`authority.place.unparsed-pattern`).
    pub fn parse(s: &str) -> Result<Entry, EnforceError> {
        let unparsed = || EnforceError::ZonePatternUnparsed(format!("allow-set entry `{s}` matches no entry form"));
        let t = s.trim();
        if t == "*" {
            return Ok(Entry::Any);
        }
        if t == "local:device" {
            return Ok(Entry::LocalDevice);
        }
        let (cat, id) = t.split_once(':').ok_or_else(unparsed)?;
        let cat = Category::parse(cat).ok_or_else(unparsed)?;
        match id {
            "*" => Ok(Entry::Category(cat)),
            id if identifier(id) => Ok(Entry::Exact(cat, id.to_string())),
            _ => Err(unparsed()),
        }
    }

    /// Whether this entry matches a zone. A bare category matches every identifier in
    /// it; an entry carrying an identifier matches that identifier alone; only `*`
    /// matches an undeclared zone (`authority.place.disjunctive`, `authority.place.undeclared`).
    pub fn matches(&self, zone: &Zone) -> bool {
        match (self, zone) {
            (Entry::Any, _) => true,
            (Entry::LocalDevice, Zone::LocalDevice) => true,
            (Entry::Category(c), Zone::In(z, _)) => c == z,
            (Entry::Exact(c, id), Zone::In(z, zid)) => c == z && id == zid,
            _ => false,
        }
    }

    /// Whether every zone this entry matches, `other` matches.
    fn within(&self, other: &Entry) -> bool {
        match (self, other) {
            (_, Entry::Any) => true,
            (Entry::Any, _) => false,
            (Entry::LocalDevice, Entry::LocalDevice) => true,
            (Entry::Category(a), Entry::Category(b)) => a == b,
            (Entry::Exact(a, _), Entry::Category(b)) => a == b,
            (Entry::Exact(a, x), Entry::Exact(b, y)) => a == b && x == y,
            _ => false,
        }
    }

    /// The entry matching exactly the zones both match, if any.
    fn meet(&self, other: &Entry) -> Option<Entry> {
        if self.within(other) {
            Some(self.clone())
        } else if other.within(self) {
            Some(other.clone())
        } else {
            None
        }
    }

    fn label(&self) -> String {
        match self {
            Entry::Any => "*".into(),
            Entry::LocalDevice => "local:device".into(),
            Entry::Category(c) => format!("{}:*", c.name()),
            Entry::Exact(c, id) => format!("{}:{id}", c.name()),
        }
    }
}

/// An allow-set: a disjunction of entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowSet(Vec<Entry>);

impl AllowSet {
    /// Parse a declared allow-set of at most 32 entries.
    pub fn parse(entries: &[String]) -> Result<AllowSet, PolicyError> {
        if entries.len() > ALLOW_SET_ENTRIES {
            return Err(PolicyError::Malformed(DeclarationMalformed(format!(
                "an allow-set holds {} entries; the bound is {ALLOW_SET_ENTRIES}",
                entries.len()
            ))));
        }
        Ok(AllowSet(entries.iter().map(|e| Entry::parse(e)).collect::<Result<_, _>>()?))
    }

    /// The fail-closed pair `local:device` and `on-prem:*` (`authority.place.fail-closed`).
    pub fn fail_closed() -> AllowSet {
        AllowSet(vec![Entry::LocalDevice, Entry::Category(Category::OnPrem)])
    }

    pub fn any() -> AllowSet {
        AllowSet(vec![Entry::Any])
    }

    /// A zone is admitted when any entry matches (`authority.place.disjunctive`).
    pub fn admits(&self, zone: &Zone) -> bool {
        self.0.iter().any(|e| e.matches(zone))
    }

    /// Whether every zone this set admits, `other` admits, decided over the entries'
    /// constructors and identifiers rather than by probing sample zones
    /// (`authority.place.symbolic-inclusion`).
    pub fn within(&self, other: &AllowSet) -> bool {
        self.0.iter().all(|e| other.0.iter().any(|o| e.within(o)))
    }

    /// The set admitting exactly the zones both admit.
    pub fn intersect(&self, other: &AllowSet) -> AllowSet {
        let mut out: Vec<Entry> = Vec::new();
        for a in &self.0 {
            for b in &other.0 {
                if let Some(m) = a.meet(b) {
                    if !out.contains(&m) {
                        out.push(m);
                    }
                }
            }
        }
        AllowSet(out)
    }

    pub fn labels(&self) -> Vec<String> {
        self.0.iter().map(Entry::label).collect()
    }
}

/// A surface's declared zone policy: its allow-set, whether it carries a protected
/// class, and whether the class-named override is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub declared: Option<AllowSet>,
    pub protected: bool,
    pub protected_override: bool,
}

impl Placement {
    /// The manifest check: a protected surface declaring a set wider than its floor
    /// without the override refuses (`authority.place.floor-widened`).
    pub fn check(&self, surface: &str) -> Result<(), EnforceError> {
        if let (true, false, Some(set)) = (self.protected, self.protected_override, &self.declared) {
            if !set.within(&AllowSet::fail_closed()) {
                return Err(EnforceError::ProtectedFloorWidened(format!(
                    "`{surface}` carries a protected class and declares [{}] past the floor [local:device, on-prem:*]",
                    set.labels().join(", ")
                )));
            }
        }
        Ok(())
    }

    /// The set served against: the declared set, or the fail-closed pair where none is
    /// declared (`authority.place.fail-closed`), resolved down to the protected floor
    /// where the surface carries a protected class and no override
    /// (`authority.place.protected-floor`).
    pub fn effective(&self) -> AllowSet {
        let set = self.declared.clone().unwrap_or_else(AllowSet::fail_closed);
        if self.protected && !self.protected_override {
            set.intersect(&AllowSet::fail_closed())
        } else {
            set
        }
    }
}

/// The set one cell serves under: the column's set narrows its table's, and the
/// authoring principal's narrows both (`authority.place.narrowest-grain`).
pub fn narrowest(table: &AllowSet, column: Option<&AllowSet>, principal: Option<&AllowSet>) -> AllowSet {
    let mut set = table.clone();
    for grain in [column, principal].into_iter().flatten() {
        set = set.intersect(grain);
    }
    set
}

/// The session zone a request serves under. The calling process declares its zone per
/// request, falling back to its credential's subject zone (`authority.place.caller-zone`).
/// Incognito pins the session to the fail-closed pair: no declared zone resolves to
/// `local:device`, and an asserted zone outside the pair refuses
/// (`authority.place.incognito`, `authority.place.incognito-widening`).
pub fn session_zone(asserted: Option<&str>, subject_zone: Option<&str>, incognito: bool) -> Result<Zone, EnforceError> {
    let declared = asserted.or(subject_zone).map(Zone::parse);
    if !incognito {
        return Ok(declared.unwrap_or(Zone::Undeclared));
    }
    match declared {
        None => Ok(Zone::LocalDevice),
        Some(z) if AllowSet::fail_closed().admits(&z) => Ok(z),
        Some(z) => Err(EnforceError::IncognitoWidening(format!(
            "the session is incognito and asserts `{}`, wider than [local:device, on-prem:*]",
            z.label()
        ))),
    }
}

/// A synthesized row's resolved set: at most the intersection of its evidence tables'
/// sets. A wider declaration refuses and resolves to that intersection
/// (`authority.place.evidence-floor`).
pub fn evidence_floor(declared: &AllowSet, evidence: &[AllowSet]) -> (AllowSet, Option<EnforceError>) {
    let floor = evidence.iter().skip(1).fold(evidence.first().cloned().unwrap_or_else(AllowSet::any), |acc, e| acc.intersect(e));
    if declared.within(&floor) {
        (declared.clone(), None)
    } else {
        let refusal = EnforceError::EvidenceFloorExceeded(format!(
            "the row declares [{}], wider than its evidence intersection [{}]",
            declared.labels().join(", "),
            floor.labels().join(", ")
        ));
        (floor, Some(refusal))
    }
}

/// Serving one row: whether it drops, and which columns arrive null by zone
/// (`authority.place.serve-outcome`). The row leaves the result where the table's set
/// omits the zone (`authority.place.excluded-row`); a cell whose set omits it arrives
/// null (`authority.place.excluded-cell`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServeOutcome {
    pub drop: bool,
    pub masked_columns: Vec<String>,
}

pub fn serve(table: &AllowSet, columns: &[(String, AllowSet)], zone: &Zone) -> ServeOutcome {
    if !table.admits(zone) {
        return ServeOutcome { drop: true, masked_columns: Vec::new() };
    }
    let masked_columns =
        columns.iter().filter(|(_, set)| !narrowest(table, Some(set), None).admits(zone)).map(|(c, _)| c.clone()).collect();
    ServeOutcome { drop: false, masked_columns }
}
