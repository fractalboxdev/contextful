//! The request pieces the declared HTTP sources share: URL templates filled in one pass,
//! and header templates hydrated onto each request through the resolver.

use contextful_core::connector::reference::{check_material, Template};
use contextful_core::run::{Failure, RunError};
use contextful_outbound::client::HeaderValue;
use contextful_outbound::Resolver;
use std::collections::BTreeMap;

/// The `{name}` placeholders of `text`, or the first unterminated `{`.
pub(crate) fn placeholders(text: &str) -> Result<Vec<&str>, String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let close = rest[open..].find('}').ok_or_else(|| format!("`{text}` opens a placeholder it never closes"))?;
        out.push(&rest[open + 1..open + close]);
        rest = &rest[open + close + 1..];
    }
    Ok(out)
}

/// Whether a placeholder sits in `text`'s scheme or host, where a filled value chooses the
/// destination.
pub(crate) fn placeholder_in_authority(text: &str) -> bool {
    let authority_end = text.find("://").map_or(text.len(), |s| text[s + 3..].find(['/', '?', '#']).map_or(text.len(), |e| s + 3 + e));
    text[..authority_end].contains('{')
}

/// Percent-encode every byte outside RFC 3986's unreserved set.
pub(crate) fn percent_encode(value: &str) -> String {
    value.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// `template` with each placeholder `values` answers replaced by `encode` of its value, in
/// one pass so a filled value never expands.
pub(crate) fn fill(template: &str, values: &dyn Fn(&str) -> Option<String>, encode: fn(&str) -> String) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some((open, close)) = rest.find('{').and_then(|o| rest[o..].find('}').map(|c| (o, o + c))) {
        out.push_str(&rest[..open]);
        match values(&rest[open + 1..close]) {
            Some(v) => out.push_str(&encode(&v)),
            None => out.push_str(&rest[open..=close]),
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

/// A value as written.
pub(crate) fn verbatim(value: &str) -> String {
    value.to_string()
}

/// Check each declared header value as a template, refusing credential material under
/// `<prefix>headers.<name>`.
pub(crate) fn header_templates(prefix: &str, headers: &BTreeMap<String, String>) -> Result<BTreeMap<String, Template>, RunError> {
    headers
        .iter()
        .map(|(name, value)| Ok((name.clone(), check_material(&format!("{prefix}headers.{name}"), value).map_err(|e| RunError::Invalid(e.to_string()))?)))
        .collect()
}

/// Hydrate `headers` through `resolver`, after a `Content-Type` of `content_type` when one
/// is given; a template carrying a reference hydrates as a sensitive value.
pub(crate) fn hydrate_headers(resolver: &Resolver, headers: &BTreeMap<String, Template>, content_type: Option<&str>) -> Result<Vec<(String, HeaderValue)>, Failure> {
    let mut out = Vec::new();
    if let Some(kind) = content_type {
        out.push(("Content-Type".to_string(), HeaderValue::Plain(kind.into())));
    }
    for (name, t) in headers {
        let v = resolver.render(t)?;
        out.push((name.clone(), if t.has_reference() { HeaderValue::Sensitive(v.into()) } else { HeaderValue::Plain(v.reveal().to_string()) }));
    }
    Ok(out)
}
