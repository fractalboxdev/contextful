//! One home per capability (`assurance.structure-tree.one-home`): the registry at
//! [`REGISTRY`] names each capability's home path and defining symbols
//! (`assurance.structure-tree.capability-registry`); a definition of a symbol, or a
//! constant carrying a home constant's name and literal value, outside its home raises
//! `CapabilityDuplicated` (`assurance.structure-tree.duplicated-capability`) in Rust,
//! TypeScript, JavaScript or Python (`assurance.structure-tree.cross-language`). A Rust
//! implementation of a port trait is an adapter (`assurance.structure-tree.adapter`); a
//! site under a comment line opening with the mirror tag is admitted
//! (`assurance.structure-tree.mirror-exemption`); `contextful-ci mirrors` resolves the
//! same lines through [`mirror_tag`]. A `[limit]` entry naming its constant as
//! `const = "<file>::<NAME>"` holds that constant to the bound's value
//! (`assurance.structure-tree.bound-constant`).

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

/// Trees the capability check scans and the mirror check resolves.
pub const ROOTS: [&str; 4] = ["crates/", "apps/", "tools/", "packages/"];
const SKIPPED_SEGMENTS: [&str; 7] = ["tests", "test", "__tests__", "fixtures", "vendor", "benches", "node_modules"];
const SCRIPT: [&str; 7] = ["ts", "tsx", "mts", "cts", "js", "mjs", "cjs"];

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    #[serde(default)]
    ports: Vec<String>,
    #[serde(default)]
    capability: Vec<Capability>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
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
        if c.symbols.is_empty() {
            unresolved.push(format!("capability `{}` names no symbol", c.name));
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
    let bounds = bound_constants(root)?;
    println!("capabilities: {} homes, {} files scanned, {bounds} bound constants", registry.capability.len(), files.len());
    Ok(())
}

/// Every unit of time or size, in its kind's smallest unit: milliseconds or bytes.
const SCALE: [(&str, f64, &str); 9] = [
    ("ms", 1.0, "time"),
    ("s", 1e3, "time"),
    ("min", 6e4, "time"),
    ("h", 3.6e6, "time"),
    ("d", 8.64e7, "time"),
    ("B", 1.0, "size"),
    ("KiB", 1024.0, "size"),
    ("MiB", 1_048_576.0, "size"),
    ("GiB", 1_073_741_824.0, "size"),
];

/// The numbers a bound of `value` `unit` may be written as: itself, or the same quantity
/// in another unit of its kind.
fn spellings(value: f64, unit: &str) -> Vec<f64> {
    let mut out = vec![value];
    if let Some((_, scale, kind)) = SCALE.iter().find(|(u, _, _)| *u == unit) {
        out.extend(SCALE.iter().filter(|(_, _, k)| k == kind).map(|(_, s, _)| value * scale / s));
    }
    out
}

/// `assurance.structure-tree.bound-constant`: each `[limit]` entry of `spec/terms/*.toml`
/// carrying `const = "<file>::<NAME>"` names a non-test constant equal to its bound.
fn bound_constants(root: &Path) -> Result<usize> {
    let Ok(dir) = std::fs::read_dir(root.join("spec/terms")) else { return Ok(0) };
    let mut fragments: Vec<_> = dir.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "toml")).collect();
    fragments.sort();
    let (mut checked, mut drift) = (0, Vec::new());
    for path in fragments {
        let text = std::fs::read_to_string(&path)?;
        let Ok(doc) = text.parse::<toml::Table>() else { continue };
        let Some(limits) = doc.get("limit").and_then(toml::Value::as_table) else { continue };
        for (id, entry) in limits {
            let Some(named) = entry.get("const").and_then(toml::Value::as_str) else { continue };
            checked += 1;
            let value = entry.get("value").and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)));
            let unit = entry.get("unit").and_then(toml::Value::as_str).unwrap_or_default();
            let Some((file, name)) = named.rsplit_once("::") else {
                drift.push(format!("`{id}` names `{named}`, not `<file>::<NAME>`"));
                continue;
            };
            if !scanned(file) {
                drift.push(format!("`{id}` names a test file, {file}"));
                continue;
            }
            let held = match root.join(file).is_file() {
                false => None,
                true => const_value(&parse(root, file)?.items, name),
            };
            let Some(held) = held else {
                drift.push(format!("`{id}`: {file} defines no constant `{name}` with a numeric value"));
                continue;
            };
            let Some(value) = value else {
                drift.push(format!("`{id}` names a constant but carries no numeric value"));
                continue;
            };
            if !spellings(value, unit).iter().any(|v| (v - held).abs() < 1e-9 * v.abs().max(1.0)) {
                drift.push(format!("`{id}` is {value} {unit}, and {named} holds {held}"));
            }
        }
    }
    if !drift.is_empty() {
        drift.iter().for_each(|d| eprintln!("BoundConstantDrift: {d}"));
        return Err(refuse("BoundConstantDrift", format!("{} bound constant(s) differ from their registry value", drift.len())));
    }
    Ok(checked)
}

