//! `read.guard` templates: the declaration, the startup checks over the engine's parse,
//! argument binding, and the tool each template projects into.

use super::error::ReadError;
use super::face::TOOL_PREFIXES;
use crate::store::declare::DeclarationMalformed;
use crate::time::Instant;
use serde::Deserialize;
use serde_json::{json, Map, Value};

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

/// Every template a manifest declares. A parameter is `name:type` over integer, float,
/// string, timestamp and boolean (`read.guard.template-declaration`); any other spelling
/// refuses the manifest.
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
    /// identifiers, or common table expressions the statement declares. Placeholders
    /// cover exactly the declared parameters (`read.guard.template-binding`). The check is
    /// caller-independent (`read.guard.startup-time-check`).
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
        if admitted.parameters != self.parameters.len() {
            return Err(ReadError::TemplateArgumentRejected(format!(
                "template `{}` carries {} placeholders and declares {} parameters",
                self.id,
                admitted.parameters,
                self.parameters.len()
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
                let mistyped = || reject(format!("`{}` is not a {}: {v}", p.name, p.ty.name()));
                Ok(match p.ty {
                    ParamType::Integer => Bound::Integer(v.as_i64().filter(|_| v.is_i64() || v.is_u64()).ok_or_else(mistyped)?),
                    ParamType::Float => Bound::Float(v.as_f64().filter(|_| v.is_number()).ok_or_else(mistyped)?),
                    ParamType::String => Bound::Text(v.as_str().ok_or_else(mistyped)?.to_string()),
                    ParamType::Timestamp => {
                        Bound::Timestamp(v.as_str().and_then(|s| Instant::parse(s).ok()).ok_or_else(mistyped)?)
                    }
                    ParamType::Boolean => Bound::Boolean(v.as_bool().ok_or_else(mistyped)?),
                })
            })
            .collect()
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
