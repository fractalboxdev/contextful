//! Pin upkeep: where a pinned path is defined, `pins --migrate`, `retag`, and the clauses
//! a test file or a diff reaches (`slice --file`, `slice --diff`).

use crate::checks::{self, Pin};
use crate::corpus::*;
use crate::util::statement_rev;
use anyhow::{bail, Context, Result};
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;
use std::sync::LazyLock;

/// One definition a pinned path names.
#[derive(Clone, Debug)]
pub struct Site {
    /// Path relative to the corpus root.
    pub file: String,
    /// Byte offset of the line holding the definition.
    pub offset: usize,
    /// How many module segments of the pinned path the file's own path accounts for.
    pub score: usize,
}

fn walk(dir: &Path) -> impl Iterator<Item = walkdir::DirEntry> {
    walkdir::WalkDir::new(dir)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| {
            let n = e.file_name().to_string_lossy();
            n != "target" && n != "node_modules" && n != "dist"
        })
        .flatten()
}

fn rel(c: &Corpus, p: &Path) -> String {
    p.strip_prefix(&c.root).unwrap_or(p).to_string_lossy().replace('\\', "/")
}

/// The definition pattern for a pin kind, anchored at a line start.
fn definition(kind: &str, name: &str) -> Regex {
    let name = regex::escape(name);
    let vis = r"(?:pub(?:\([^)]*\))?\s+)?";
    let pat = match kind {
        "test" => format!(r"(?m)^[ \t]*{vis}(?:async\s+)?fn\s+(?:r#)?{name}\b"),
        "theorem" => format!(r"(?m)^[ \t]*(?:(?:private|protected)\s+)?(?:theorem|lemma)\s+{name}\b"),
        _ => format!(r"(?m)^[ \t]*{vis}(?:fn|struct|enum|trait|type|const|static|mod)\s+{name}\b"),
    };
    Regex::new(&pat).unwrap()
}

/// Whether a package directory answers to the crate identifier `ident`: by its directory
/// name, that name prefixed `contextful_`, or its manifest's package name.
fn answers(dir: &Path, ident: &str) -> bool {
    let name = dir.file_name().map(|n| n.to_string_lossy().replace('-', "_")).unwrap_or_default();
    if name == ident || format!("contextful_{name}") == ident {
        return true;
    }
    let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).ok().and_then(|t| t.parse::<toml::Table>().ok());
    manifest.and_then(|m| m.get("package")?.get("name")?.as_str().map(|n| n.replace('-', "_"))).as_deref() == Some(ident)
}

/// The module path a Rust file stands for inside its package: `src/a/b.rs` is `a::b`,
/// `tests/<target>/a/b.rs` is `a::b`, and a crate or target root is empty. A single-file
/// target `tests/x.rs` answers to both the empty path and `x`.
fn modules_of(in_package: &str) -> Vec<Vec<String>> {
    let strip = |parts: &[&str]| -> Vec<String> {
        let mut v: Vec<String> = parts.iter().map(|p| p.trim_end_matches(".rs").to_string()).collect();
        if v.last().is_some_and(|l| matches!(l.as_str(), "lib" | "main" | "mod")) {
            v.pop();
        }
        v
    };
    let parts: Vec<&str> = in_package.split('/').collect();
    match parts.first() {
        Some(&"src") => vec![strip(&parts[1..])],
        Some(&"tests") if parts.len() == 2 => vec![Vec::new(), strip(&parts[1..])],
        Some(&"tests") => vec![strip(&parts[2..])],
        _ => Vec::new(),
    }
}

