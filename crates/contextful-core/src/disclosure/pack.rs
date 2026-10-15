//! `disclosure.pack`: the `[pack.<name>]` block that lands one source — the tables it lands,
//! the access mapping of each, the tables it excludes, and its container allowlist.
//!
//! [`Pack::parse_manifest`] is the load: a key asserting access outside the mapping schema,
//! an any-of allowlist, and `coarse` with no container signal refuse there.
//! [`Pack::diagnose`] refuses a landed table with neither a mapping nor an exclusion.

use super::declare::{Family, Fidelity};
use super::VisibilityError;
use crate::pipeline::declare::ManifestFile;
use crate::store::declare::DeclarationMalformed;
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error(transparent)]
    Visibility(#[from] VisibilityError),
    #[error(transparent)]
    Malformed(#[from] DeclarationMalformed),
}

/// The keys an access mapping holds (`disclosure.pack.mapping-keys`).
pub const MAPPING_KEYS: [&str; 7] = ["resource_key", "resource_kind", "family", "fidelity", "container_key", "principal_kind", "level"];

/// The keys a pack block holds beside its fetch inputs (`disclosure.pack.pack-block`).
pub const PACK_KEYS: [&str; 4] = ["tables", "excluded", "access", "allowlist"];

/// The name roots that make a key access-shaped: a key naming who reads, shares or owns a
/// resource (`disclosure.pack.asserts-access`). A segment matches a root as written or with
/// one trailing `s`.
pub const ACCESS_SEGMENTS: [&str; 27] = [
    "access", "acl", "admin", "allow", "anyone", "audience", "collaborator", "deny", "editor", "everyone", "grant", "guest", "invitee", "member", "owner",
    "permission", "private", "public", "reader", "role", "share", "shared", "sharing", "viewer", "visibility", "visible", "writer",
];

/// The lowercased segments of `key`: split at `_`, `-` and each lowercase-to-uppercase
/// boundary, so `shared_with`, `shared-with` and `sharedWith` read alike.
fn segments(key: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in key.chars() {
        if c == '_' || c == '-' {
            out.push(std::mem::take(&mut cur));
            prev_lower = false;
            continue;
        }
        if c.is_ascii_uppercase() && prev_lower {
            out.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        cur.push(c.to_ascii_lowercase());
    }
    out.push(cur);
    out.retain(|s| !s.is_empty());
    out
}

/// Whether `key` is access-shaped: one of its segments, as [`segments`] splits it, is an
/// [`ACCESS_SEGMENTS`] root, as written or with one trailing `s`.
pub fn access_shaped(key: &str) -> bool {
    segments(key).iter().any(|seg| ACCESS_SEGMENTS.contains(&seg.as_str()) || seg.strip_suffix('s').is_some_and(|root| ACCESS_SEGMENTS.contains(&root)))
}

/// How an allowlist composes its entries with the access mapping.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub enum Evaluate {
    /// A row is admitted only when every condition holds.
    #[default]
    #[serde(rename = "all-of")]
    AllOf,
    /// A row is admitted when any one entry admits it; refused at load
    /// (`disclosure.pack.any-of-allowlist`).
    #[serde(rename = "any-of")]
    AnyOf,
}

/// A pack's container allowlist.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Allowlist {
    pub containers: Vec<String>,
    #[serde(default)]
    pub evaluate: Evaluate,
}

/// One table's access mapping (`disclosure.pack.mapping-keys`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapping {
    pub resource_key: String,
    pub resource_kind: Option<String>,
    pub family: Family,
    pub fidelity: Option<Fidelity>,
    /// The column carrying each row's container or workspace, the signal `coarse` reads.
    pub container_key: Option<String>,
    /// Source principal kinds onto the access tables' kinds.
    pub principal_kind: BTreeMap<String, String>,
    /// Source levels onto read, write and admin.
    pub level: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMapping {
    resource_key: String,
    #[serde(default)]
    resource_kind: Option<String>,
    #[serde(default)]
    family: Option<Family>,
    #[serde(default)]
    fidelity: Option<Fidelity>,
    #[serde(default)]
    container_key: Option<String>,
    #[serde(default)]
    principal_kind: BTreeMap<String, String>,
    #[serde(default)]
    level: BTreeMap<String, String>,
}

/// One parsed `[pack.<name>]` block (`disclosure.pack.pack-block`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pack {
    pub name: String,
    /// The tables the pack lands.
    pub tables: Vec<String>,
    /// The landed tables it declares excluded, carrying no mapping.
    pub excluded: Vec<String>,
    pub access: BTreeMap<String, Mapping>,
    pub allowlist: Option<Allowlist>,
}

fn strings(pack: &str, key: &str, v: Option<&toml::Value>) -> Result<Vec<String>, DeclarationMalformed> {
    let Some(v) = v else { return Ok(Vec::new()) };
    let bad = || DeclarationMalformed(format!("pack `{pack}`: `{key}` is an array of table names"));
    v.as_array().ok_or_else(bad)?.iter().map(|e| e.as_str().map(str::to_string).ok_or_else(bad)).collect()
}

/// Refuse the first access-shaped key at or under `value`, a fetch input of `pack`
/// (`disclosure.pack.asserts-access`).
fn refuse_access(pack: &str, value: &toml::Value) -> Result<(), VisibilityError> {
    match value {
        toml::Value::Table(t) => {
            for (k, v) in t {
                if access_shaped(k) {
                    return Err(VisibilityError::PackAssertsAccess { pack: pack.into(), key: k.clone() });
                }
                refuse_access(pack, v)?;
            }
            Ok(())
        }
        toml::Value::Array(items) => items.iter().try_for_each(|v| refuse_access(pack, v)),
        _ => Ok(()),
    }
}

