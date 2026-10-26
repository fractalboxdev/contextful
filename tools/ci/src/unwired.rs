//! `contextful-ci unwired` (`corpus.state.unwired`): a public item under `crates/*/src` that
//! a performed pin's test names, while no non-test source reaches it. Non-test source
//! reaches an item when another file names it, or when an item of its own file that is
//! reached names it; `main` and trait-impl methods are reached. Such a clause performs
//! through its test and through no binary. A `// entry: <clause id>` comment in non-test
//! source clears the clause. Items of the acceptance harness under `crates/acceptance` are
//! test support and never count.

use anyhow::{Context, Result};
use proc_macro2::{Delimiter, TokenStream, TokenTree};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::refuse;

const LOCK: &str = "spec/spec.lock.json";

/// A function, method, constant or static: its key (`name`, or `Type::name` for a method),
/// whether it is `pub`, whether it is reached by construction, and the names its body holds.
struct Item {
    key: String,
    public: bool,
    root: bool,
    names: BTreeSet<String>,
}

/// One source file under `crates/*/src`, read past its test items and `use` items.
#[derive(Default)]
struct Source {
    file: String,
    items: Vec<Item>,
    /// Every name the file holds: identifiers, `A::b` path pairs and `.m` method calls.
    names: BTreeSet<String>,
    /// The clause ids its `// entry:` comments name.
    entries: Vec<String>,
}

fn ident(t: Option<&TokenTree>) -> Option<String> {
    match t {
        Some(TokenTree::Ident(i)) => Some(i.to_string()),
        _ => None,
    }
}

fn punct(t: Option<&TokenTree>, ch: char) -> bool {
    matches!(t, Some(TokenTree::Punct(p)) if p.as_char() == ch)
}

fn brace(t: &TokenTree) -> Option<TokenStream> {
    match t {
        TokenTree::Group(g) if g.delimiter() == Delimiter::Brace => Some(g.stream()),
        _ => None,
    }
}

/// Whether `t` is the bracket group of a `#[test]` or `#[cfg(test)]`-like attribute.
fn test_attribute(t: Option<&TokenTree>) -> bool {
    let Some(TokenTree::Group(g)) = t else { return false };
    let text = g.stream().to_string();
    g.delimiter() == Delimiter::Bracket && (text == "test" || (text.starts_with("cfg") && text.contains("test")))
}

/// Identifiers, `A::b` path pairs and `.m` method calls in `tokens`, groups included.
fn names(tokens: TokenStream, out: &mut BTreeSet<String>) {
    let tokens: Vec<TokenTree> = tokens.into_iter().collect();
    for (i, t) in tokens.iter().enumerate() {
        match t {
            TokenTree::Ident(id) => {
                let id = id.to_string();
                if i >= 1 && punct(tokens.get(i - 1), '.') {
                    out.insert(format!(".{id}"));
                }
                if i >= 3 && punct(tokens.get(i - 1), ':') && punct(tokens.get(i - 2), ':') {
                    if let Some(owner) = ident(tokens.get(i - 3)) {
                        out.insert(format!("{owner}::{id}"));
                    }
                }
                out.insert(id);
            }
            TokenTree::Group(g) => names(g.stream(), out),
            _ => {}
        }
    }
}

/// The self type of an `impl` whose header is `header`, and whether it implements a trait.
fn self_type(header: &[TokenTree]) -> (Option<String>, bool) {
    let after_for = header.iter().position(|t| ident(Some(t)).as_deref() == Some("for"));
    let from = after_for.map_or(0, |i| i + 1);
    let mut depth = 0i32;
    for t in &header[from..] {
        match t {
            TokenTree::Punct(p) if p.as_char() == '<' => depth += 1,
            TokenTree::Punct(p) if p.as_char() == '>' => depth -= 1,
            TokenTree::Ident(i) if depth == 0 && !matches!(i.to_string().as_str(), "impl" | "dyn" | "mut" | "const") => {
                return (Some(i.to_string()), after_for.is_some());
            }
            _ => {}
        }
    }
    (None, after_for.is_some())
}

