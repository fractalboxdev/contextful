//! `corpus.registry.drift`: the source sites that define a registered error, checked
//! against the registry entry that is the error's one home.

use crate::checks::{clause_verdict, pins_by_clause};
use crate::corpus::*;
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

/// Roots whose `<package>/src/` trees hold source, and the root holding Lean models.
const SOURCE_ROOTS: [&str; 3] = ["crates", "tools", "apps"];
const FORMAL_ROOT: &str = "formal";

static ENUM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?enum\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap());
static ERROR_ATTR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"#\[error\("([A-Z][A-Za-z0-9]*): "#).unwrap());
static VARIANT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*([A-Z][A-Za-z0-9_]*)\b").unwrap());
static POINTER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\(`[a-z0-9-]+\.[a-z0-9-]+\.[a-z0-9-]+`\)").unwrap());
static RUST_LITERAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""([A-Z][A-Za-z0-9]+)(?:"|: )"#).unwrap());
static TS_LITERAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"["'`]([A-Z][A-Za-z0-9]+)(?:["'`]|: )"#).unwrap());

/// One Rust variant whose `#[error]` text opens with an identifier.
struct Variant {
    id: String,
    name: String,
    owner: String,
    gloss: String,
    file: String,
    line: usize,
}

/// Every defining variant, and every file holding a string literal of each identifier.
struct Sites {
    variants: Vec<Variant>,
    literals: BTreeMap<String, BTreeSet<String>>,
}

fn sources(c: &Corpus) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for root in SOURCE_ROOTS.into_iter().chain([FORMAL_ROOT]) {
        let walk = walkdir::WalkDir::new(c.root.join(root)).sort_by_file_name().into_iter().filter_entry(|e| {
            let n = e.file_name().to_string_lossy();
            !matches!(n.as_ref(), "target" | "node_modules" | "dist" | ".lake")
        });
        for e in walk.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(&c.root).unwrap_or(p).to_string_lossy().replace('\\', "/");
            let ext = p.extension().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
            let in_src = rel.split('/').nth(2) == Some("src");
            let wanted = match ext.as_str() {
                "rs" => root != "apps" && root != FORMAL_ROOT && in_src,
                "ts" | "tsx" => root == "apps" && in_src,
                "lean" => root == FORMAL_ROOT,
                _ => false,
            };
            if !wanted {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(p) {
                out.push((rel, text));
            }
        }
    }
    out
}

fn scan(c: &Corpus) -> Sites {
    let mut sites = Sites { variants: Vec::new(), literals: BTreeMap::new() };
    for (rel, text) in sources(c) {
        let lines: Vec<&str> = text.lines().collect();
        let literal: &Regex = if rel.ends_with(".rs") || rel.ends_with(".lean") { &RUST_LITERAL } else { &TS_LITERAL };
        let mut owner = String::new();
        for (i, l) in lines.iter().enumerate() {
            let t = l.trim_start();
            if t.starts_with("//") || t.starts_with("--") || t.starts_with('*') || t.starts_with("/*") {
                continue;
            }
            for cap in literal.captures_iter(l) {
                sites.literals.entry(cap[1].to_string()).or_default().insert(rel.clone());
            }
            if !rel.ends_with(".rs") {
                continue;
            }
            if let Some(cap) = ENUM.captures(l) {
                owner = cap[1].to_string();
            }
            let Some(cap) = ERROR_ATTR.captures(l) else { continue };
            let name = lines[i + 1..].iter().map(|n| n.trim()).find(|n| !n.starts_with("#[")).and_then(|n| VARIANT.captures(n)).map(|v| v[1].to_string());
            sites.variants.push(Variant {
                id: cap[1].to_string(),
                name: name.unwrap_or_default(),
                owner: owner.clone(),
                gloss: gloss_above(&lines[..i]),
                file: rel.clone(),
                line: i + 1,
            });
        }
    }
    sites
}

/// The `///` text above a variant's attributes, up to its clause pointer or its first
/// blank doc line.
fn gloss_above(above: &[&str]) -> String {
    let mut doc: Vec<&str> = Vec::new();
    for l in above.iter().rev().map(|l| l.trim()) {
        if l.starts_with("#[") {
            continue;
        }
        let Some(text) = l.strip_prefix("///") else { break };
        doc.push(text.trim());
    }
    doc.reverse();
    let first: Vec<&str> = doc.into_iter().take_while(|l| !l.is_empty()).collect();
    let joined = first.join(" ");
    let cut = POINTER.find(&joined).map(|m| m.start()).unwrap_or(joined.len());
    joined[..cut].trim().to_string()
}

