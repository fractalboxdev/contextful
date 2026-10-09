//! Exclusion assertions over a collection the test never shows non-empty
//! (`assurance.test.vacuous-exclusion`, `assurance.test.presence-before-absence`).
//!
//! An exclusion assertion is `assert!(<c>.….all(…))` or `assert!(!<c>.….any(…))`: it holds
//! for every element, so it holds over none. The collection `<c>` is the receiver once the
//! iterator adapters are peeled. The assertion passes when the function, before it, or the
//! assertion itself, establishes presence: `<c>` followed by `.is_empty()`, `.len()`,
//! `.first()`, `.last()`, `.contains(`, `.get(`, an index, or an unnegated `.any(`, or `<c>`
//! compared by `assert_eq!`.

use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use syn::parse::Parser;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Expr, Macro, Token};

use crate::refuse;

/// Methods that turn a collection into an iterator over (some of) its elements.
const ADAPTERS: &[&str] = &[
    "iter", "into_iter", "iter_mut", "values", "keys", "values_mut", "windows", "chunks", "chars",
    "bytes", "lines", "split_whitespace", "filter", "map", "filter_map", "flat_map", "flatten",
    "skip", "take", "enumerate", "rev", "cloned", "copied", "as_slice", "as_bytes", "as_str",
    "as_array", "as_object", "unwrap",
];

/// Suffixes of `<c>` that establish its presence.
const PRESENCE: &[&str] = &[".len()", ".first()", ".last()", ".contains(", ".get(", "[", ".count()"];

/// Refuse when any of `files` (paths relative to `root`) holds an exclusion assertion with
/// no presence check, naming every site on stderr. A file paired with a line set is held
/// only on those lines: the lines a change adds or alters.
pub fn check(root: &Path, files: &[(String, Option<BTreeSet<usize>>)]) -> Result<()> {
    let mut sites = Vec::new();
    for (rel, lines) in files {
        let source = fs::read_to_string(root.join(rel)).with_context(|| format!("read {rel}"))?;
        sites.extend(
            sites_in(&source)
                .into_iter()
                .filter(|(line, _)| lines.as_ref().is_none_or(|held| held.contains(line)))
                .map(|(line, c)| format!("{rel}:{line}: no presence check of `{c}` precedes it")),
        );
    }
    if sites.is_empty() {
        eprintln!("vacuous: {} test file(s), no exclusion assertion over an unshown collection", files.len());
        return Ok(());
    }
    sites.iter().for_each(|s| eprintln!("VacuousAssertion: {s}"));
    Err(refuse("VacuousAssertion", format!("{} exclusion assertion(s) over a collection never shown non-empty, first {}", sites.len(), sites[0])))
}

/// Each `(line, collection)` of `source` whose exclusion assertion lacks a presence check.
pub fn sites_in(source: &str) -> Vec<(usize, String)> {
    let Ok(file) = syn::parse_file(source) else { return Vec::new() };
    let lines: Vec<&str> = source.lines().collect();
    let mut v = Fns { lines: &lines, out: Vec::new() };
    v.visit_file(&file);
    v.out
}

struct Fns<'a> {
    lines: &'a [&'a str],
    out: Vec<(usize, String)>,
}

impl<'ast> Visit<'ast> for Fns<'_> {
    fn visit_block(&mut self, block: &'ast syn::Block) {
        // Only an outermost function body opens a scope; nested blocks belong to it.
        let start = block.span().start();
        let mut asserts = Asserts::default();
        asserts.visit_block(block);
        for (at, collection, inline) in asserts.found {
            let before = text_between(self.lines, (start.line, start.column), at);
            if !(present(&before, &collection) || present(&inline, &collection)) {
                self.out.push((at.0, collection));
            }
        }
    }

    // Closures and nested fns are covered by the enclosing body's scan.
    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        self.visit_block(&f.block);
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        self.visit_block(&f.block);
    }
}

#[derive(Default)]
struct Asserts {
    /// `((line, column), collection, the assertion's other conjuncts)` per exclusion assertion.
    found: Vec<((usize, usize), String, String)>,
}

