//! One home per capability (`assurance.structure-tree.one-home`): the registry at
//! [`REGISTRY`] names each capability's home path and defining symbols
//! (`assurance.structure-tree.capability-registry`); a definition of a symbol, or a
//! constant carrying a home constant's name and literal value, outside its home raises
//! `CapabilityDuplicated` (`assurance.structure-tree.duplicated-capability`) in Rust,
//! TypeScript, JavaScript or Python (`assurance.structure-tree.cross-language`). A Rust
//! implementation of a port trait is an adapter (`assurance.structure-tree.adapter`); a
//! site under a `mirrors:` comment is admitted (`assurance.structure-tree.mirror-exemption`).

use anyhow::{Context, Result};
use proc_macro2::Span;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::source_lint::cfg_test;
use crate::{refuse, tracked};

pub const REGISTRY: &str = "spec/capabilities.toml";

/// The capabilities `assurance.structure-tree.one-home` names, each holding one home.
const CAPABILITIES: [&str; 5] = [
    "enforcement",
    "control-plane state",
    "credential resolution",
    "statement guarding",
    "visibility",
];

/// A tree carrying the corpus lock carries the registry beside it.
const CORPUS_MARK: &str = "spec/spec.lock.json";

const ROOTS: [&str; 4] = ["crates/", "apps/", "tools/", "packages/"];
const SKIPPED_SEGMENTS: [&str; 7] = ["tests", "test", "__tests__", "fixtures", "vendor", "benches", "node_modules"];
const SCRIPT: [&str; 7] = ["ts", "tsx", "mts", "cts", "js", "mjs", "cjs"];

#[derive(serde::Deserialize)]
struct Registry {
    #[serde(default)]
    ports: Vec<String>,
    #[serde(default)]
    capability: Vec<Capability>,
}

#[derive(serde::Deserialize)]
struct Capability {
    name: String,
    home: String,
    #[serde(default)]
    symbols: Vec<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// A function, type or value binding.
    Item,
    /// A constant or immutable binding with a literal initializer.
    Constant,
}

/// One definition in a scanned file.
struct Def {
    file: String,
    line: usize,
    name: String,
    kind: Kind,
    /// The literal value of a constant, normalized.
    value: Option<String>,
    mirrored: bool,
}

impl Def {
    fn site(&self) -> String {
        format!("{}:{}", self.file, self.line)
    }
}

pub fn check(root: &Path) -> Result<()> {
    let path = root.join(REGISTRY);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(_) if !root.join(CORPUS_MARK).is_file() => return Ok(()),
        Err(_) => return Err(refuse("CapabilityHomeUnresolved", format!("{REGISTRY} is absent beside {CORPUS_MARK}"))),
    };
    let registry: Registry = toml::from_str(&text).with_context(|| format!("parse {REGISTRY}"))?;

    let files: Vec<String> = tracked(root)?
        .into_iter()
        .filter(|f| ROOTS.iter().any(|r| f.starts_with(r)) && scanned(f) && root.join(f).is_file())
        .collect();
    let ports: BTreeSet<String> = port_traits(root, &files, &registry.ports)?;
    let mut defs = Vec::new();
    for file in &files {
        defs.extend(definitions(root, file, &ports)?);
    }

    let mut unresolved = Vec::new();
    for name in CAPABILITIES {
        match registry.capability.iter().filter(|c| c.name == name).count() {
            0 => unresolved.push(format!("capability `{name}` names no home")),
            1 => {}
            n => unresolved.push(format!("capability `{name}` names {n} homes")),
        }
    }
    for c in &registry.capability {
        if !CAPABILITIES.contains(&c.name.as_str()) {
            unresolved.push(format!("`{}` is no capability of assurance.structure-tree.one-home", c.name));
        }
        if !root.join(&c.home).exists() {
            unresolved.push(format!("capability `{}` names home {}, absent from the tree", c.name, c.home));
            continue;
        }
        for symbol in &c.symbols {
            if !defs.iter().any(|d| under(&d.file, &c.home) && fold(&d.name) == fold(symbol)) {
                unresolved.push(format!("capability `{}` names symbol `{symbol}`, which {} defines nowhere", c.name, c.home));
            }
        }
    }
    if !unresolved.is_empty() {
        unresolved.iter().for_each(|u| eprintln!("CapabilityHomeUnresolved: {u}"));
        return Err(refuse("CapabilityHomeUnresolved", format!("{} registry entr(ies) resolve to no home in {REGISTRY}", unresolved.len())));
    }

    let mut duplicated = Vec::new();
    for c in &registry.capability {
        let homed: Vec<&Def> = defs.iter().filter(|d| under(&d.file, &c.home)).collect();
        let symbols: BTreeMap<String, &Def> = c
            .symbols
            .iter()
            .filter_map(|s| homed.iter().find(|d| fold(&d.name) == fold(s)).map(|d| (fold(s), *d)))
            .collect();
        let constants: Vec<&Def> = homed.iter().copied().filter(|d| d.kind == Kind::Constant).collect();
        for d in defs.iter().filter(|d| !under(&d.file, &c.home) && !d.mirrored) {
            let folded = fold(&d.name);
            if let Some(home) = symbols.get(&folded) {
                duplicated.push(format!("{} defines `{}`, a second implementation of {} at {}", d.site(), d.name, c.name, home.site()));
            } else if let Some(home) = constants.iter().find(|h| d.kind == Kind::Constant && fold(&h.name) == folded && h.value == d.value) {
                duplicated.push(format!("{} copies constant `{}`, the {} home's {}", d.site(), d.name, c.name, home.site()));
            }
        }
    }
    if !duplicated.is_empty() {
        duplicated.iter().for_each(|d| eprintln!("CapabilityDuplicated: {d}"));
        return Err(refuse("CapabilityDuplicated", format!("{} definition(s) restate an engine capability", duplicated.len())));
    }
    println!("capabilities: {} homes, {} files scanned", registry.capability.len(), files.len());
    Ok(())
}

