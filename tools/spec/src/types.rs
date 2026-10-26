//! `{{type:<rust path>}}` pointers (`corpus.reference.type-pointer`): the struct or enum a
//! clause names in place of its keys, read with `syn` into the lock file and the cards; the
//! pointers naming none (`corpus.reference.type-unresolved`); and the clauses that list a
//! deserialized type's keys without one (`corpus.reference.restates-code`).

use crate::corpus::*;
use crate::util::{tick_spans, without_ticks};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::OnceLock;

/// One pointed type in the lock file: where it is defined, its members, and the clauses
/// pointing at it.
#[derive(Serialize, Clone, Debug)]
pub struct TypeRecord {
    pub file: String,
    pub line: usize,
    pub kind: &'static str,
    pub members: Vec<Member>,
    pub clauses: Vec<String>,
}

/// A field or variant, under the spelling `serde` reads, with its doc comment.
#[derive(Serialize, Clone, Debug)]
pub struct Member {
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub doc: String,
}

const ROOTS: [&str; 2] = ["crates", "tools"];

/// Every type pointer in the corpus's prose, with the file and line it sits on.
fn sites(c: &Corpus) -> Vec<(String, usize, String)> {
    let mut out = Vec::new();
    for d in &c.docs {
        for (n, l, k) in d.each() {
            if k == LineKind::Code || k == LineKind::Fence {
                continue;
            }
            for p in type_pointers(&without_ticks(l)) {
                out.push((d.rel.clone(), n, p));
            }
        }
    }
    out
}

/// Each resolved type pointer, keyed by its path, computed once per run; an unresolved
/// pointer has no record.
pub fn records(c: &Corpus) -> &'static BTreeMap<String, TypeRecord> {
    static RECORDS: OnceLock<BTreeMap<String, TypeRecord>> = OnceLock::new();
    RECORDS.get_or_init(|| {
        let mut out: BTreeMap<String, TypeRecord> = BTreeMap::new();
        for path in sites(c).into_iter().map(|(_, _, p)| p).collect::<BTreeSet<_>>() {
            if let Some(record) = resolve(c, &path) {
                out.insert(path, record);
            }
        }
        for cl in c.clauses() {
            for p in type_pointers(&cl.statement) {
                if let Some(r) = out.get_mut(&p) {
                    if !r.clauses.contains(&cl.id) {
                        r.clauses.push(cl.id.clone());
                    }
                }
            }
        }
        out
    })
}

/// `SpecTypeUnresolved` for each type pointer naming no struct or enum where its crate and
/// module part point.
pub fn unresolved(c: &Corpus) -> Vec<Finding> {
    let found = records(c);
    sites(c)
        .into_iter()
        .filter(|(_, _, p)| !found.contains_key(p))
        .map(|(file, line, p)| {
            Finding::new("reference", &file, line, "SpecTypeUnresolved", format!("{{{{{TYPE_POINTER}{p}}}}} names no struct or enum in the module its path names"))
        })
        .collect()
}

/// The struct or enum `<crate>::<module>::<Type>` names: defined at the top level of the
/// file whose module path is exactly the pointer's module part.
fn resolve(c: &Corpus, path: &str) -> Option<TypeRecord> {
    let segs: Vec<&str> = path.split("::").collect();
    let [_, modules @ .., name] = segs.as_slice() else { return None };
    for site in crate::pins::locate(c, "item", path, &ROOTS) {
        if site.score != modules.len() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(c.root.join(&site.file)) else { continue };
        let Ok(file) = syn::parse_file(&text) else { continue };
        for item in &file.items {
            if let Some((kind, ident, members)) = shape(item) {
                if ident == *name {
                    let keyword = regex::Regex::new(&format!(r"(?m)^.*\b{kind}\s+{ident}\b")).ok()?;
                    let line = keyword.find(&text).map_or(0, |m| text[..m.start()].lines().count() + 1);
                    return Some(TypeRecord { file: site.file.clone(), line, kind, members, clauses: Vec::new() });
                }
            }
        }
    }
    None
}