/// Every definition the pinned `path` names under `roots`, best match first. The file or
/// module part of the path must agree with where the definition sits: a `<file>.rs::<fn>`
/// path names that file, a `<crate>::<module>::<fn>` path a file of that package whose
/// module path opens the pinned one, and a Lean `A.B.name` a file ending `A/B.lean`.
pub fn locate(c: &Corpus, kind: &str, path: &str, roots: &[&str]) -> Vec<Site> {
    let mut out = Vec::new();
    let mut found = |file: &Path, score: usize| {
        let Ok(text) = std::fs::read_to_string(file) else { return };
        let last = if kind == "theorem" { path.rsplit('.').next() } else { path.rsplit("::").next() };
        if let Some(m) = definition(kind, last.unwrap_or(path)).find(&text) {
            out.push(Site { file: rel(c, file), offset: m.start(), score });
        }
    };
    if kind == "theorem" {
        let (modules, _) = path.rsplit_once('.').unwrap_or(("", path));
        let suffix = format!("{}.lean", modules.replace('.', "/"));
        for root in roots {
            for e in walk(&c.root.join(root)) {
                let r = rel(c, e.path());
                if r.ends_with(".lean") && (modules.is_empty() || r.ends_with(&format!("/{suffix}"))) {
                    found(e.path(), 0);
                }
            }
        }
    } else if let Some((file, _)) = path.split_once(".rs::") {
        let file = c.root.join(format!("{file}.rs"));
        if roots.iter().any(|r| file.starts_with(c.root.join(r))) {
            found(&file, 0);
        }
    } else {
        let segs: Vec<&str> = path.split("::").collect();
        let (ident, modules) = match segs.as_slice() {
            [ident, modules @ .., _] => (*ident, modules),
            _ => return out,
        };
        let mut packages = Vec::new();
        for root in roots {
            let dir = c.root.join(root);
            if answers(&dir, ident) {
                packages.push(dir.clone());
            }
            for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                if e.path().is_dir() && answers(&e.path(), ident) {
                    packages.push(e.path());
                }
            }
        }
        for package in packages {
            for e in walk(&package) {
                let p = e.path();
                if p.extension().is_none_or(|x| x != "rs") {
                    continue;
                }
                let inside = p.strip_prefix(&package).unwrap_or(p).to_string_lossy().replace('\\', "/");
                let score = modules_of(&inside).into_iter().filter(|m| m.len() <= modules.len() && m.iter().zip(modules.iter()).all(|(a, b)| a == b)).map(|m| m.len()).max();
                if let Some(score) = score {
                    found(p, score);
                }
            }
        }
    }
    out.sort_by(|a, b| b.score.cmp(&a.score));
    out
}