/// A name lowercased with `_` and `-` removed, so `single_statement` and `singleStatement` meet.
fn fold(name: &str) -> String {
    name.chars().filter(|c| *c != '_' && *c != '-').flat_map(char::to_lowercase).collect()
}

fn under(file: &str, home: &str) -> bool {
    let home = home.trim_end_matches('/');
    file == home || file.strip_prefix(home).is_some_and(|rest| rest.starts_with('/'))
}

fn scanned(file: &str) -> bool {
    let Some((dirs, name)) = file.rsplit_once('/') else { return false };
    if dirs.split('/').any(|seg| SKIPPED_SEGMENTS.contains(&seg)) {
        return false;
    }
    let Some((stem, ext)) = name.rsplit_once('.') else { return false };
    if stem.ends_with(".test") || stem.ends_with(".spec") || stem.ends_with("_test") || stem.ends_with("_tests") || stem.starts_with("test_") {
        return false;
    }
    ext == "rs" || ext == "py" || SCRIPT.contains(&ext)
}

/// Names of the traits declared under the registry's port paths.
fn port_traits(root: &Path, files: &[String], ports: &[String]) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for file in files.iter().filter(|f| f.ends_with(".rs") && ports.iter().any(|p| under(f, p))) {
        let syntax = parse(root, file)?;
        collect_traits(&syntax.items, &mut out);
    }
    Ok(out)
}

fn collect_traits(items: &[syn::Item], out: &mut BTreeSet<String>) {
    for item in items {
        match item {
            syn::Item::Trait(t) => {
                out.insert(t.ident.to_string());
            }
            syn::Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    collect_traits(inner, out);
                }
            }
            _ => {}
        }
    }
}

fn parse(root: &Path, file: &str) -> Result<syn::File> {
    let source = std::fs::read_to_string(root.join(file)).with_context(|| format!("read {file}"))?;
    syn::parse_file(&source).with_context(|| format!("parse {file}"))
}

fn definitions(root: &Path, file: &str, ports: &BTreeSet<String>) -> Result<Vec<Def>> {
    let text = std::fs::read_to_string(root.join(file)).with_context(|| format!("read {file}"))?;
    let lines: Vec<&str> = text.lines().collect();
    let mut raw = Vec::new();
    if file.ends_with(".rs") {
        let syntax = syn::parse_file(&text).with_context(|| format!("parse {file}"))?;
        rust_items(&syntax.items, ports, &mut raw);
    } else if file.ends_with(".py") {
        python(&lines, &mut raw);
    } else {
        script(&lines, &mut raw);
    }
    Ok(raw
        .into_iter()
        .map(|(line, name, kind, value)| Def { file: file.to_string(), line, mirrored: mirrored(&lines, line), name, kind, value })
        .collect())
}