/// A struct with named fields or an enum: its kind, name and members.
fn shape(item: &syn::Item) -> Option<(&'static str, String, Vec<Member>)> {
    match item {
        syn::Item::Struct(s) => {
            let rule = rename_all(&s.attrs);
            let syn::Fields::Named(named) = &s.fields else { return None };
            let members = named
                .named
                .iter()
                .filter(|f| !skipped(&f.attrs))
                .map(|f| {
                    let ident = f.ident.as_ref().map(|i| i.to_string()).unwrap_or_default();
                    Member { name: renamed(&f.attrs).unwrap_or_else(|| apply(rule.as_deref(), &ident, false)), doc: doc(&f.attrs) }
                })
                .collect();
            Some(("struct", s.ident.to_string(), members))
        }
        syn::Item::Enum(e) => {
            let rule = rename_all(&e.attrs);
            let members = e
                .variants
                .iter()
                .filter(|v| !skipped(&v.attrs))
                .map(|v| Member { name: renamed(&v.attrs).unwrap_or_else(|| apply(rule.as_deref(), &v.ident.to_string(), true)), doc: doc(&v.attrs) })
                .collect();
            Some(("enum", e.ident.to_string(), members))
        }
        _ => None,
    }
}

/// A member's `///` text, its lines trimmed and joined by one space.
fn doc(attrs: &[syn::Attribute]) -> String {
    let mut lines = Vec::new();
    for a in attrs.iter().filter(|a| a.path().is_ident("doc")) {
        if let syn::Meta::NameValue(nv) = &a.meta {
            if let syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) = &nv.value {
                let v = s.value();
                if !v.trim().is_empty() {
                    lines.push(v.trim().to_string());
                }
            }
        }
    }
    lines.join(" ")
}

/// Each `#[serde(...)]` entry as its key and, for `key = "value"`, the value.
fn serde_entries(attrs: &[syn::Attribute]) -> Vec<(String, Option<String>)> {
    fn skip(meta: &syn::meta::ParseNestedMeta) -> syn::Result<()> {
        if meta.input.peek(syn::Token![=]) {
            meta.value()?.parse::<syn::Expr>()?;
        } else if meta.input.peek(syn::token::Paren) {
            meta.parse_nested_meta(|inner| skip(&inner))?;
        }
        Ok(())
    }
    let mut out = Vec::new();
    for a in attrs.iter().filter(|a| a.path().is_ident("serde")) {
        let _ = a.parse_nested_meta(|meta| {
            let key = meta.path.get_ident().map(|i| i.to_string()).unwrap_or_default();
            if meta.input.peek(syn::Token![=]) {
                match meta.value()?.parse::<syn::Expr>()? {
                    syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) => out.push((key, Some(s.value()))),
                    _ => out.push((key, None)),
                }
            } else {
                skip(&meta)?;
                out.push((key, None));
            }
            Ok(())
        });
    }
    out
}

fn renamed(attrs: &[syn::Attribute]) -> Option<String> {
    serde_entries(attrs).into_iter().find(|(k, _)| k == "rename").and_then(|(_, v)| v)
}

fn rename_all(attrs: &[syn::Attribute]) -> Option<String> {
    serde_entries(attrs).into_iter().find(|(k, _)| k == "rename_all").and_then(|(_, v)| v)
}

fn skipped(attrs: &[syn::Attribute]) -> bool {
    serde_entries(attrs).iter().any(|(k, _)| k == "skip" || k == "skip_deserializing")
}

/// A field (snake_case) or variant (PascalCase) name under a `rename_all` rule.
fn apply(rule: Option<&str>, ident: &str, variant: bool) -> String {
    let words: Vec<String> = if variant {
        let mut words: Vec<String> = Vec::new();
        for ch in ident.chars() {
            match words.last_mut() {
                Some(w) if !ch.is_uppercase() => w.push(ch),
                _ => words.push(ch.to_string()),
            }
        }
        words.into_iter().map(|w| w.to_lowercase()).collect()
    } else {
        ident.split('_').map(str::to_string).collect()
    };
    let capital = |w: &String| w.chars().take(1).flat_map(char::to_uppercase).chain(w.chars().skip(1)).collect::<String>();
    match rule {
        Some("lowercase") => words.concat(),
        Some("UPPERCASE") => words.concat().to_uppercase(),
        Some("snake_case") => words.join("_"),
        Some("SCREAMING_SNAKE_CASE") => words.join("_").to_uppercase(),
        Some("kebab-case") => words.join("-"),
        Some("SCREAMING-KEBAB-CASE") => words.join("-").to_uppercase(),
        Some("camelCase") => words.iter().enumerate().map(|(i, w)| if i == 0 { w.clone() } else { capital(w) }).collect(),
        Some("PascalCase") => words.iter().map(capital).collect(),
        _ => ident.to_string(),
    }
}

