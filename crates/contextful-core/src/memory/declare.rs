//! `read.declare`: the five memory table shapes, their canonical columns, and the
//! relation vocabulary.

use super::MemoryError;
use crate::store::declare::{DeclarationMalformed, TableDecl};
use serde::Deserialize;

/// A memory table shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Shape {
    Episodes,
    Facts,
    Entities,
    Edges,
    Preferences,
}

impl Shape {
    pub const ALL: [Shape; 5] = [Shape::Episodes, Shape::Facts, Shape::Entities, Shape::Edges, Shape::Preferences];

    pub fn name(self) -> &'static str {
        match self {
            Shape::Episodes => "memory_episodes",
            Shape::Facts => "memory_facts",
            Shape::Entities => "memory_entities",
            Shape::Edges => "memory_edges",
            Shape::Preferences => "memory_preferences",
        }
    }

    pub fn parse(s: &str) -> Option<Shape> {
        Shape::ALL.into_iter().find(|shape| shape.name() == s)
    }

    /// The columns every table of this shape carries. The first is the row's key.
    pub fn canonical_columns(self) -> &'static [&'static str] {
        match self {
            Shape::Episodes => &["episode_id", "source", "observed_at", "text", "evidence"],
            Shape::Facts => &[
                "claim_id", "subject", "predicate", "object", "scope", "tier", "confidence", "valid_from", "valid_to", "evidence",
                "superseded_by", "grant_id", "agent",
            ],
            Shape::Entities => &["entity_id", "kind", "name", "aliases"],
            Shape::Edges => &["edge_id", "rel_type", "source_id", "target_id", "evidence"],
            Shape::Preferences => &["preference_id", "subject", "key", "value", "scope"],
        }
    }

    pub fn key(self) -> &'static str {
        self.canonical_columns()[0]
    }
}

/// Relation types every deployment admits; a declaration adds its own to the union.
pub const RESERVED_RELATIONS: [&str; 5] = ["supports", "contradicts", "supersedes", "about", "derived_from"];

/// One memory table a manifest declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryTable {
    pub name: String,
    pub shape: Shape,
    pub columns: Vec<String>,
    /// A claims table's declared ranking half-life in seconds; `None` ranks without decay
    /// (`read.revise.retention-default`).
    pub decay_half_life_secs: Option<u64>,
    /// A claims table's settled-outcome table, whose `prediction_id` names a `claim_id`
    /// and whose `verdict` settles it (`read.recall.confidence-label`).
    pub labels: Option<String>,
}

impl MemoryTable {
    /// The table as the store declares it: keyed on its shape's key, so a later version of
    /// a row replaces the earlier one on read.
    pub fn table_decl(&self) -> TableDecl {
        TableDecl { primary_key: Some(vec![self.shape.key().to_string()]), ..TableDecl::named(&self.name) }
    }
}

/// A manifest's memory declarations: its shaped tables and declared relation types.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryDeclarations {
    pub tables: Vec<MemoryTable>,
    pub declared_relations: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTable {
    name: String,
    shape: String,
    columns: Vec<String>,
    #[serde(default)]
    decay_half_life: Option<String>,
    #[serde(default)]
    labels: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawRelation {
    #[serde(default)]
    declared: Vec<String>,
}

#[derive(Deserialize)]
struct RawManifest {
    #[serde(default)]
    table: Vec<RawTable>,
    #[serde(default)]
    relation: Option<RawRelation>,
}

/// A memory declaration refusal, or a declaration that does not parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeclareError {
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error(transparent)]
    Malformed(#[from] DeclarationMalformed),
}

impl MemoryDeclarations {
    /// Read the `[[table]]` and `[relation]` blocks of a manifest. A table naming a shape
    /// and omitting one of its canonical columns refuses when the declaration loads
    /// (`read.declare.canonical-column`); an unknown shape refuses as malformed.
    pub fn parse(manifest: &str) -> Result<MemoryDeclarations, DeclareError> {
        let value: toml::Value = toml::from_str(manifest).map_err(|e| DeclarationMalformed(e.to_string()))?;
        let raw: RawManifest = value.try_into().map_err(|e: toml::de::Error| DeclarationMalformed(e.to_string()))?;
        let mut tables = Vec::new();
        for t in raw.table {
            let shape = Shape::parse(&t.shape).ok_or_else(|| {
                DeclarationMalformed(format!(
                    "table `{}`: shape `{}` is not one of {}",
                    t.name,
                    t.shape,
                    Shape::ALL.map(Shape::name).join(", ")
                ))
            })?;
            let missing: Vec<&str> =
                shape.canonical_columns().iter().copied().filter(|c| !t.columns.iter().any(|d| d == c)).collect();
            if !missing.is_empty() {
                return Err(MemoryError::ShapeColumnMissing(format!(
                    "table `{}` names shape `{}` and omits {}",
                    t.name,
                    shape.name(),
                    missing.join(", ")
                ))
                .into());
            }
            let decay_half_life_secs = match t.decay_half_life.as_deref() {
                None => None,
                Some(_) if shape != Shape::Facts => {
                    return Err(DeclarationMalformed(format!("table `{}`: only a `{}` table declares `decay_half_life`", t.name, Shape::Facts.name())).into())
                }
                Some(h) => match crate::time::duration_secs(h) {
                    Some(secs) if secs > 0 => Some(secs),
                    _ => return Err(DeclarationMalformed(format!("table `{}`: `decay_half_life = \"{h}\"` is no positive span such as `30d`", t.name)).into()),
                },
            };
            if t.labels.is_some() && shape != Shape::Facts {
                return Err(DeclarationMalformed(format!("table `{}`: only a `{}` table declares `labels`", t.name, Shape::Facts.name())).into());
            }
            tables.push(MemoryTable { name: t.name, shape, columns: t.columns, decay_half_life_secs, labels: t.labels });
        }
        Ok(MemoryDeclarations { tables, declared_relations: raw.relation.unwrap_or_default().declared })
    }

    pub fn table(&self, name: &str) -> Option<&MemoryTable> {
        self.tables.iter().find(|t| t.name == name)
    }

    /// The first declared table of a shape.
    pub fn of_shape(&self, shape: Shape) -> Option<&MemoryTable> {
        self.tables.iter().find(|t| t.shape == shape)
    }

    /// Whether a relation type is in the union of the reserved core and the declared types.
    pub fn admits_relation(&self, rel_type: &str) -> bool {
        RESERVED_RELATIONS.contains(&rel_type) || self.declared_relations.iter().any(|d| d == rel_type)
    }
}
