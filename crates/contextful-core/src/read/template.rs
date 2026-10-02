//! `read.guard` templates: the declaration, the startup checks over the engine's parse,
//! argument binding, and the tool each template projects into.

use super::error::ReadError;
use super::face::TOOL_PREFIXES;
use crate::store::declare::DeclarationMalformed;
use crate::time::Instant;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// A declared parameter's type (`read.guard.template-declaration`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamType {
    Integer,
    Float,
    String,
    Timestamp,
    Boolean,
}

impl ParamType {
    /// Every type a parameter declares, by the name a declaration spells it.
    pub const NAMES: [&'static str; 5] = ["integer", "float", "string", "timestamp", "boolean"];

    fn parse(s: &str) -> Option<ParamType> {
        Some(match s {
            "integer" => ParamType::Integer,
            "float" => ParamType::Float,
            "string" => ParamType::String,
            "timestamp" => ParamType::Timestamp,
            "boolean" => ParamType::Boolean,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            ParamType::Integer => "integer",
            ParamType::Float => "float",
            ParamType::String => "string",
            ParamType::Timestamp => "timestamp",
            ParamType::Boolean => "boolean",
        }
    }

    fn schema(self) -> Value {
        match self {
            ParamType::Integer => json!({ "type": "integer" }),
            ParamType::Float => json!({ "type": "number" }),
            ParamType::String => json!({ "type": "string" }),
            ParamType::Timestamp => json!({ "type": "string", "format": "date-time" }),
            ParamType::Boolean => json!({ "type": "boolean" }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    pub name: String,
    pub ty: ParamType,
}

/// One `[[query_templates]]` entry: an identifier, one SQL statement, positional
/// parameters and an optional row ceiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryTemplate {
    pub id: String,
    pub sql: String,
    pub parameters: Vec<Parameter>,
    pub max_rows: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTemplate {
    id: String,
    sql: String,
    #[serde(default)]
    parameters: Vec<String>,
    #[serde(default)]
    max_rows: Option<u64>,
}

#[derive(Deserialize)]
struct ManifestTemplates {
    #[serde(default)]
    query_templates: Vec<RawTemplate>,
}

/// The read arguments every template tool admits beside its own parameters, so no
/// parameter takes their names (`read.guard.template-reserved-parameter`).
pub const READ_ARGUMENTS: [&str; 4] = ["as_of", "valid_as_of", "zone", "pin"];

/// Every template a manifest declares. A parameter is `name:type` over integer, float,
/// string, timestamp and boolean (`read.guard.template-declaration`); any other spelling
/// refuses the manifest.
/// A parameter named for a read argument refuses it too.
pub fn parse_templates(toml_text: &str) -> Result<Vec<QueryTemplate>, DeclarationMalformed> {
    let value: toml::Value = toml::from_str(toml_text).map_err(|e| DeclarationMalformed(e.to_string()))?;
    let raw: ManifestTemplates = value.try_into().map_err(|e: toml::de::Error| DeclarationMalformed(e.to_string()))?;
    raw.query_templates
        .into_iter()
        .map(|t| {
            let parameters = t
                .parameters
                .iter()
                .map(|p| {
                    let (name, ty) = p.split_once(':').unwrap_or((p, ""));
                    if READ_ARGUMENTS.contains(&name) {
                        return Err(DeclarationMalformed(format!(
                            "template `{}`: parameter `{name}` is a read argument every template tool admits",
                            t.id
                        )));
                    }
                    match ParamType::parse(ty) {
                        Some(ty) if !name.is_empty() => Ok(Parameter { name: name.to_string(), ty }),
                        _ => Err(DeclarationMalformed(format!(
                            "template `{}`: parameter `{p}` is not name:type over integer, float, string, timestamp, boolean",
                            t.id
                        ))),
                    }
                })
                .collect::<Result<_, _>>()?;
            Ok(QueryTemplate { id: t.id, sql: t.sql, parameters, max_rows: t.max_rows })
        })
        .collect()
}

impl QueryTemplate {
    /// The startup check over the engine's serialization of the template's SQL
    /// (`read.guard.template-relation-shape`): an identifier clear of every built-in tool
    /// prefix, and base relations that are the store's own tables named as plain
    /// identifiers, or common table expressions the statement declares. Placeholders are
    /// `$1`…`$n` or `?` in declaration order, or exactly the declared names
    /// (`read.guard.template-binding`). The check is caller-independent (`read.guard.startup-time-check`).
    pub fn check(&self, serialized: &Value, store_tables: &[String]) -> Result<(), ReadError> {
        if let Some(prefix) = TOOL_PREFIXES.iter().find(|p| self.id.starts_with(*p)) {
            return Err(ReadError::TemplateNamesForeignRelation(format!(
                "template `{}` collides with the built-in tool prefix `{prefix}`",
                self.id
            )));
        }
        let foreign = |what: String| ReadError::TemplateNamesForeignRelation(format!("template `{}` names {what}", self.id));
        let admitted = super::guard::admit(serialized, |name| store_tables.iter().any(|t| t == name)).map_err(|e| match e {
            super::error::Refusal::Read(ReadError::TableFunctionRefused(why)) => foreign(why),
            other => foreign(other.to_string()),
        })?;
        let names: BTreeSet<String> = self.parameters.iter().map(|p| p.name.clone()).collect();
        let numbered: BTreeSet<String> = (1..=self.parameters.len()).map(|i| i.to_string()).collect();
        if admitted.placeholders != numbered && admitted.placeholders != names {
            let list = |s: &BTreeSet<String>| s.iter().map(|p| format!("`{p}`")).collect::<Vec<_>>().join(", ");
            return Err(ReadError::TemplateArgumentRejected(format!(
                "template `{}` carries placeholders [{}] and declares parameters [{}]; placeholders are `$1`…`$n` or `?` in declaration order, or the declared names",
                self.id,
                list(&admitted.placeholders),
                list(&names)
            )));
        }
        Ok(())
    }

    /// The tool this template projects into: named by its identifier, every declared
    /// parameter a required field of a typed schema (`read.guard.template-projection`
    /// through `read.register.template-projection`), with `limits` listing the row
    /// ceiling where one is declared.
    pub fn tool(&self) -> Value {
        let properties: Map<String, Value> = self.parameters.iter().map(|p| (p.name.clone(), p.ty.schema())).collect();
        let required: Vec<&str> = self.parameters.iter().map(|p| p.name.as_str()).collect();
        let mut tool = json!({
            "name": self.id,
            "inputSchema": { "type": "object", "properties": properties, "required": required, "additionalProperties": false },
        });
        if let Some(max) = self.max_rows {
            tool["limits"] = json!({ "max_rows": max });
        }
        tool
    }

    /// Bind arguments to the declared parameters in order. A missing, unknown or
    /// mistyped argument refuses ahead of execution, with no coercion
    /// (`read.guard.template-binding`).
    pub fn bind(&self, arguments: &Map<String, Value>) -> Result<Vec<Bound>, ReadError> {
        let reject = |why: String| ReadError::TemplateArgumentRejected(format!("template `{}`: {why}", self.id));
        if let Some(unknown) = arguments.keys().find(|k| !self.parameters.iter().any(|p| &p.name == *k)) {
            return Err(reject(format!("`{unknown}` is not a declared parameter")));
        }
        self.parameters
            .iter()
            .map(|p| {
                let v = arguments.get(&p.name).ok_or_else(|| reject(format!("`{}` is missing", p.name)))?;
                p.ty.bind(v).ok_or_else(|| reject(format!("`{}` is not a {}: {v}", p.name, p.ty.name())))
            })
            .collect()
    }

    /// Key bound `values`, in declaration order, to the placeholders the startup check
    /// admitted: under each declared name where the statement spells its placeholders by
    /// name, else under `1`…`n` in declaration order.
    pub fn bindings(&self, values: Vec<Bound>, placeholders: &BTreeSet<String>) -> Bindings {
        if self.parameters.iter().all(|p| placeholders.contains(&p.name)) {
            Bindings(self.parameters.iter().map(|p| p.name.clone()).zip(values).collect())
        } else {
            Bindings::positional(values)
        }
    }
}

impl ParamType {
    /// `v` as a value of this type, with no coercion: a numeral string is no integer, an
    /// integral float is no integer, `1` is no boolean.
    fn bind(self, v: &Value) -> Option<Bound> {
        Some(match self {
            ParamType::Integer => Bound::Integer(v.as_i64().filter(|_| v.is_i64() || v.is_u64())?),
            ParamType::Float => Bound::Float(v.as_f64().filter(|_| v.is_number())?),
            ParamType::String => Bound::Text(v.as_str()?.to_string()),
            ParamType::Timestamp => Bound::Timestamp(v.as_str().and_then(|s| Instant::parse(s).ok())?),
            ParamType::Boolean => Bound::Boolean(v.as_bool()?),
        })
    }
}

/// Bind `context.query` parameters to the placeholders of an admitted statement
/// (`read.guard.query-binding`). Each parameter is a name mapped to `{type, value}` over
/// the template types; a placeholder with no parameter, a parameter with no placeholder,
/// an unknown type or a mismatched value raises `QueryParameterRejected` with no coercion.
pub fn bind_query(parameters: &Map<String, Value>, placeholders: &BTreeSet<String>) -> Result<Bindings, ReadError> {
    let reject = |why: String| ReadError::QueryParameterRejected(why);
    let last = placeholders.iter().filter_map(|p| p.parse::<u64>().ok()).max().unwrap_or(0);
    if let Some(gap) = (1..=last).find(|i| !placeholders.contains(&i.to_string())) {
        return Err(reject(format!("placeholder `{gap}` has no parameter: numbered placeholders run from `1` without a gap")));
    }
    if let Some(missing) = placeholders.iter().find(|p| !parameters.contains_key(*p)) {
        return Err(reject(format!("placeholder `{missing}` has no parameter")));
    }
    let mut bound = BTreeMap::new();
    for (name, spec) in parameters {
        let field = spec.as_object().ok_or_else(|| reject(format!("parameter `{name}` is not an object of `type` and `value`")))?;
        if let Some(extra) = field.keys().find(|k| !["type", "value"].contains(&k.as_str())) {
            return Err(reject(format!("parameter `{name}` carries the unknown field `{extra}`")));
        }
        let ty = match field.get("type") {
            Some(Value::String(t)) => ParamType::parse(t).ok_or_else(|| {
                reject(format!("parameter `{name}` has type `{t}`, not one of {}", ParamType::NAMES.join(", ")))
            })?,
            _ => return Err(reject(format!("parameter `{name}` carries no `type`"))),
        };
        let value = field.get("value").ok_or_else(|| reject(format!("parameter `{name}` carries no `value`")))?;
        let value = ty.bind(value).ok_or_else(|| reject(format!("parameter `{name}` is not a {}: {value}", ty.name())))?;
        if !placeholders.contains(name) {
            return Err(reject(format!("parameter `{name}` names no placeholder of the statement")));
        }
        bound.insert(name.clone(), value);
    }
    Ok(Bindings(bound))
}

/// Values bound to a statement's placeholders, each under the identifier the engine's
/// parse gives its placeholder: `1`, `2`, … for a positional `?`, the name for `$name`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Bindings(BTreeMap<String, Bound>);

impl Bindings {
    /// Values for positional placeholders, the first under identifier `1`.
    pub fn positional(values: impl IntoIterator<Item = Bound>) -> Bindings {
        Bindings(values.into_iter().enumerate().map(|(i, v)| ((i + 1).to_string(), v)).collect())
    }

    /// The value bound to the placeholder `identifier`.
    pub fn get(&self, identifier: &str) -> Option<&Bound> {
        self.0.get(identifier)
    }
}

/// An argument bound to its declared type.
#[derive(Debug, Clone, PartialEq)]
pub enum Bound {
    Integer(i64),
    Float(f64),
    Text(String),
    Timestamp(Instant),
    Boolean(bool),
}