type Raw = (usize, String, Kind, Option<String>);

/// Whether the definition at 1-based `line` carries a `mirrors:` comment on its line or in
/// the comment, attribute and decorator lines directly above it.
fn mirrored(lines: &[&str], line: usize) -> bool {
    let tagged = |l: &str| l.contains("mirrors:");
    if lines.get(line.wrapping_sub(1)).is_some_and(|l| tagged(l)) {
        return true;
    }
    let mut at = line.wrapping_sub(1);
    while at > 0 {
        at -= 1;
        let l = lines[at].trim_start();
        let annotation = ["//", "#", "/*", "*", "@"].iter().any(|p| l.starts_with(p));
        if !annotation {
            return false;
        }
        if tagged(l) {
            return true;
        }
    }
    false
}

fn line(span: Span) -> usize {
    span.start().line
}

fn rust_items(items: &[syn::Item], ports: &BTreeSet<String>, out: &mut Vec<Raw>) {
    use syn::Item;
    for item in items {
        let item_ident = |ident: &syn::Ident| (line(ident.span()), ident.to_string(), Kind::Item, None);
        match item {
            Item::Fn(f) if !cfg_test(&f.attrs) => out.push(item_ident(&f.sig.ident)),
            Item::Struct(s) if !cfg_test(&s.attrs) => out.push(item_ident(&s.ident)),
            Item::Enum(e) if !cfg_test(&e.attrs) => out.push(item_ident(&e.ident)),
            Item::Union(u) if !cfg_test(&u.attrs) => out.push(item_ident(&u.ident)),
            Item::Trait(t) if !cfg_test(&t.attrs) => out.push(item_ident(&t.ident)),
            Item::Type(t) if !cfg_test(&t.attrs) => out.push(item_ident(&t.ident)),
            Item::Const(c) if !cfg_test(&c.attrs) => out.push(rust_constant(&c.ident, &c.expr)),
            Item::Static(s) if !cfg_test(&s.attrs) => out.push(rust_constant(&s.ident, &s.expr)),
            Item::Mod(m) if !cfg_test(&m.attrs) => {
                if let Some((_, inner)) = &m.content {
                    rust_items(inner, ports, out);
                }
            }
            Item::Impl(i) if !cfg_test(&i.attrs) => {
                let port = i
                    .trait_
                    .as_ref()
                    .and_then(|(_, path, _)| path.segments.last())
                    .is_some_and(|s| ports.contains(&s.ident.to_string()));
                if port {
                    continue;
                }
                for member in &i.items {
                    match member {
                        syn::ImplItem::Fn(f) if !cfg_test(&f.attrs) => out.push(item_ident(&f.sig.ident)),
                        syn::ImplItem::Const(c) if !cfg_test(&c.attrs) => out.push(rust_constant(&c.ident, &c.expr)),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

fn rust_constant(ident: &syn::Ident, expr: &syn::Expr) -> Raw {
    let value = rust_literal(expr);
    let kind = if value.is_some() { Kind::Constant } else { Kind::Item };
    (line(ident.span()), ident.to_string(), kind, value)
}

fn rust_literal(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Lit(l) => match &l.lit {
            syn::Lit::Str(s) => Some(format!("s:{}", s.value())),
            syn::Lit::Int(i) => Some(format!("n:{}", i.base10_digits())),
            syn::Lit::Float(f) => Some(format!("n:{}", f.base10_digits())),
            syn::Lit::Bool(b) => Some(format!("b:{}", b.value)),
            _ => None,
        },
        syn::Expr::Unary(u) if matches!(u.op, syn::UnOp::Neg(_)) => rust_literal(&u.expr).map(|v| v.replacen("n:", "n:-", 1)),
        syn::Expr::Paren(p) => rust_literal(&p.expr),
        syn::Expr::Group(g) => rust_literal(&g.expr),
        _ => None,
    }
}

/// The literal at the head of `text` in script syntax, normalized as [`rust_literal`] does.
fn script_literal(text: &str) -> Option<String> {
    let text = text.trim().trim_end_matches(';').trim_end_matches(',').trim();
    let text = text.split(" as ").next().unwrap_or(text).trim();
    if let Some(q) = text.chars().next().filter(|c| matches!(c, '"' | '\'' | '`')) {
        let inner = text.strip_prefix(q)?.strip_suffix(q)?;
        if inner.contains(q) || (q == '`' && inner.contains("${")) {
            return None;
        }
        return Some(format!("s:{inner}"));
    }
    if text == "true" || text == "false" || text == "True" || text == "False" {
        return Some(format!("b:{}", text.to_ascii_lowercase()));
    }
    let digits = text.trim_end_matches('n').replace('_', "");
    let unsigned = digits.strip_prefix('-').unwrap_or(&digits);
    if !unsigned.is_empty() && unsigned.chars().all(|c| c.is_ascii_digit() || c == '.') && unsigned.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        let trimmed = if digits.contains('.') { digits.trim_end_matches('0').trim_end_matches('.').to_string() } else { digits };
        return Some(format!("n:{trimmed}"));
    }
    None
}

fn identifier(text: &str) -> Option<(&str, &str)> {
    let end = text.find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$')).unwrap_or(text.len());
    (end > 0 && !text.as_bytes()[0].is_ascii_digit()).then(|| (&text[..end], &text[end..]))
}

/// TypeScript and JavaScript definitions: functions, classes, enums and `const`, `let` or
/// `var` bindings. Type aliases and interfaces declare shapes and define nothing.
fn script(lines: &[&str], out: &mut Vec<Raw>) {
    let mut in_block = false;
    for (n, raw) in lines.iter().enumerate() {
        let mut l = raw.trim();
        if in_block {
            match l.find("*/") {
                Some(at) => {
                    in_block = false;
                    l = l[at + 2..].trim();
                }
                None => continue,
            }
        }
        if l.starts_with("/*") && !l.contains("*/") {
            in_block = true;
            continue;
        }
        if l.starts_with("//") || l.starts_with('*') {
            continue;
        }
        for prefix in ["export ", "default ", "declare ", "async "] {
            l = l.strip_prefix(prefix).unwrap_or(l).trim_start();
        }
        let line = n + 1;
        if let Some(rest) = l.strip_prefix("function").map(|r| r.trim_start_matches('*').trim_start()) {
            if let Some((name, _)) = identifier(rest) {
                out.push((line, name.to_string(), Kind::Item, None));
            }
        } else if let Some(rest) = l.strip_prefix("class ").or_else(|| l.strip_prefix("enum ")).or_else(|| l.strip_prefix("const enum ")) {
            if let Some((name, _)) = identifier(rest.trim_start()) {
                out.push((line, name.to_string(), Kind::Item, None));
            }
        } else if let Some((immutable, rest)) =
            l.strip_prefix("const ").map(|r| (true, r)).or_else(|| l.strip_prefix("let ").or_else(|| l.strip_prefix("var ")).map(|r| (false, r)))
        {
            let Some((name, after)) = identifier(rest.trim_start()) else { continue };
            let after = after.trim_start();
            let after = if after.starts_with(':') { after.split_once('=').map_or("", |(_, v)| v) } else { after.strip_prefix('=').unwrap_or("") };
            let value = if immutable { script_literal(after) } else { None };
            let kind = if value.is_some() { Kind::Constant } else { Kind::Item };
            out.push((line, name.to_string(), kind, value));
        }
    }
}

/// Python definitions: `def` and `class` at any depth, and module-level uppercase
/// assignments of a literal as constants.
fn python(lines: &[&str], out: &mut Vec<Raw>) {
    for (n, raw) in lines.iter().enumerate() {
        let l = raw.trim_start();
        let line = n + 1;
        if let Some(rest) = l.strip_prefix("def ").or_else(|| l.strip_prefix("async def ")).or_else(|| l.strip_prefix("class ")) {
            if let Some((name, _)) = identifier(rest.trim_start()) {
                out.push((line, name.to_string(), Kind::Item, None));
            }
        } else if l.len() == raw.len() {
            let Some((name, after)) = identifier(l) else { continue };
            let after = after.trim_start();
            let after = if after.starts_with(':') { after.split_once('=').map_or("", |(_, v)| v) } else { after };
            let Some(value) = after.strip_prefix('=').filter(|v| !v.starts_with('=')) else { continue };
            if name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') {
                let value = script_literal(value);
                let kind = if value.is_some() { Kind::Constant } else { Kind::Item };
                out.push((line, name.to_string(), kind, value));
            }
        }
    }
}