impl Pack {
    /// Every `[pack.<name>]` block `manifest` declares, each loaded: a key asserting access
    /// outside the mapping schema, an any-of allowlist, a mapping naming no family, and
    /// `coarse` with no `container_key` refuse.
    pub fn parse_manifest(manifest: &str) -> Result<Vec<Pack>, PackError> {
        let value: toml::Value = toml::from_str(manifest).map_err(|e| DeclarationMalformed(e.to_string()))?;
        let Some(packs) = value.get("pack") else { return Ok(Vec::new()) };
        let packs = packs.as_table().ok_or_else(|| DeclarationMalformed("`pack` is a table of `[pack.<name>]` blocks".into()))?;
        let mut out = Vec::new();
        for (name, block) in packs {
            let block = block.as_table().ok_or_else(|| DeclarationMalformed(format!("pack `{name}` is a table, `[pack.{name}]`")))?;
            for (k, v) in block.iter().filter(|(k, _)| !PACK_KEYS.contains(&k.as_str())) {
                if access_shaped(k) {
                    return Err(VisibilityError::PackAssertsAccess { pack: name.clone(), key: k.clone() }.into());
                }
                refuse_access(name, v)?;
            }
            let tables = strings(name, "tables", block.get("tables"))?;
            let excluded = strings(name, "excluded", block.get("excluded"))?;
            let allowlist = match block.get("allowlist") {
                None => None,
                Some(v) => {
                    let a: Allowlist = v.clone().try_into().map_err(|e: toml::de::Error| DeclarationMalformed(format!("pack `{name}`: allowlist: {e}")))?;
                    if a.evaluate == Evaluate::AnyOf {
                        return Err(VisibilityError::AllowlistAnyOf { pack: name.clone() }.into());
                    }
                    Some(a)
                }
            };
            let mut access = BTreeMap::new();
            if let Some(a) = block.get("access") {
                let a = a.as_table().ok_or_else(|| DeclarationMalformed(format!("pack `{name}`: `access` is a table of `access.<table>` mappings")))?;
                for (table, m) in a {
                    access.insert(table.clone(), Self::mapping(name, table, m)?);
                }
            }
            for t in access.keys().chain(&excluded) {
                if !tables.contains(t) {
                    return Err(DeclarationMalformed(format!("pack `{name}`: `{t}` is mapped or excluded and absent from `tables`")).into());
                }
            }
            if let Some(t) = excluded.iter().find(|t| access.contains_key(*t)) {
                return Err(DeclarationMalformed(format!("pack `{name}`: table `{t}` is both mapped and excluded")).into());
            }
            out.push(Pack { name: name.clone(), tables, excluded, access, allowlist });
        }
        Ok(out)
    }

    fn mapping(pack: &str, table: &str, value: &toml::Value) -> Result<Mapping, PackError> {
        let t = value.as_table().ok_or_else(|| DeclarationMalformed(format!("pack `{pack}`: access `{table}` is a table")))?;
        if let Some(k) = t.keys().find(|k| !MAPPING_KEYS.contains(&k.as_str()) && access_shaped(k)) {
            return Err(VisibilityError::PackAssertsAccess { pack: pack.into(), key: k.clone() }.into());
        }
        let raw: RawMapping = value.clone().try_into().map_err(|e: toml::de::Error| DeclarationMalformed(format!("pack `{pack}`: access `{table}`: {e}")))?;
        let family = raw.family.ok_or_else(|| VisibilityError::FamilyUndeclared { table: table.into() })?;
        if raw.fidelity == Some(Fidelity::Coarse) && raw.container_key.is_none() {
            return Err(VisibilityError::UnsupportedCoarse { pack: pack.into(), table: table.into() }.into());
        }
        Ok(Mapping {
            resource_key: raw.resource_key,
            resource_kind: raw.resource_kind,
            family,
            fidelity: raw.fidelity,
            container_key: raw.container_key,
            principal_kind: raw.principal_kind,
            level: raw.level,
        })
    }

    /// Every pack across a declaration set's `.toml` files; a pack named in two files refuses.
    pub fn parse_set(files: &[ManifestFile]) -> Result<Vec<Pack>, PackError> {
        let mut out: Vec<Pack> = Vec::new();
        let mut homes: BTreeMap<String, &str> = BTreeMap::new();
        for f in files.iter().filter(|f| f.path.ends_with(".toml")) {
            let packs = Pack::parse_manifest(&f.text).map_err(|e| match e {
                PackError::Malformed(m) => PackError::Malformed(DeclarationMalformed(format!("{}: {}", f.path, m.0))),
                other => other,
            })?;
            for p in packs {
                if let Some(first) = homes.insert(p.name.clone(), &f.path) {
                    return Err(DeclarationMalformed(format!("{}: pack `{}` is declared a second time (first in {first})", f.path, p.name)).into());
                }
                out.push(p);
            }
        }
        Ok(out)
    }

    /// Refuse the first landed table carrying neither an access mapping nor an exclusion
    /// (`disclosure.pack.mapping-absent`).
    pub fn diagnose(&self) -> Result<(), VisibilityError> {
        match self.tables.iter().find(|t| !self.access.contains_key(*t) && !self.excluded.contains(t)) {
            Some(t) => Err(VisibilityError::MappingAbsent { pack: self.name.clone(), table: t.clone() }),
            None => Ok(()),
        }
    }
}