pub fn check(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let d = |file: &str, line: usize, msg: String| Finding::new("registry", file, line, "SpecRegistryDrift", msg);
    let sites = scan(c);
    let contract_of = |id: &str| -> Option<(&str, &ErrorEntry)> {
        c.reg.fragments.iter().find_map(|(k, fr)| fr.error.get(id).map(|e| (k.as_str(), e)))
    };
    let mut by_id: BTreeMap<&str, Vec<&Variant>> = BTreeMap::new();
    let mut by_enum: BTreeMap<(&str, &str), Vec<(&Variant, &str)>> = BTreeMap::new();
    for v in &sites.variants {
        let Some((contract, entry)) = contract_of(&v.id) else { continue };
        by_id.entry(&v.id).or_default().push(v);
        by_enum.entry((&v.file, &v.owner)).or_default().push((v, contract));
        let short = v.owner.strip_suffix("Error").and_then(|p| v.id.strip_prefix(p)).filter(|s| !s.is_empty());
        if v.name != v.id && Some(v.name.as_str()) != short {
            let want = short.map(|s| format!("`{}` or `{s}`", v.id)).unwrap_or_else(|| format!("`{}`", v.id));
            out.push(d(&v.file, v.line, format!("variant `{}::{}` defines `{}`; name it {want}", v.owner, v.name, v.id)));
        }
        if !v.gloss.is_empty() && v.gloss != entry.gloss {
            out.push(d(&v.file, v.line, format!("`{}` variant gloss `{}` differs from its registry gloss `{}`", v.id, v.gloss, entry.gloss)));
        }
    }
    for (id, vs) in &by_id {
        if let [first, second, ..] = vs.as_slice() {
            out.push(d(&second.file, second.line, format!("`{id}` is defined by two variants, at {}:{} and {}:{}", first.file, first.line, second.file, second.line)));
        }
    }
    for ((_, owner), vs) in &by_enum {
        let mut count: BTreeMap<&str, usize> = BTreeMap::new();
        for (_, k) in vs {
            *count.entry(k).or_default() += 1;
        }
        let Some((&home, _)) = count.iter().max_by_key(|(_, n)| **n) else { continue };
        for (v, k) in vs.iter().filter(|(_, k)| *k != home) {
            out.push(d(&v.file, v.line, format!("enum `{owner}` holds `{home}` errors and `{}` of `{k}`; move it to an enum of `{k}`", v.id)));
        }
    }
    let (committed, performed) = refusal_sites(c, &sites, &by_id);
    out.extend(performed);
    let live = committed_live(committed.len());
    match crate::ratchet::ceiling(c, &live.key) {
        Some(cap) if committed.len() <= cap => out.extend(crate::ratchet::compare(c, &live)),
        _ => out.extend(committed),
    }
    out
}

const COMMITTED_SITES: &str = "registry-committed-sites";

fn committed_live(count: usize) -> crate::ratchet::Live {
    crate::ratchet::Live { key: COMMITTED_SITES.into(), count, check: "registry", code: "SpecRegistryDrift" }
}

/// The `registry-committed-sites` ratchet: committed refusals whose error has a defining site.
pub fn committed_sites(c: &Corpus) -> crate::ratchet::Live {
    let sites = scan(c);
    let mut by_id: BTreeMap<&str, Vec<&Variant>> = BTreeMap::new();
    for v in &sites.variants {
        by_id.entry(&v.id).or_default().push(v);
    }
    committed_live(refusal_sites(c, &sites, &by_id).0.len())
}

/// Findings for committed refusals whose error has a defining site, and for performed
/// refusals whose error has none.
fn refusal_sites(c: &Corpus, sites: &Sites, by_id: &BTreeMap<&str, Vec<&Variant>>) -> (Vec<Finding>, Vec<Finding>) {
    let d = |file: &str, line: usize, msg: String| Finding::new("registry", file, line, "SpecRegistryDrift", msg);
    let (mut committed, mut performed) = (Vec::new(), Vec::new());
    let clauses = c.clause_map();
    let pins = pins_by_clause(c);
    for fr in c.reg.fragments.values() {
        for (id, e) in &fr.error {
            let Some(cl) = clauses.get(&e.clause) else { continue };
            let mut defined: BTreeSet<&str> = by_id.get(id.as_str()).map(|vs| vs.iter().map(|v| v.file.as_str()).collect()).unwrap_or_default();
            defined.extend(sites.literals.get(id).map(|fs| fs.iter().map(String::as_str).collect::<Vec<_>>()).unwrap_or_default());
            match pins.get(&e.clause) {
                None if !defined.is_empty() => {
                    let files: Vec<&str> = defined.into_iter().collect();
                    committed.push(d(&cl.file, cl.line, format!("committed refusal `{}` raises `{id}`, which {} defines; pin the clause", cl.id, files.join(", "))));
                }
                Some(group) if defined.is_empty() && clause_verdict(c, group) == "performed" => {
                    performed.push(d(&cl.file, cl.line, format!("performed refusal `{}` raises `{id}`, which no source defines", cl.id)));
                }
                _ => {}
            }
        }
    }
    (committed, performed)
}
