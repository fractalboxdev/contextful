//! `disclosure.declare-fidelity` and `disclosure.bound-staleness`: a table's visibility
//! block, the fidelity level it claims, the family bounding that claim, and the grammar
//! of its staleness budget.

use super::VisibilityError;
use crate::store::declare::{DeclarationMalformed, TableDecl};
use serde::Deserialize;

/// The fidelity level a table claims for its source. `mirrored` and `coarse` are served
/// from the store; `federated` answers live under the reader's credential; `excluded` is
/// never served.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fidelity {
    Mirrored,
    Coarse,
    Federated,
    Excluded,
}

impl Fidelity {
    pub const ALL: [Fidelity; 4] = [Fidelity::Mirrored, Fidelity::Coarse, Fidelity::Federated, Fidelity::Excluded];

    pub fn as_str(self) -> &'static str {
        match self {
            Fidelity::Mirrored => "mirrored",
            Fidelity::Coarse => "coarse",
            Fidelity::Federated => "federated",
            Fidelity::Excluded => "excluded",
        }
    }

    /// Whether rows of a table at this level are served from the store.
    pub fn is_servable(self) -> bool {
        matches!(self, Fidelity::Mirrored | Fidelity::Coarse)
    }
}

/// The source family: the shape of a source's permission model, which bounds the
/// fidelity a table of that source may claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Family {
    /// A membership list on a container that its content inherits.
    ContainerRoster,
    /// A container base plus per-item exceptions to inheritance.
    ItemException,
    /// A container that is one person: a mailbox, a direct conversation.
    PersonContainer,
    /// Principals and group membership, joined into reach and never served as content.
    Directory,
}

impl Family {
    pub const ALL: [Family; 4] = [Family::ContainerRoster, Family::ItemException, Family::PersonContainer, Family::Directory];

    pub fn as_str(self) -> &'static str {
        match self {
            Family::ContainerRoster => "container-roster",
            Family::ItemException => "item-exception",
            Family::PersonContainer => "person-container",
            Family::Directory => "directory",
        }
    }

    /// Whether this family permits a table to claim `fidelity`: a `person-container`
    /// table holds `federated` or `excluded`, and a `directory` table holds `excluded`
    /// alone (`disclosure.declare-fidelity.family-bound`).
    pub fn permits(self, fidelity: Fidelity) -> bool {
        match self {
            Family::ContainerRoster | Family::ItemException => true,
            Family::PersonContainer => !fidelity.is_servable(),
            Family::Directory => fidelity == Fidelity::Excluded,
        }
    }
}

/// What a read of the table does once its source lag exceeds the budget: refuse, or
/// narrow to rows whose public status was re-confirmed within budget.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnStale {
    #[default]
    Refuse,
    PublicOnly,
}

/// A table's parsed `[pipeline.tables.visibility]` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub table: String,
    pub source: String,
    pub resource_key: String,
    pub resource_kind: Option<String>,
    pub fidelity: Fidelity,
    pub family: Family,
    /// The staleness budget in seconds, when declared.
    pub max_acl_staleness_secs: Option<u64>,
    pub on_stale: OnStale,
}

#[derive(Debug, thiserror::Error)]
pub enum DeclareError {
    #[error(transparent)]
    Visibility(#[from] VisibilityError),
    #[error(transparent)]
    Malformed(#[from] DeclarationMalformed),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBinding {
    source: String,
    resource_key: String,
    #[serde(default)]
    resource_kind: Option<String>,
    fidelity: Fidelity,
    #[serde(default)]
    family: Option<Family>,
    #[serde(default)]
    max_acl_staleness: Option<String>,
    #[serde(default)]
    on_stale: OnStale,
}

impl Binding {
    /// Read the visibility block of `table`, or `None` when it declares none. A block
    /// naming no family, a malformed budget, or a level the family does not permit is
    /// refused, and the table does not load.
    pub fn of(table: &TableDecl) -> Result<Option<Binding>, DeclareError> {
        let Some(block) = &table.visibility else { return Ok(None) };
        let raw: RawBinding = serde_json::from_value(block.clone())
            .map_err(|e| DeclarationMalformed(format!("table `{}`: visibility: {e}", table.name)))?;
        let family = raw.family.ok_or_else(|| VisibilityError::FamilyUndeclared { table: table.name.clone() })?;
        if !family.permits(raw.fidelity) {
            return Err(VisibilityError::FamilyBound {
                table: table.name.clone(),
                family: family.as_str(),
                fidelity: raw.fidelity.as_str(),
            }
            .into());
        }
        let max_acl_staleness_secs = raw.max_acl_staleness.as_deref().map(|text| parse_budget(&table.name, text)).transpose()?;
        Ok(Some(Binding {
            table: table.name.clone(),
            source: raw.source,
            resource_key: raw.resource_key,
            resource_kind: raw.resource_kind,
            fidelity: raw.fidelity,
            family,
            max_acl_staleness_secs,
            on_stale: raw.on_stale,
        }))
    }
}

/// Parse a staleness budget of `table`: ASCII digits naming a positive integer, then
/// exactly one suffix from `s`, `m`, `h`, `d`. Whitespace, a sign, a compound form, a
/// bare number, another suffix, and a figure past `u64` seconds all refuse
/// (`disclosure.bound-staleness.budget-grammar`).
pub fn parse_budget(table: &str, text: &str) -> Result<u64, VisibilityError> {
    let malformed = || VisibilityError::BudgetMalformed { table: table.to_string(), text: text.to_string() };
    let unit = match text.as_bytes().last() {
        Some(b's') => 1,
        Some(b'm') => 60,
        Some(b'h') => 3_600,
        Some(b'd') => 86_400,
        _ => return Err(malformed()),
    };
    let digits = &text[..text.len() - 1];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(malformed());
    }
    let n: u64 = digits.parse().map_err(|_| malformed())?;
    if n == 0 {
        return Err(malformed());
    }
    n.checked_mul(unit).ok_or_else(malformed)
}