/// Record the items of an item-level token list, skipping test items and `use` items;
/// `owner` is the self type and trait flag of an enclosing `impl`.
fn scan(tokens: TokenStream, owner: Option<(&str, bool)>, out: &mut Source) {
    let tokens: Vec<TokenTree> = tokens.into_iter().collect();
    let mut i = 0;
    let mut public = false;
    while i < tokens.len() {
        let t = &tokens[i];
        let word = ident(Some(t));
        if punct(Some(t), '#') && test_attribute(tokens.get(i + 1)) {
            while i < tokens.len() && !punct(tokens.get(i), ';') && brace(&tokens[i]).is_none() {
                i += 1;
            }
            i += 1;
            public = false;
            continue;
        }
        match word.as_deref() {
            Some("use") => {
                while i < tokens.len() && !punct(tokens.get(i), ';') {
                    i += 1;
                }
            }
            Some("pub") => {
                public = !matches!(tokens.get(i + 1), Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis);
                i += 1;
                continue;
            }
            Some("impl") => {
                let Some(end) = tokens[i..].iter().position(|t| brace(t).is_some()).map(|n| i + n) else { break };
                names(tokens[i..end].iter().cloned().collect(), &mut out.names);
                let (ty, by_trait) = self_type(&tokens[i + 1..end]);
                scan(brace(&tokens[end]).unwrap_or_default(), Some((ty.as_deref().unwrap_or("?"), by_trait)), out);
                i = end;
            }
            Some("mod") => {
                if let Some(body) = tokens.get(i + 2).and_then(brace) {
                    scan(body, None, out);
                    i += 2;
                }
            }
            Some("fn") | Some("const") | Some("static") => {
                let name = ident(tokens.get(i + 1)).filter(|n| n != "fn" && n != "_");
                let Some(name) = name else {
                    i += 1;
                    continue;
                };
                // A function's body is its first brace group; a constant's runs to its `;`.
                let end = if word.as_deref() == Some("fn") {
                    tokens[i..].iter().position(|t| brace(t).is_some() || punct(Some(t), ';')).map_or(tokens.len() - 1, |n| i + n)
                } else {
                    tokens[i..].iter().position(|t| punct(Some(t), ';')).map_or(tokens.len() - 1, |n| i + n)
                };
                let mut body = BTreeSet::new();
                names(tokens[i + 2..=end].iter().cloned().collect(), &mut body);
                out.names.extend(body.iter().cloned());
                out.names.insert(name.clone());
                let key = match owner {
                    Some((ty, _)) => format!("{ty}::{name}"),
                    None => name.clone(),
                };
                let root = name == "main" || owner.is_some_and(|(_, by_trait)| by_trait);
                out.items.push(Item { key, public, root, names: body });
                i = end;
            }
            _ => match t {
                TokenTree::Group(g) => names(g.stream(), &mut out.names),
                TokenTree::Ident(id) => drop(out.names.insert(id.to_string())),
                _ => {}
            },
        }
        if !matches!(word.as_deref(), Some("async" | "unsafe" | "extern")) && !matches!(t, TokenTree::Literal(_)) {
            public = false;
        }
        i += 1;
    }
}

fn entries(text: &str) -> Vec<String> {
    text.lines().filter_map(|l| l.trim().strip_prefix("//")?.trim().strip_prefix("entry:").map(|id| id.trim().to_string())).collect()
}

fn sources(root: &Path) -> Vec<Source> {
    let mut files = Vec::new();
    // The acceptance harness is test support: its items serve tests, not a binary.
    let mut dirs: Vec<_> = std::fs::read_dir(root.join("crates"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path() != root.join(crate::ACCEPTANCE_DIR))
        .map(|e| e.path().join("src"))
        .collect();
    while let Some(dir) = dirs.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                files.push(p);
            }
        }
    }
    files.sort();
    files
        .into_iter()
        .filter_map(|p| {
            let text = std::fs::read_to_string(&p).ok()?;
            let tokens: TokenStream = text.parse().ok()?;
            let file = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
            let mut s = Source { file, entries: entries(&text), ..Source::default() };
            scan(tokens, None, &mut s);
            Some(s)
        })
        .collect()
}

/// The last segment of a key.
fn last(key: &str) -> &str {
    key.rsplit("::").next().unwrap_or(key)
}

/// The keys of `s` that non-test source reaches: those another file names — a method
/// `T::m` as that pair, or as `.m` in a file also naming `T` — plus `main` and trait-impl
/// methods, closed over the names each reached item's body holds.
fn reached(s: &Source, others: &[&Source]) -> BTreeSet<String> {
    let named = |key: &str| {
        others.iter().any(|o| {
            o.names.contains(key)
                || key.split_once("::").is_some_and(|(ty, m)| o.names.contains(ty) && o.names.contains(&format!(".{m}")))
        })
    };
    let mut reached: BTreeSet<String> = s.items.iter().filter(|it| it.root || named(&it.key)).map(|it| it.key.clone()).collect();
    loop {
        let more: Vec<String> = s
            .items
            .iter()
            .filter(|it| reached.contains(&it.key))
            .flat_map(|it| it.names.iter())
            .flat_map(|n| s.items.iter().filter(move |x| x.key == *n || last(&x.key) == n.trim_start_matches('.')))
            .map(|x| x.key.clone())
            .filter(|k| !reached.contains(k))
            .collect();
        if more.is_empty() {
            return reached;
        }
        reached.extend(more);
    }
}