impl<'ast> Visit<'ast> for Asserts {
    fn visit_macro(&mut self, m: &'ast Macro) {
        if m.path.is_ident("assert") {
            let parser = syn::punctuated::Punctuated::<Expr, Token![,]>::parse_terminated;
            if let Ok(args) = parser.parse2(m.tokens.clone()) {
                if let Some(first) = args.first() {
                    let mut conjuncts = Vec::new();
                    split_and(first, &mut conjuncts);
                    for (i, c) in conjuncts.iter().enumerate() {
                        if let Some(collection) = exclusion(c) {
                            let start = m.span().start();
                            let others: String = conjuncts.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, o)| format!("{};", text(*o))).collect();
                            self.found.push(((start.line, start.column), collection, others));
                        }
                    }
                }
            }
        }
    }

    fn visit_item_fn(&mut self, _: &'ast syn::ItemFn) {}
}

fn split_and<'e>(e: &'e Expr, out: &mut Vec<&'e Expr>) {
    match e {
        Expr::Binary(b) if matches!(b.op, syn::BinOp::And(_)) => {
            split_and(&b.left, out);
            split_and(&b.right, out);
        }
        Expr::Paren(p) => split_and(&p.expr, out),
        other => out.push(other),
    }
}

/// The collection an exclusion assertion ranges over, or `None` for any other assertion.
fn exclusion(e: &Expr) -> Option<String> {
    let call = match e {
        Expr::MethodCall(m) if m.method == "all" => m,
        Expr::Unary(u) if matches!(u.op, syn::UnOp::Not(_)) => match &*u.expr {
            Expr::MethodCall(m) if m.method == "any" => m,
            _ => return None,
        },
        _ => return None,
    };
    let mut receiver = &*call.receiver;
    while let Expr::MethodCall(m) = receiver {
        if !ADAPTERS.contains(&m.method.to_string().as_str()) {
            break;
        }
        receiver = &m.receiver;
    }
    // A literal array or `vec!` of elements is present by construction.
    match receiver {
        Expr::Array(a) if !a.elems.is_empty() => return None,
        Expr::Reference(r) if matches!(&*r.expr, Expr::Array(a) if !a.elems.is_empty()) => return None,
        Expr::Macro(m) if m.mac.path.is_ident("vec") && !m.mac.tokens.is_empty() => return None,
        Expr::Lit(_) => return None,
        // A constant is a fixed collection the source shows whole.
        Expr::Path(p) if p.path.segments.last().is_some_and(|s| s.ident.to_string().chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')) => return None,
        _ => {}
    }
    Some(squash(&text(receiver)))
}

fn text(e: &impl Spanned) -> String {
    e.span().source_text().unwrap_or_default()
}

fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Whether `text` establishes that `collection` holds an element: `!<c>.is_empty()`, an
/// unnegated `<c>.….any(`, a length, an element or an index read, or `assert_eq!(<c>`.
fn present(text: &str, collection: &str) -> bool {
    let text = squash(text);
    let bare = collection.trim_start_matches('&');
    for c in [collection, bare] {
        if c.is_empty() {
            continue;
        }
        for (i, _) in text.match_indices(c) {
            let (before, after) = (&text[..i], &text[i + c.len()..]);
            if before.chars().last().is_some_and(|ch| ch.is_alphanumeric() || ch == '_' || ch == '.' || ch == ':') {
                continue;
            }
            let negated = before.ends_with('!');
            let found = match after {
                a if a.starts_with(".is_empty()") => negated,
                a if a.starts_with(".iter().any(") || a.starts_with(".into_iter().any(") => !negated,
                a => PRESENCE.iter().any(|p| a.starts_with(p)) || before.ends_with("assert_eq!("),
            };
            if found {
                return true;
            }
        }
    }
    false
}

/// The source text from `from` (1-based line, 0-based column) up to `to`.
fn text_between(lines: &[&str], from: (usize, usize), to: (usize, usize)) -> String {
    let mut out = String::new();
    for line in from.0..=to.0.min(lines.len()) {
        let l = lines.get(line - 1).copied().unwrap_or_default();
        let chars: Vec<char> = l.chars().collect();
        let a = if line == from.0 { from.1.min(chars.len()) } else { 0 };
        let b = if line == to.0 { to.1.min(chars.len()) } else { chars.len() };
        if a <= b {
            out.extend(&chars[a..b]);
        }
        out.push('\n');
    }
    out
}
