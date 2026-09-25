//! `connector.reference`: the `secret://` scheme, the value-template grammar, and the
//! wrapper every hydrated value rides.

use super::ConnectorError;
use crate::pipeline::guard;

/// Longest logical credential name: 128 chars (`connector.reference.reference-scheme`).
pub const NAME_MAX: usize = 128;
pub const SCHEME: &str = "secret://";

/// A logical credential name: `[a-z0-9-]+`, at most 128 chars.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SecretName(String);

impl SecretName {
    pub fn parse(s: &str) -> Result<SecretName, ConnectorError> {
        if !s.is_empty() && s.len() <= NAME_MAX && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
            Ok(SecretName(s.to_string()))
        } else {
            Err(ConnectorError::SecretMalformedTemplate(format!(
                "`{s}` is not a logical name: 1 to {NAME_MAX} chars of `[a-z0-9-]`"
            )))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SecretName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A piece of a value template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    Literal(String),
    Secret(SecretName),
}

/// A value of literal text with `${secret://<name>}` placeholders (`connector.reference.value-template`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    pub parts: Vec<Part>,
}

impl Template {
    /// Parse `value`. A placeholder in another scheme or none refuses as foreign; an
    /// unclosed, empty or nested one as malformed. Both fire while the declaration is read.
    pub fn parse(value: &str) -> Result<Template, ConnectorError> {
        let mut parts = Vec::new();
        let mut rest = value;
        while let Some(open) = rest.find("${") {
            if open > 0 {
                parts.push(Part::Literal(rest[..open].to_string()));
            }
            let after = &rest[open + 2..];
            let close = after.find('}').ok_or_else(|| {
                ConnectorError::SecretMalformedTemplate(format!("`{value}`: the placeholder at byte {} is unclosed", value.len() - rest.len() + open))
            })?;
            let inner = &after[..close];
            let span = format!("${{{inner}}}");
            if inner.contains("${") {
                return Err(ConnectorError::SecretMalformedTemplate(format!("`{value}`: `{span}` nests a placeholder")));
            }
            if inner.trim().is_empty() {
                return Err(ConnectorError::SecretMalformedTemplate(format!("`{value}`: `{span}` is empty")));
            }
            let Some(name) = inner.strip_prefix(SCHEME) else {
                return Err(ConnectorError::SecretForeignPlaceholder(format!(
                    "`{value}`: `{span}` is not a `${{secret://<name>}}` reference; environment material binds as `env://NAME` outside a template"
                )));
            };
            parts.push(Part::Secret(SecretName::parse(name)?));
            rest = &after[close + 1..];
        }
        if !rest.is_empty() {
            parts.push(Part::Literal(rest.to_string()));
        }
        Ok(Template { parts })
    }

    /// The names this template references.
    pub fn names(&self) -> impl Iterator<Item = &SecretName> {
        self.parts.iter().filter_map(|p| match p {
            Part::Secret(n) => Some(n),
            Part::Literal(_) => None,
        })
    }

    pub fn has_reference(&self) -> bool {
        self.names().next().is_some()
    }

    /// Fill the template, each name answered by `hydrate`.
    pub fn render<E>(&self, mut hydrate: impl FnMut(&SecretName) -> Result<Hydrated, E>) -> Result<Hydrated, E> {
        let mut out = String::new();
        for p in &self.parts {
            match p {
                Part::Literal(s) => out.push_str(s),
                Part::Secret(n) => out.push_str(hydrate(n)?.reveal()),
            }
        }
        Ok(Hydrated::new(out))
    }
}

/// Refuse a credential-shaped literal standing where a reference belongs
/// (`connector.reference.material-in-a-declaration`): a literal part of `value` holding
/// a credential shape, or a `Bearer`/`Basic` scheme followed by literal material.
pub fn check_material(key: &str, value: &str) -> Result<Template, ConnectorError> {
    let t = Template::parse(value)?;
    for p in &t.parts {
        if let Part::Literal(s) = p {
            let lower = s.trim_start().to_ascii_lowercase();
            let scheme_literal = ["bearer ", "basic ", "token "].iter().any(|sch| lower.starts_with(sch) && s.trim().len() > sch.len() + 7);
            if !guard::spans(s).is_empty() || scheme_literal {
                return Err(ConnectorError::SecretMaterialInDeclaration(format!(
                    "`{key}` holds credential material; bind it as `${{secret://<name>}}`"
                )));
            }
        }
    }
    Ok(t)
}

/// The sentinel a hydrated value prints as.
pub const SENTINEL: &str = "[secret]";

/// A hydrated value. Its debug and display forms print [`SENTINEL`]; the bytes are
/// reached only through [`Hydrated::reveal`], where the host writes the request.
#[derive(Clone, PartialEq, Eq)]
pub struct Hydrated(String);

impl Hydrated {
    pub fn new(value: impl Into<String>) -> Hydrated {
        Hydrated(value.into())
    }

    pub fn reveal(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Hydrated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(SENTINEL)
    }
}

impl std::fmt::Display for Hydrated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(SENTINEL)
    }
}