/// Whether the attribute block directly above the line at `offset` holds `#[test]`.
pub fn is_test(text: &str, offset: usize) -> bool {
    for l in text[..offset].lines().rev().map(str::trim) {
        if l.is_empty() {
            continue;
        }
        if !(l.starts_with("#[") || l.starts_with("//")) {
            return false;
        }
        if l.starts_with("#[test]") {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------- migrate

static ENTRY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"^"([^"]+)"\s*=\s*\{\s*test\s*=\s*"([^"]+)"\s*\}\s*$"#).unwrap());

/// `pins --migrate`: rewrite each `test` entry of `spec/pins.toml` as a `// spec: <id>@<rev>`
/// tag above the one test it names, and drop the entry. An entry naming no clause, or no
/// single test, stays and is reported.
pub fn migrate(c: &Corpus) -> Result<()> {
    let path = c.root.join("spec/pins.toml");
    let text = std::fs::read_to_string(&path).context("reading spec/pins.toml")?;
    let clauses = c.clause_map();
    let mut inserts: BTreeMap<String, Vec<(usize, String)>> = BTreeMap::new();
    let mut kept = Vec::new();
    let mut moved = 0;
    for line in text.lines() {
        let Some(cap) = ENTRY.captures(line.trim()) else {
            kept.push(line.to_string());
            continue;
        };
        let (id, target) = (&cap[1], &cap[2]);
        let Some(cl) = clauses.get(id) else {
            eprintln!("migrate: `{id}` names no clause; the entry stays");
            kept.push(line.to_string());
            continue;
        };
        let sites: Vec<Site> = locate(c, "test", target, &checks::PIN_ROOTS)
            .into_iter()
            .filter(|s| std::fs::read_to_string(c.root.join(&s.file)).is_ok_and(|t| is_test(&t, s.offset)))
            .collect();
        let best: Vec<&Site> = sites.iter().filter(|s| Some(s.score) == sites.first().map(|f| f.score)).collect();
        let [site] = best.as_slice() else {
            eprintln!("migrate: `{id}` = `{target}` names {} tests; the entry stays", best.len());
            kept.push(line.to_string());
            continue;
        };
        inserts.entry(site.file.clone()).or_default().push((site.offset, format!("{id}@{}", statement_rev(&cl.statement))));
        moved += 1;
    }
    for (file, tags) in inserts {
        let full = c.root.join(&file);
        let mut src = std::fs::read_to_string(&full)?;
        let mut by_def: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
        for (offset, tag) in tags {
            by_def.entry(offset).or_default().insert(tag);
        }
        // From the last definition up, so an insertion moves no offset still to visit.
        for (offset, tags) in by_def.into_iter().rev() {
            let indent: String = src[offset..].chars().take_while(|ch| *ch == ' ' || *ch == '\t').collect();
            let mut at = offset;
            let mut present = BTreeSet::new();
            // Above the attribute block, below any doc comment.
            while let Some(prev) = src[..at].strip_suffix('\n').map(|s| s.rfind('\n').map(|n| n + 1).unwrap_or(0)) {
                let l = src[prev..at].trim();
                if !(l.starts_with("#[") || (l.starts_with("//") && !l.starts_with("///"))) {
                    break;
                }
                if let Some(tag) = l.strip_prefix("// spec:") {
                    present.insert(tag.trim().to_string());
                }
                at = prev;
            }
            let block: String = tags.difference(&present).map(|t| format!("{indent}// spec: {t}\n")).collect();
            src.insert_str(at, &block);
        }
        std::fs::write(&full, src)?;
    }
    let mut out = kept.join("\n");
    out.push('\n');
    std::fs::write(&path, out)?;
    println!("migrate: {moved} test entr{} moved to tags", if moved == 1 { "y" } else { "ies" });
    Ok(())
}

// ---------------------------------------------------------------- retag

/// The checks whose findings `retag` and regeneration repair.
const REPAIRED: [&str; 2] = ["state", "render"];

/// The statement `id` carried in the lock file committed at `HEAD`, when git can show it.
fn committed_statement(c: &Corpus, id: &str) -> Option<String> {
    let out = Command::new("git").args(["show", "HEAD:spec/spec.lock.json"]).current_dir(&c.root).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let lock: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    lock["clauses"].as_array()?.iter().find(|cl| cl["id"] == id)?["statement"].as_str().map(str::to_string)
}

/// Word-level similarity: twice the longest common subsequence over the summed lengths.
fn similarity(a: &str, b: &str) -> f64 {
    let (a, b): (Vec<&str>, Vec<&str>) = (a.split_whitespace().collect(), b.split_whitespace().collect());
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let mut row = vec![0usize; b.len() + 1];
    for x in &a {
        let mut prev = 0;
        for (j, y) in b.iter().enumerate() {
            let keep = row[j + 1];
            row[j + 1] = if x == y { prev + 1 } else { row[j + 1].max(row[j]) };
            prev = keep;
        }
    }
    2.0 * row[b.len()] as f64 / (a.len() + b.len()) as f64
}

/// `text` as `///` lines under `indent`, each at most `width` columns where words allow.
fn doc_lines(text: &str, indent: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = format!("{indent}///");
    for w in text.split_whitespace() {
        if line.len() > indent.len() + 3 && line.len() + 1 + w.len() > width {
            out.push(std::mem::replace(&mut line, format!("{indent}///")));
        }
        line.push(' ');
        line.push_str(w);
    }
    out.push(line);
    out
}

/// `retag <clause>…`: rewrite the rev on every tag of each clause to its statement's
/// current digest, and the `///` copy directly above a Rust tag when it reads at least 0.9
/// like the statement the tag recorded. Refuses while lint reports a finding outside the
/// checks retag and regeneration repair.
pub fn retag(c: &Corpus, ids: &[String]) -> Result<()> {
    let blocking: Vec<Finding> = checks::CHECKS.iter().filter(|n| !REPAIRED.contains(n)).flat_map(|n| checks::run(c, n)).collect();
    if !blocking.is_empty() {
        for f in &blocking {
            eprintln!("{}:{}  {}  {}", f.file, f.line, f.code, f.message);
        }
        bail!("retag: lint reports {} finding(s) retag does not repair", blocking.len());
    }
    let clauses = c.clause_map();
    let mut by_file: BTreeMap<String, Vec<(Pin, String, Option<String>)>> = BTreeMap::new();
    for id in ids {
        let Some(cl) = clauses.get(id) else { bail!("retag: `{id}` names no clause") };
        let rev = statement_rev(&cl.statement);
        let old = committed_statement(c, id);
        for pin in checks::scan_tags(c).into_iter().filter(|p| &p.clause == id) {
            let recorded = pin.tag.as_ref().map(|t| t.rev.clone()).unwrap_or_default();
            if recorded == rev {
                continue;
            }
            let old = old.clone().filter(|o| statement_rev(o) == recorded);
            by_file.entry(pin.file.clone()).or_default().push((pin, cl.statement.clone(), old));
        }
    }
    let mut count = 0;
    for (file, pins) in by_file {
        let full = c.root.join(&file);
        let mut lines: Vec<String> = std::fs::read_to_string(&full)?.lines().map(str::to_string).collect();
        let mut pins = pins;
        pins.sort_by(|a, b| b.0.line.cmp(&a.0.line));
        for (pin, statement, old) in pins {
            let at = pin.line - 1;
            let recorded = pin.tag.as_ref().map(|t| t.rev.as_str()).unwrap_or_default();
            let rev = statement_rev(&statement);
            lines[at] = lines[at].replacen(&format!("{}@{recorded}", pin.clause), &format!("{}@{rev}", pin.clause), 1);
            let top = (0..at).rev().take_while(|i| lines[*i].trim_start().starts_with("///")).last();
            let mut doc = "";
            if let Some(top) = top.filter(|_| file.ends_with(".rs")) {
                let copy: String = lines[top..at].iter().map(|l| l.trim_start().trim_start_matches("///").trim()).collect::<Vec<_>>().join(" ");
                let close = match &old {
                    Some(old) => similarity(&copy, old) >= 0.9,
                    None => statement_rev(&copy) == recorded || similarity(&copy, &statement) >= 0.9,
                };
                if close {
                    let indent: String = lines[top].chars().take_while(|ch| ch.is_whitespace()).collect();
                    let width = lines[top..at].iter().map(String::len).max().unwrap_or(0).max(100);
                    lines.splice(top..at, doc_lines(&statement, &indent, width));
                    doc = ", statement copy rewritten";
                }
            }
            println!("{}  {} @{recorded} -> @{rev}{doc}", pin.site(), pin.clause);
            count += 1;
        }
        let mut text = lines.join("\n");
        text.push('\n');
        std::fs::write(&full, text)?;
    }
    println!("retag: {count} tag(s) rewritten");
    Ok(())
}

// ---------------------------------------------------------------- reached clauses

/// A clause a test file or a diff reaches, with its current statement and the tagged
/// functions that reach it.
#[derive(Serialize)]
pub struct Reached {
    pub id: String,
    pub statement: String,
    pub sites: Vec<String>,
}

fn reached(c: &Corpus, pins: Vec<Pin>) -> Vec<Reached> {
    let clauses = c.clause_map();
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for p in pins {
        out.entry(p.clause.clone()).or_default().insert(p.site());
    }
    out.into_iter()
        .map(|(id, sites)| {
            let statement = clauses.get(&id).map(|cl| cl.statement.clone()).unwrap_or_else(|| "(names no clause)".into());
            Reached { id, statement, sites: sites.into_iter().collect() }
        })
        .collect()
}

/// `slice --file`: each clause the file's tags name.
pub fn by_file(c: &Corpus, file: &Path) -> Result<Vec<Reached>> {
    let full = if file.is_absolute() { file.to_path_buf() } else { c.root.join(file) };
    let text = std::fs::read_to_string(&full).with_context(|| format!("reading {}", full.display()))?;
    Ok(reached(c, checks::tags_in(c, &rel(c, &full), &text)))
}

static HUNK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^@@ -\S+ \+(\d+)(?:,(\d+))? @@").unwrap());

fn git(c: &Corpus, args: &[&str]) -> Result<String> {
    let out = Command::new("git").args(args).current_dir(&c.root).output().context("running git")?;
    if !out.status.success() {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The 1-based line span a tag covers: from the comment and attribute block holding it
/// through the end of the definition below it.
fn span(text: &str, pin: &Pin) -> (usize, usize) {
    let lines: Vec<&str> = text.lines().collect();
    let mut start = pin.line;
    while start > 1 {
        let l = lines[start - 2].trim();
        if !(l.starts_with("//") || l.starts_with("#[") || l.starts_with("--") || l.starts_with("@[")) {
            break;
        }
        start -= 1;
    }
    let Some(def) = (pin.line..lines.len()).find(|i| {
        let t = lines[*i].trim();
        !(t.is_empty() || t.starts_with("//") || t.starts_with("#[") || t.starts_with("--") || t.starts_with("@["))
    }) else {
        return (start, pin.line);
    };
    let offset: usize = lines[..def].iter().map(|l| l.len() + 1).sum();
    let end = if pin.file.ends_with(".lean") {
        (def + 1..lines.len()).find(|i| !lines[*i].is_empty() && !lines[*i].starts_with([' ', '\t'])).unwrap_or(lines.len())
    } else {
        let rest = &text[offset.min(text.len())..];
        let close = checks::body_range(rest).map(|r| (r.end + 1).min(rest.len())).unwrap_or(0);
        def + rest[..close].lines().count().max(1)
    };
    (start, end)
}

/// `slice --diff <rev>`: each clause whose tag, or the definition under it, a line the
/// working tree changes against the merge base of `rev` and `HEAD` falls within.
pub fn by_diff(c: &Corpus, rev: &str) -> Result<Vec<Reached>> {
    let base = git(c, &["merge-base", rev, "HEAD"]).map(|s| s.trim().to_string()).unwrap_or_else(|_| rev.to_string());
    let diff = git(c, &["-c", "core.quotepath=off", "diff", "-U0", "--no-color", "--no-ext-diff", &base, "--"])?;
    let mut changed: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    let mut file = None;
    for l in diff.lines() {
        if let Some(p) = l.strip_prefix("+++ ") {
            file = p.strip_prefix("b/").map(str::to_string);
        } else if let (Some(f), Some(h)) = (&file, HUNK.captures(l)) {
            let start: usize = h[1].parse().unwrap_or(0);
            let count: usize = h.get(2).map(|n| n.as_str().parse().unwrap_or(1)).unwrap_or(1);
            let range = if count == 0 { (start, start + 1) } else { (start, start + count - 1) };
            changed.entry(f.clone()).or_default().push(range);
        }
    }
    let mut pins = Vec::new();
    for (file, ranges) in changed {
        let Ok(text) = std::fs::read_to_string(c.root.join(&file)) else { continue };
        for pin in checks::tags_in(c, &file, &text) {
            let (a, b) = span(&text, &pin);
            if ranges.iter().any(|(x, y)| *x <= b && a <= *y) {
                pins.push(pin);
            }
        }
    }
    Ok(reached(c, pins))
}

pub fn markdown(heading: &str, rows: &[Reached]) -> String {
    let mut s = format!("# Clauses {heading}\n\n");
    if rows.is_empty() {
        s.push_str("None.\n");
    }
    for r in rows {
        let _ = writeln!(s, "- `{}` — {}", r.id, r.statement);
        for site in &r.sites {
            let _ = writeln!(s, "  - `{site}`");
        }
    }
    s
}
