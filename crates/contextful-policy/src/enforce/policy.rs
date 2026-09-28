//! A table's declared policy — column masks, zone, row predicate and its exceptions,
//! published limits — read from the `policy` key of its declaration block and checked at
//! manifest load.

use super::mask::{ColumnPolicy, RawColumnPolicy, MASKS_PER_TABLE};
use super::predicate::Predicate;
use super::zone::{AllowSet, Placement};
use super::PolicyError;
use contextful_core::enforce::EnforceError;
use contextful_core::store::declare::{DeclarationMalformed, TableDecl};
use contextful_core::store::reconcile::Column;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPolicy {
    #[serde(default)]
    columns: BTreeMap<String, RawColumnPolicy>,
    #[serde(default)]
    zone: Option<RawZone>,
    #[serde(default)]
    rows: Option<RawRows>,
    #[serde(default)]
    redistribution: Option<RawRedistribution>,
    #[serde(default)]
    limits: Option<RawLimits>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawZone {
    allow: Vec<String>,
    /// Protected classes or columns whose floor this declaration lifts, each by name.
    #[serde(default)]
    protected_class_override: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRows {
    predicate: String,
    #[serde(default)]
    exception: Vec<RawException>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawException {
    when: String,
    predicate: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRedistribution {
    replicate: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLimits {
    #[serde(default)]
    max_rows: Option<u64>,
}

/// The table policy's row predicate and the exceptions overriding it for matching
/// subjects (`authority.filter-rows.exception`, `authority.filter-rows.override`).
#[derive(Debug, Clone, PartialEq)]
pub struct RowPolicy {
    pub predicate: Predicate,
    pub exceptions: Vec<(Predicate, Predicate)>,
}

impl RowPolicy {
    /// The compiled table-policy step: the first matching exception's predicate replaces
    /// the table predicate, and nothing else.
    pub fn sql(&self) -> String {
        if self.exceptions.is_empty() {
            return self.predicate.sql();
        }
        let arms: Vec<String> = self.exceptions.iter().map(|(when, p)| format!("WHEN {} THEN {}", when.sql(), p.sql())).collect();
        format!("(CASE {} ELSE {} END)", arms.join(" "), self.predicate.sql())
    }
}

/// One table's policy.
#[derive(Debug, Clone, PartialEq)]
pub struct TablePolicy {
    pub columns: BTreeMap<String, ColumnPolicy>,
    pub placement: Placement,
    /// The classes and columns the zone declaration's override names.
    pub overrides: Vec<String>,
    pub rows: Option<RowPolicy>,
    pub replicate: Option<bool>,
    /// The per-table row ceiling published as `limits.max_rows`.
    pub max_rows: Option<u64>,
}

impl TablePolicy {
    /// Read and check a declaration's `policy` key at manifest load: every mask, the zone
    /// allow-set and its protected floor, and every predicate parse here or refuse.
    pub fn from_decl(decl: &TableDecl) -> Result<TablePolicy, PolicyError> {
        let raw: RawPolicy = match &decl.policy {
            None => RawPolicy::default(),
            Some(v) => serde_json::from_value(v.clone())
                .map_err(|e| DeclarationMalformed(format!("table `{}`: policy: {e}", decl.name)))?,
        };
        if raw.columns.len() > MASKS_PER_TABLE {
            return Err(DeclarationMalformed(format!(
                "table `{}` declares {} column masks; the bound is {MASKS_PER_TABLE}",
                decl.name,
                raw.columns.len()
            ))
            .into());
        }
        let mut columns = BTreeMap::new();
        for (name, c) in raw.columns {
            let policy = ColumnPolicy::parse(&name, c)?;
            columns.insert(name, policy);
        }
        let table_class = match &decl.class {
            None => None,
            Some(Value::String(c)) => Some(super::mask::class(c)?),
            Some(other) => return Err(DeclarationMalformed(format!("table `{}`: class {other} is not a string", decl.name)).into()),
        };
        let (declared, overrides) = match raw.zone {
            Some(z) => (Some(AllowSet::parse(&z.allow)?), z.protected_class_override),
            None => (None, Vec::new()),
        };
        let protected_override = table_class.is_some_and(|c| overrides.iter().any(|o| o == c.name));
        let placement = Placement { declared, protected: table_class.is_some_and(|c| c.protected()), protected_override };
        placement.check(&format!("table {}", decl.name))?;
        let rows = raw
            .rows
            .map(|r| -> Result<RowPolicy, EnforceError> {
                let predicate = Predicate::parse(&r.predicate)?;
                let exceptions = r
                    .exception
                    .iter()
                    .map(|e| {
                        let when = Predicate::parse(&e.when)?;
                        if !when.subject_only() {
                            return Err(EnforceError::PredicateOutsideGrammar(format!(
                                "exception condition `{}` reads a column; it is over subject fields alone",
                                e.when
                            )));
                        }
                        Ok((when, Predicate::parse(&e.predicate)?))
                    })
                    .collect::<Result<_, _>>()?;
                Ok(RowPolicy { predicate, exceptions })
            })
            .transpose()?;
        Ok(TablePolicy {
            columns,
            placement,
            overrides,
            rows,
            replicate: raw.redistribution.map(|r| r.replicate),
            max_rows: raw.limits.and_then(|l| l.max_rows),
        })
    }

    /// Hold the policy to the table's schema: a mask naming a column the schema omits
    /// refuses (`authority.mask.absent-column`), as does a strategy the column's type
    /// does not admit (`authority.mask.typed-strategy`).
    pub fn check_schema(&self, table: &str, schema: &[Column]) -> Result<(), EnforceError> {
        for (name, c) in &self.columns {
            let Some(mask) = &c.mask else { continue };
            let Some(column) = schema.iter().find(|s| &s.name == name) else {
                return Err(EnforceError::MaskOnAbsentColumn(format!("table `{table}` masks `{name}`, which its schema omits")));
            };
            if !mask.admits(column.ty) {
                return Err(EnforceError::StrategyOutsideType(format!(
                    "table `{table}` masks `{name}`, a {} column, by `{}`; a binary column takes drop or hash, a vector column drop alone",
                    column.ty.name(),
                    mask.strategy_name()
                )));
            }
        }
        Ok(())
    }

    /// The set a column's cells serve under: the table's, narrowed to the protected floor
    /// for a protected-class column, which declares no set of its own and so resolves down
    /// at serve time unless the override names its class or the column itself
    /// (`authority.place.protected-floor`).
    pub fn column_set(&self, column: &str) -> AllowSet {
        let protected = self.columns.get(column).and_then(|c| c.class).is_some_and(|c| c.protected());
        let named = self.overrides.iter().any(|o| o == "phi" || o == column);
        let column = Placement {
            protected: protected || self.placement.protected,
            protected_override: if protected { named } else { self.placement.protected_override },
            ..self.placement.clone()
        };
        super::zone::narrowest(&self.placement.effective(), Some(&column.effective()), None)
    }
}