/// The numeric value of constant `name` among `items`, inside modules and impls too; a
/// `Duration::from_*` initializer reads in milliseconds.
fn const_value(items: &[syn::Item], name: &str) -> Option<f64> {
    for item in items {
        let found = match item {
            syn::Item::Const(c) if c.ident == name => duration_or_number(&c.expr),
            syn::Item::Static(st) if st.ident == name => duration_or_number(&st.expr),
            syn::Item::Mod(m) if !cfg_test(&m.attrs) => m.content.as_ref().and_then(|(_, items)| const_value(items, name)),
            syn::Item::Impl(i) => i.items.iter().find_map(|ii| match ii {
                syn::ImplItem::Const(c) if c.ident == name => duration_or_number(&c.expr),
                _ => None,
            }),
            _ => None,
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

fn duration_or_number(expr: &syn::Expr) -> Option<f64> {
    let as_f64 = |n: Number| match n {
        Number::Int(i) => i as f64,
        Number::Float(f) => f,
    };
    if let syn::Expr::Call(call) = expr {
        let syn::Expr::Path(p) = &*call.func else { return None };
        let scale = match p.path.segments.last()?.ident.to_string().as_str() {
            "from_millis" => 1.0,
            "from_secs" => 1e3,
            "from_mins" => 6e4,
            "from_hours" => 3.6e6,
            _ => return None,
        };
        let [arg] = call.args.iter().collect::<Vec<_>>()[..] else { return None };
        return rust_number(arg).map(|n| as_f64(n) * scale);
    }
    rust_number(expr).map(as_f64)
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

const MIRROR_TAG: &str = "mirrors:";

/// The clause id of a comment line whose body opens with the mirror tag, empty when the
/// tag names none. A tag after code or inside comment prose is no annotation.
pub fn mirror_tag(line: &str) -> Option<&str> {
    let code = line.trim_start();
    let body = code.trim_start_matches(['/', '#', '-', '*']).trim_start();
    let rest = body.strip_prefix(MIRROR_TAG).filter(|_| body.len() < code.len())?;
    Some(rest.split_whitespace().next().unwrap_or(""))
}

/// Whether a comment, attribute or decorator line directly above the definition at
/// 1-based `line` is a mirror annotation.
fn mirrored(lines: &[&str], line: usize) -> bool {
    let mut at = line.saturating_sub(1).min(lines.len());
    while at > 0 {
        at -= 1;
        let l = lines[at].trim_start();
        if !["//", "#", "/*", "*", "@"].iter().any(|p| l.starts_with(p)) {
            return false;
        }
        if mirror_tag(l).is_some() {
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
            syn::Lit::Bool(b) => Some(format!("b:{}", b.value)),
            _ => rust_number(expr).map(number_text),
        },
        syn::Expr::Paren(p) => rust_literal(&p.expr),
        syn::Expr::Group(g) => rust_literal(&g.expr),
        _ => rust_number(expr).map(number_text),
    }
}

#[derive(Clone, Copy)]
enum Number {
    Int(i128),
    Float(f64),
}

/// A numeric literal, or integer `*`, `+`, `-` and `<<` over numeric literals, evaluated.
fn rust_number(expr: &syn::Expr) -> Option<Number> {
    use syn::{BinOp, Expr, Lit};
    match expr {
        Expr::Lit(l) => match &l.lit {
            Lit::Int(i) => i.base10_digits().parse().ok().map(Number::Int),
            Lit::Float(f) => f.base10_digits().parse().ok().map(Number::Float),
            _ => None,
        },
        Expr::Unary(u) if matches!(u.op, syn::UnOp::Neg(_)) => match rust_number(&u.expr)? {
            Number::Int(i) => i.checked_neg().map(Number::Int),
            Number::Float(f) => Some(Number::Float(-f)),
        },
        Expr::Paren(p) => rust_number(&p.expr),
        Expr::Group(g) => rust_number(&g.expr),
        Expr::Binary(b) => {
            let (Number::Int(l), Number::Int(r)) = (rust_number(&b.left)?, rust_number(&b.right)?) else { return None };
            match b.op {
                BinOp::Mul(_) => l.checked_mul(r),
                BinOp::Add(_) => l.checked_add(r),
                BinOp::Sub(_) => l.checked_sub(r),
                BinOp::Shl(_) => u32::try_from(r).ok().and_then(|r| l.checked_shl(r)),
                _ => None,
            }
            .map(Number::Int)
        }
        _ => None,
    }
}

/// A number in one canonical spelling across languages: `4096`, `4 * 1024` and `4096.0`
/// meet, as do `2.50` and `2.5`.
fn number_text(n: Number) -> String {
    match n {
        Number::Int(i) => format!("n:{i}"),
        Number::Float(f) if f.fract() == 0.0 && f.abs() < 1e18 => format!("n:{}", f as i128),
        Number::Float(f) => format!("n:{f}"),
    }
}

/// A script or Python numeric literal: decimal, `0x`, `0o` or `0b`, with `_` separators and
/// an optional BigInt `n` suffix.
fn script_number(text: &str) -> Option<Number> {
    let text = text.replace('_', "");
    let (negative, unsigned) = text.strip_prefix('-').map_or((false, text.as_str()), |u| (true, u));
    let sign = |i: i128| if negative { -i } else { i };
    let radix = [("0x", 16), ("0X", 16), ("0o", 8), ("0O", 8), ("0b", 2), ("0B", 2)]
        .iter()
        .find_map(|(p, r)| unsigned.strip_prefix(p).map(|d| (d, *r)));
    if let Some((digits, radix)) = radix {
        return i128::from_str_radix(digits.trim_end_matches('n'), radix).ok().map(|i| Number::Int(sign(i)));
    }
    if !unsigned.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    if let Ok(i) = unsigned.trim_end_matches('n').parse::<i128>() {
        return Some(Number::Int(sign(i)));
    }
    unsigned.parse::<f64>().ok().filter(|f| f.is_finite()).map(|f| Number::Float(if negative { -f } else { f }))
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
    script_number(text).map(number_text)
}

fn identifier(text: &str) -> Option<(&str, &str)> {
    let end = text.find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$')).unwrap_or(text.len());
    (end > 0 && !text.as_bytes()[0].is_ascii_digit()).then(|| (&text[..end], &text[end..]))
}

/// TypeScript and JavaScript definitions: functions, classes, enums, `const`, `let` or
/// `var` bindings, and class methods and function-valued class fields. Type aliases,
/// interfaces and bodiless method signatures declare shapes and define nothing.
fn script(lines: &[&str], out: &mut Vec<Raw>) {
    let mut in_block = false;
    let mut depth = 0i32;
    // Brace depth of each open class body, innermost last.
    let mut classes: Vec<i32> = Vec::new();
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
        let line = n + 1;
        let (opened, closed) = braces(l);
        if classes.last() == Some(&depth) {
            script_member(l, line, out);
        } else {
            let declared = script_declaration(l, line, out);
            if declared {
                classes.push(depth + 1);
            }
        }
        depth += opened - closed;
        while closed > 0 && classes.last().is_some_and(|body| depth < *body) {
            classes.pop();
        }
    }
}

/// Records a top-level-style script definition on `l`; true when it opens a class.
fn script_declaration(l: &str, line: usize, out: &mut Vec<Raw>) -> bool {
    let mut l = l;
    loop {
        let before = l;
        for prefix in ["export ", "default ", "declare ", "abstract ", "async "] {
            l = l.strip_prefix(prefix).unwrap_or(l).trim_start();
        }
        if l == before {
            break;
        }
    }
    if let Some(rest) = l.strip_prefix("function").map(|r| r.trim_start_matches('*').trim_start()) {
        if let Some((name, _)) = identifier(rest) {
            out.push((line, name.to_string(), Kind::Item, None));
        }
    } else if let Some(rest) = l.strip_prefix("class ") {
        if let Some((name, _)) = identifier(rest.trim_start()) {
            out.push((line, name.to_string(), Kind::Item, None));
        }
        return true;
    } else if let Some(rest) = l.strip_prefix("enum ").or_else(|| l.strip_prefix("const enum ")) {
        if let Some((name, _)) = identifier(rest.trim_start()) {
            out.push((line, name.to_string(), Kind::Item, None));
        }
    } else if let Some((immutable, rest)) =
        l.strip_prefix("const ").map(|r| (true, r)).or_else(|| l.strip_prefix("let ").or_else(|| l.strip_prefix("var ")).map(|r| (false, r)))
    {
        let Some((name, after)) = identifier(rest.trim_start()) else { return false };
        let after = after.trim_start();
        let after = if after.starts_with(':') { after.split_once('=').map_or("", |(_, v)| v) } else { after.strip_prefix('=').unwrap_or("") };
        let value = if immutable { script_literal(after) } else { None };
        let kind = if value.is_some() { Kind::Constant } else { Kind::Item };
        out.push((line, name.to_string(), kind, value));
    }
    false
}

const MEMBER_MODIFIERS: [&str; 11] =
    ["static ", "async ", "public ", "private ", "protected ", "readonly ", "override ", "abstract ", "declare ", "get ", "set "];

/// Records a class member on `l` that carries a body: a method `name(…) {` or a field
/// holding a function, `name = (…) =>`.
fn script_member(l: &str, line: usize, out: &mut Vec<Raw>) {
    let mut l = l;
    loop {
        let before = l;
        for prefix in MEMBER_MODIFIERS {
            l = l.strip_prefix(prefix).unwrap_or(l).trim_start();
        }
        if l == before {
            break;
        }
    }
    let l = l.trim_start_matches('*').trim_start_matches('#');
    let Some((name, after)) = identifier(l) else { return };
    if name == "constructor" {
        return;
    }
    let after = after.trim_start();
    let method = (after.starts_with('(') || after.starts_with('<')) && !l.trim_end().ends_with(';');
    let field = after.strip_prefix('=').filter(|v| !v.starts_with(['=', '>'])).is_some_and(|v| {
        let v = v.trim_start();
        v.starts_with('(') || v.starts_with("async") || v.starts_with("function")
    });
    if method || field {
        out.push((line, name.to_string(), Kind::Item, None));
    }
}

/// Counts of `{` and `}` on a script line outside string literals and a trailing `//` comment.
fn braces(l: &str) -> (i32, i32) {
    let (mut opened, mut closed) = (0, 0);
    let mut quote: Option<char> = None;
    let mut chars = l.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(_) if c == '\\' => {
                chars.next();
            }
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '"' | '\'' | '`' => quote = Some(c),
                '/' if chars.peek() == Some(&'/') => break,
                '{' => opened += 1,
                '}' => closed += 1,
                _ => {}
            },
        }
    }
    (opened, closed)
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