/// The identifiers and path pairs in the body of the first function named `name`.
fn body_names(tokens: TokenStream, name: &str) -> Option<BTreeSet<String>> {
    let tokens: Vec<TokenTree> = tokens.into_iter().collect();
    for (i, t) in tokens.iter().enumerate() {
        if ident(Some(t)).as_deref() == Some("fn") && ident(tokens.get(i + 1)).as_deref() == Some(name) {
            let body = tokens[i + 2..].iter().find_map(brace)?;
            let mut out = BTreeSet::new();
            names(body, &mut out);
            return Some(out);
        }
        if let TokenTree::Group(g) = t {
            if let Some(found) = body_names(g.stream(), name) {
                return Some(found);
            }
        }
    }
    None
}

/// The file holding the unwired count's ceiling, which only falls (`corpus.state.unwired-ceiling`).
const CEILING: &str = "tools/ci/unwired.toml";

/// The recorded ceiling; an absent file admits none.
fn ceiling(root: &Path) -> Result<usize> {
    let Ok(text) = std::fs::read_to_string(root.join(CEILING)) else { return Ok(0) };
    let table: toml::Table = text.parse().with_context(|| format!("parsing {CEILING}"))?;
    Ok(table.get("ceiling").and_then(toml::Value::as_integer).and_then(|n| usize::try_from(n).ok()).unwrap_or(0))
}

/// `contextful-ci unwired [--contract <name>]… [--lower]`: report every unwired item of a
/// performed clause. With contracts named, refuse any of theirs; otherwise refuse a count
/// over the ceiling, and with `--lower` set the ceiling to a lower live count.
pub fn check(root: &Path, contracts: &[String], lower: bool) -> Result<()> {
    let Ok(text) = std::fs::read_to_string(root.join(LOCK)) else {
        println!("unwired: the tree holds no {LOCK}, so no clause performs");
        return Ok(());
    };
    let lock: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("parsing {LOCK}"))?;
    let sources = sources(root);
    // Each public key's defining files, and each file's reached keys.
    let mut home: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (n, s) in sources.iter().enumerate() {
        for it in s.items.iter().filter(|it| it.public) {
            home.entry(it.key.as_str()).or_default().push(n);
        }
    }
    let mut live: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
    let entries: BTreeSet<&str> = sources.iter().flat_map(|s| s.entries.iter().map(String::as_str)).collect();
    let mut found = BTreeSet::new();
    // Each clause maps to its pins: `kind`, `path`, `name`, `line`, and the clause's `verdict`.
    for (id, pins) in lock["pins"].as_object().into_iter().flatten() {
        let contract = id.split('.').next().unwrap_or_default();
        let pins = pins.as_array().map(Vec::as_slice).unwrap_or_default();
        let performed = pins.first().is_some_and(|p| p["verdict"] == "performed");
        if !performed || entries.contains(id.as_str()) || !(contracts.is_empty() || contracts.iter().any(|c| c == contract)) {
            continue;
        }
        for pin in pins.iter().filter(|p| p["kind"] == "test") {
            let (Some(file), Some(function)) = (pin["path"].as_str(), pin["name"].as_str()) else { continue };
            let site = format!("{file}::{function}");
            let Some(tokens) = file.ends_with(".rs").then(|| std::fs::read_to_string(root.join(file)).ok()).flatten().and_then(|t| t.parse::<TokenStream>().ok()) else {
                continue;
            };
            for name in body_names(tokens, function).unwrap_or_default() {
                let Some(&[n]) = home.get(name.as_str()).map(Vec::as_slice) else { continue };
                let reached = live.entry(n).or_insert_with(|| {
                    let others: Vec<&Source> = sources.iter().enumerate().filter(|(m, _)| *m != n).map(|(_, s)| s).collect();
                    reached(&sources[n], &others)
                });
                if !reached.contains(&name) {
                    found.insert(format!("{id}: {site} calls `{name}` ({}), which no non-test source reaches", sources[n].file));
                }
            }
        }
    }
    for f in &found {
        eprintln!("SpecUnwired: {f}");
    }
    let limit = if contracts.is_empty() { ceiling(root)? } else { 0 };
    if found.len() > limit {
        return Err(refuse("SpecUnwired", format!("{} item(s) only a performed pin's test calls, over the ceiling of {limit} in {CEILING}", found.len())));
    }
    if lower && found.len() < limit {
        let text = std::fs::read_to_string(root.join(CEILING)).unwrap_or_default();
        let mut lines: Vec<String> = text.lines().filter(|l| !l.trim_start().starts_with("ceiling")).map(str::to_string).collect();
        lines.push(format!("ceiling = {}", found.len()));
        std::fs::write(root.join(CEILING), lines.join("\n") + "\n").with_context(|| format!("writing {CEILING}"))?;
        println!("unwired: {CEILING} ceiling lowered from {limit} to {}", found.len());
    } else if found.len() < limit {
        println!("unwired: {} item(s) under the ceiling of {limit}; `contextful-ci unwired --lower` records the fall", found.len());
    } else {
        println!("unwired: {} item(s), at the ceiling of {limit}", found.len());
    }
    Ok(())
}
