//! The decode keys the HTTP and object sources share: how a body is compressed, the zip
//! member it lands, and how delimited text splits (`connector.source.object-gzip`,
//! `connector.source.delimiter`, `connector.source.declared-columns`).

use crate::decode::{gunzip, unzip, Delimited, Format};
use crate::http::ConfigError;
use contextful_core::connector::ConnectorError;
use contextful_core::run::{Failure, RunError};
use serde_json::{Map, Value};
use std::borrow::Cow;

/// Decompressed bytes of one object or body: 256 MiB (`connector.source.object-expansion`).
pub const EXPANSION_CEILING: u64 = 256 * 1024 * 1024;

/// How a body's bytes are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// Gzip where the key ends `.gz`, zip where it ends `.zip`, as stored otherwise.
    ByKey,
    Gzip,
    Zip,
    None,
}

/// The shared decode keys one source declares.
#[derive(Debug, Clone)]
pub struct BodyKeys {
    pub compression: Compression,
    /// The zip member a read lands; absent, the archive holds exactly one.
    pub member: Option<String>,
    pub delimited: Delimited,
}

/// A string key of `source`'s configuration; any other JSON value refuses.
pub(crate) fn text(source: &str, cfg: &Map<String, Value>, k: &str) -> Result<Option<String>, ConfigError> {
    match cfg.get(k) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(RunError::Invalid(format!("the `{source}` source key `{k}` is a string, found {other}")).into()),
    }
}

/// `name` without `ext`, the extension compared without case (`connector.source.body-format`).
pub(crate) fn strip_ext<'a>(name: &'a str, ext: &str) -> Option<&'a str> {
    let at = name.len().checked_sub(ext.len())?;
    (name.is_char_boundary(at) && name[at..].eq_ignore_ascii_case(ext)).then(|| &name[..at])
}

impl BodyKeys {
    /// Parse `compression`, `member`, `delimiter` and `columns` for a source reading
    /// `format`; `undeclared` is the compression a source declaring none reads under.
    pub fn parse(source: &str, cfg: &Map<String, Value>, format: Format, undeclared: Compression) -> Result<BodyKeys, ConfigError> {
        let invalid = |why: String| -> ConfigError { RunError::Invalid(format!("the `{source}` source {why}")).into() };
        let compression = match text(source, cfg, "compression")?.as_deref() {
            None => undeclared,
            Some("gzip") => Compression::Gzip,
            Some("zip") => Compression::Zip,
            Some("none") => Compression::None,
            Some(other) => return Err(invalid(format!("`compression = \"{other}\"` is none of `gzip`, `zip` and `none`"))),
        };
        let member = text(source, cfg, "member")?;
        if member.is_some() && !matches!(compression, Compression::Zip | Compression::ByKey) {
            return Err(invalid("declares `member`, which selects inside a zip archive, and its compression is not `zip`".into()));
        }
        if member.as_deref() == Some("") {
            return Err(invalid("declares an empty `member`".into()));
        }
        let delimiter = text(source, cfg, "delimiter")?;
        let columns = cfg.get("columns");
        if format != Format::Csv {
            if let Some(k) = [delimiter.as_ref().map(|_| "delimiter"), columns.map(|_| "columns")].into_iter().flatten().next() {
                return Err(ConnectorError::ConnectorFormatKeyRejected(format!("`{k}` reads delimited text and the source's format is `{}`", format.name())).into());
            }
        }
        let delimiter = match delimiter {
            None => ',',
            Some(d) => {
                let mut chars = d.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if !matches!(c, '"' | '\n' | '\r') => c,
                    _ => return Err(invalid(format!("`delimiter = {d:?}` is not one character other than a quote or a line break"))),
                }
            }
        };
        let columns = match columns {
            None => None,
            Some(v) => {
                let names = v.as_array().filter(|a| !a.is_empty()).ok_or_else(|| invalid(format!("`columns` is a non-empty array of column names, found {v}")))?;
                let mut out: Vec<String> = Vec::with_capacity(names.len());
                for n in names {
                    let n = n.as_str().filter(|n| !n.is_empty()).ok_or_else(|| invalid(format!("`columns` holds {n}, and each entry is a non-empty name")))?;
                    if out.iter().any(|o| o == n) {
                        return Err(invalid(format!("`columns` names `{n}` twice")));
                    }
                    out.push(n.to_string());
                }
                Some(out)
            }
        };
        Ok(BodyKeys { compression, member, delimited: Delimited { delimiter, columns } })
    }

    /// The compression `name` is stored under: a key or URL path judged by its extension
    /// where the source declares none.
    pub fn compression_of(&self, name: &str) -> Compression {
        match self.compression {
            Compression::ByKey if strip_ext(name, ".gz").is_some() => Compression::Gzip,
            Compression::ByKey if strip_ext(name, ".zip").is_some() => Compression::Zip,
            Compression::ByKey => Compression::None,
            c => c,
        }
    }

    /// `body` decompressed under [`EXPANSION_CEILING`], `name` choosing by extension and
    /// `input` naming it in a refusal.
    pub fn decompress<'a>(&self, name: &str, body: &'a [u8], input: &str) -> Result<Cow<'a, [u8]>, Failure> {
        match self.compression_of(name) {
            Compression::Gzip => gunzip(body, EXPANSION_CEILING, input).map(Cow::Owned),
            Compression::Zip => unzip(body, self.member.as_deref(), EXPANSION_CEILING, input).map(Cow::Owned),
            Compression::ByKey | Compression::None => Ok(Cow::Borrowed(body)),
        }
    }
}