// ---------------------------------------------------------------- restatements

/// How many keys of one type a clause backticks before it restates the type.
const RESTATED_KEYS: usize = 3;

/// Every struct and enum deriving `Deserialize` under `crates/*/src`, keyed by its pointer
/// path, with its member spellings.
fn deserialized(c: &Corpus) -> BTreeMap<String, BTreeSet<String>> {
    let mut out = BTreeMap::new();
    let crates = c.root.join("crates");
    for package in std::fs::read_dir(&crates).into_iter().flatten().flatten().map(|e| e.path()) {
        let Some(ident) = crate_ident(&package) else { continue };
        let src = package.join("src");
        for e in walkdir::WalkDir::new(&src).sort_by_file_name().into_iter().flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(p) else { continue };
            if !text.contains("Deserialize") {
                continue;
            }
            let Ok(file) = syn::parse_file(&text) else { continue };
            let module = module_path(p.strip_prefix(&src).unwrap_or(p));
            for item in &file.items {
                let attrs = match item {
                    syn::Item::Struct(s) => &s.attrs,
                    syn::Item::Enum(e) => &e.attrs,
                    _ => continue,
                };
                if !derives_deserialize(attrs) {
                    continue;
                }
                if let Some((_, name, members)) = shape(item) {
                    let path = [ident.clone()].into_iter().chain(module.iter().cloned()).chain([name]).collect::<Vec<_>>().join("::");
                    out.insert(path, members.into_iter().map(|m| m.name).collect());
                }
            }
        }
    }
    out
}

/// The crate identifier a package's manifest names.
fn crate_ident(package: &Path) -> Option<String> {
    let manifest = std::fs::read_to_string(package.join("Cargo.toml")).ok()?.parse::<toml::Table>().ok()?;
    manifest.get("package")?.get("name")?.as_str().map(|n| n.replace('-', "_"))
}

/// `a/b.rs` and `a/b/mod.rs` are `a::b`; `lib.rs` and `main.rs` the crate root.
fn module_path(in_src: &Path) -> Vec<String> {
    let mut parts: Vec<String> = in_src.iter().map(|p| p.to_string_lossy().trim_end_matches(".rs").to_string()).collect();
    if parts.last().is_some_and(|l| matches!(l.as_str(), "lib" | "main" | "mod")) {
        parts.pop();
    }
    parts
}

fn derives_deserialize(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().filter(|a| a.path().is_ident("derive")).any(|a| {
        let mut found = false;
        let _ = a.parse_nested_meta(|meta| {
            found |= meta.path.segments.last().is_some_and(|s| s.ident == "Deserialize");
            Ok(())
        });
        found
    })
}

/// The identifiers a statement backticks: each identifier of a span holding only
/// identifiers, dots, brackets and spaced `=`-assignments' keys.
fn backticked(statement: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (_, span) in tick_spans(statement) {
        let key = span.split(" =").next().unwrap_or("");
        if key.is_empty() || !key.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '[' | ']')) {
            continue;
        }
        out.extend(key.split(['.', '[', ']']).filter(|w| !w.is_empty()).map(str::to_string));
    }
    out
}

/// Each clause backticking at least three keys of one deserialized type without pointing
/// at it, with that type's path; the type sharing the most keys wins.
pub fn restatements(c: &Corpus) -> Vec<(String, String)> {
    let types = deserialized(c);
    let mut out = Vec::new();
    for cl in c.clauses() {
        let keys = backticked(&cl.statement);
        if keys.len() < RESTATED_KEYS {
            continue;
        }
        let pointed: BTreeSet<String> = type_pointers(&cl.statement).into_iter().collect();
        let best = types
            .iter()
            .map(|(path, members)| (members.intersection(&keys).count(), path))
            .filter(|(n, _)| *n >= RESTATED_KEYS)
            .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(a.1)));
        if let Some((_, path)) = best {
            if !pointed.contains(path) {
                out.push((cl.id.clone(), path.clone()));
            }
        }
    }
    out
}
