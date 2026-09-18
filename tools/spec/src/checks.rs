//! One function per rule the corpus law states. Each reports
//! `<file>:<line>  <ErrorIdentifier>  <message>`.
//!
//! Where a clause names no error identifier of its own, the code reported is the
//! nearest registered one, or a `Spec…` name formed from the clause's subject; the
//! comment above each such check names the clause it implements.

use crate::corpus::*;
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

pub const KINDS: [&str; 6] = [
    "invariant", "refusal", "limit", "shape", "interface", "workflow",
];

pub const MODALS: [&str; 8] = [
    "must", "never", "refuses", "is refused", "at most", "at least", "exactly", "always",
];

pub const CHECKS: [&str; 25] = [
    "ids", "owns", "anatomy", "titles", "size", "terms", "aliases", "foreign", "literals",
    "limits", "errors", "shingles", "prose", "links", "rationale", "citations", "adr-orphans",
    "adr-shape", "tense", "unsettled", "banned", "paths", "diagrams", "pins", "roadmap",
];

fn word_re(words: &[&str]) -> Regex {
    let alts: Vec<String> = words.iter().map(|w| regex::escape(w)).collect();
    Regex::new(&format!(r"(?i)\b(?:{})\b", alts.join("|"))).unwrap()
}

/// Modal occurrences in a line, ignoring backticked spans.
fn modal_hits(s: &str) -> Vec<(usize, String)> {
    let plain = without_ticks(s);
    let re = word_re(&MODALS);
    re.find_iter(&plain)
        .map(|m| (m.start(), m.as_str().to_string()))
        .collect()
}

fn has_numeral(s: &str) -> bool {
    Regex::new(r"\d").unwrap().is_match(s)
}

// ---------------------------------------------------------------- 1. ids

pub fn ids(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let seg = Regex::new(r"^[a-z0-9-]+$").unwrap();
    let mut seen: HashMap<String, (String, usize)> = HashMap::new();

    for d in c.docs_for("ids") {
        let file_contract = d.contract.clone().unwrap_or_default();
        for cl in &d.clauses {
            let segs: Vec<&str> = cl.id.split('.').collect();
            if segs.len() != 4 || segs.iter().any(|s| !seg.is_match(s)) {
                out.push(Finding::new(
                    "ids", &cl.file, cl.line, "SpecClauseIdMalformed",
                    format!("`{}` is not four `[a-z0-9-]+` segments", cl.id),
                ));
                continue;
            }
            if cl.contract != file_contract {
                out.push(Finding::new(
                    "ids", &cl.file, cl.line, "SpecOwnsConflict",
                    format!(
                        "first segment `{}` is not the file's contract `{}`",
                        cl.contract, file_contract
                    ),
                ));
            }
            if !KINDS.contains(&cl.kind.as_str()) {
                out.push(Finding::new(
                    "ids", &cl.file, cl.line, "SpecClauseIdMalformed",
                    format!("third segment `{}` is not one of {}", cl.kind, KINDS.join("/")),
                ));
            }
            let key = format!("{}.{}", cl.contract, cl.operation);
            if !c.reg.operations.contains_key(&key) {
                out.push(Finding::new(
                    "ids", &cl.file, cl.line, "SpecOwnsConflict",
                    format!("`{}` is absent from operation.toml", key),
                ));
            }
            if c.reg.retired_ids.contains(&cl.id) {
                out.push(Finding::new(
                    "ids", &cl.file, cl.line, "SpecRetiredId",
                    format!("`{}` is listed under [retired]", cl.id),
                ));
            }
            match seen.get(&cl.id) {
                Some((f, l)) => out.push(Finding::new(
                    "ids", &cl.file, cl.line, "SpecDuplicateId",
                    format!("`{}` also extracts from {}:{}", cl.id, f, l),
                )),
                None => {
                    seen.insert(cl.id.clone(), (cl.file.clone(), cl.line));
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------- 2. owns

pub fn owns(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let docs = c.docs_for("owns");
    // operation key -> the files claiming it
    let mut claims: BTreeMap<String, Vec<(String, usize)>> = BTreeMap::new();

    for d in &docs {
        let contract = d.contract.clone().unwrap_or_default();
        for o in &d.owns {
            let key = format!("{}.{}", contract, o);
            let line = d
                .lines
                .iter()
                .find(|l| l.is_front_matter && l.raw.trim() == format!("- {}", o))
                .map(|l| l.no)
                .unwrap_or(1);
            if !c.reg.operations.contains_key(&key) {
                out.push(Finding::new(
                    "owns", &d.rel, line, "SpecOwnsConflict",
                    format!("front-matter `owns` entry `{}` has no operation.toml key", key),
                ));
                continue;
            }
            claims.entry(key).or_default().push((d.rel.clone(), line));
        }
    }

    for (key, who) in &claims {
        if who.len() > 1 {
            for (f, l) in who {
                out.push(Finding::new(
                    "owns", f, *l, "SpecOwnsConflict",
                    format!(
                        "`{}` is claimed by {} files of one contract: {}",
                        key,
                        who.len(),
                        who.iter().map(|(f, _)| f.as_str()).collect::<Vec<_>>().join(", ")
                    ),
                ));
            }
        }
    }

    // An operation the registry carries that no file of its contract claims.
    for (key, op) in &c.reg.operations {
        if claims.contains_key(key) {
            continue;
        }
        let file = op.file.clone().unwrap_or_else(|| "spec/terms/operation.toml".into());
        out.push(Finding::new(
            "owns", &file, 1, "SpecOwnsConflict",
            format!("operation `{}` is claimed by no file's `owns` list", key),
        ));
    }

    // A clause addressing an operation its own file does not own.
    for d in &docs {
        for cl in &d.clauses {
            let key = format!("{}.{}", cl.contract, cl.operation);
            if !c.reg.operations.contains_key(&key) {
                continue;
            }
            if !d.owns.iter().any(|o| o == &cl.operation) {
                out.push(Finding::new(
                    "owns", &cl.file, cl.line, "SpecOwnsConflict",
                    format!("`{}` addresses `{}`, which this file does not own", cl.id, key),
                ));
            }
        }
    }
    out
}

// ---------------------------------------------------------------- 3. anatomy

/// `corpus.anatomy.shape.file-headings` names no error identifier, so the heading
/// arm reports `SpecFileAnatomy`; the unaddressed-operation arm reports the
/// `SpecOwnsConflict` that `corpus.anatomy.refusal.unowned-operation` names.
pub fn anatomy(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let expected = ["Parties", "Operations", "Clauses", "Shapes", "Unsettled"];
    for d in c.docs_for("anatomy") {
        if d.contract.is_none() {
            out.push(Finding::new(
                "anatomy", &d.rel, 1, "SpecFileAnatomy",
                "front matter carries no `contract`".into(),
            ));
        }
        if d.owns.is_empty() {
            out.push(Finding::new(
                "anatomy", &d.rel, 1, "SpecFileAnatomy",
                "front matter carries no `owns` list".into(),
            ));
        }
        // Collapse the run of `## Clauses — <op>` headings into one Clauses section.
        let mut seq: Vec<(usize, String)> = Vec::new();
        for (no, level, text) in &d.headings {
            if *level != 2 {
                continue;
            }
            let name = if text.starts_with("Clauses") { "Clauses".to_string() } else { text.clone() };
            if seq.last().map(|(_, n)| n == &name).unwrap_or(false) {
                continue;
            }
            seq.push((*no, name));
        }
        let got: Vec<&str> = seq.iter().map(|(_, n)| n.as_str()).collect();
        if got != expected {
            let line = seq.first().map(|(n, _)| *n).unwrap_or(1);
            out.push(Finding::new(
                "anatomy", &d.rel, line, "SpecFileAnatomy",
                format!(
                    "top-level headings are [{}]; the law fixes [{}]",
                    got.join(", "),
                    expected.join(", ")
                ),
            ));
        }
        // Every operation the Operations table lists is addressed by a clause.
        let listed = operations_table(d);
        let addressed: HashSet<&str> = d.clauses.iter().map(|cl| cl.operation.as_str()).collect();
        for (line, op) in listed {
            if !addressed.contains(op.as_str()) {
                out.push(Finding::new(
                    "anatomy", &d.rel, line, "SpecOwnsConflict",
                    format!("operation `{}` is listed under Operations and addressed by no clause", op),
                ));
            }
        }
    }
    out
}

fn operations_table(d: &Doc) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let start = d
        .headings
        .iter()
        .find(|(_, lvl, t)| *lvl == 2 && t == "Operations")
        .map(|(n, _, _)| *n);
    let start = match start {
        Some(s) => s,
        None => return out,
    };
    for l in d.lines.iter().filter(|l| l.no > start) {
        if l.raw.starts_with("## ") {
            break;
        }
        if !l.is_table_row {
            continue;
        }
        let cells = split_cells(&l.raw);
        if cells.is_empty() {
            continue;
        }
        let first = cells[0].trim();
        if first.starts_with('`') && first.ends_with('`') {
            out.push((l.no, first.trim_matches('`').to_string()));
        }
    }
    out
}

fn split_cells(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut tick = false;
    for ch in s.trim().chars() {
        match ch {
            '`' => {
                tick = !tick;
                cur.push(ch);
            }
            '|' if !tick => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    out.push(cur.trim().to_string());
    out.retain(|x| !x.is_empty());
    out
}

// ---------------------------------------------------------------- 4. titles

pub fn titles(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    for d in c.docs_for("titles") {
        let h1: Vec<&(usize, usize, String)> =
            d.headings.iter().filter(|(_, lvl, _)| *lvl == 1).collect();
        if h1.len() != 1 {
            out.push(Finding::new(
                "titles", &d.rel, h1.first().map(|h| h.0).unwrap_or(1), "SpecTitleMismatch",
                format!("the file carries {} `# ` headings; the law fixes one", h1.len()),
            ));
            continue;
        }
        let (line, _, text) = h1[0];
        match c.contract_of_path(&d.rel) {
            None => out.push(Finding::new(
                "titles", &d.rel, *line, "SpecTitleMismatch",
                "no contract.toml entry lists this path".into(),
            )),
            Some(ce) => {
                let idx = ce.files.iter().position(|f| f == &d.rel).unwrap();
                let want = ce.titles.get(idx).unwrap_or(&ce.title);
                if want != text {
                    out.push(Finding::new(
                        "titles", &d.rel, *line, "SpecTitleMismatch",
                        format!("title is \"{}\"; contract.toml declares \"{}\"", text, want),
                    ));
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------- 5. size

pub fn size(c: &Corpus) -> Vec<Finding> {
    let limit = c.scope.check_int("size", "limit").unwrap_or(900) as usize;
    let mut out = Vec::new();
    for d in c.docs_for("size") {
        if d.line_count > limit {
            let ops: Vec<&str> = d.owns.iter().map(|s| s.as_str()).collect();
            out.push(Finding::new(
                "size", &d.rel, d.line_count, "SpecFileTooLong",
                format!(
                    "{} lines against a {} line ceiling; the operations that would relieve it are {}",
                    d.line_count, limit, ops.join(", ")
                ),
            ));
        }
    }
    out
}

// ---------------------------------------------------------------- 6. terms

/// `corpus.registry.invariant.vocabulary-is-registered`. The narrowest reading that
/// keeps false positives down: a bare identifier-shaped token, backticked, in two or
/// more *contract* files, that is not already a registered contract, operation, unit,
/// error, named bound, clause id, record number or file path.
pub fn terms(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut seen: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let ident = Regex::new(r"^[A-Za-z_][A-Za-z0-9_.\-]*$").unwrap();
    let numeric = Regex::new(r"^[0-9.]+$").unwrap();

    for d in c.docs_for("terms") {
        if d.role != "contract" {
            continue;
        }
        for l in &d.lines {
            if l.authored.is_empty() {
                continue;
            }
            for (_, tok) in tick_spans(&l.authored) {
                let t = tok.trim();
                if t.is_empty() || !ident.is_match(t) || numeric.is_match(t) {
                    continue;
                }
                if t.contains('/') || t.ends_with(".md") || t.ends_with(".toml") || t.ends_with(".json") {
                    continue;
                }
                if t.split('.').count() == 4 {
                    continue; // a clause id
                }
                if c.reg.terms.contains_key(t)
                    || c.reg.errors.contains_key(t)
                    || c.reg.limits.contains_key(t)
                    || c.reg.contracts.contains_key(t)
                    || c.reg.operations.contains_key(t)
                    || c.reg.unit_of(t).is_some()
                {
                    continue;
                }
                // `<contract>.<operation>` and bare operation spellings.
                if c.reg.operations.values().any(|o| o.operation == t) {
                    continue;
                }
                seen.entry(t.to_string())
                    .or_default()
                    .entry(d.rel.clone())
                    .or_insert(l.no);
            }
        }
    }

    for (tok, files) in seen {
        if files.len() < 2 {
            continue;
        }
        let (file, line) = files.iter().next().map(|(f, l)| (f.clone(), *l)).unwrap();
        let owner = c
            .contract_of_path(&file)
            .map(|x| x.name.clone())
            .unwrap_or_else(|| "TODO".into());
        out.push(Finding::new(
            "terms", &file, line, "SpecUnregisteredTerm",
            format!(
                "`{}` appears in {} contract files with no [term] entry — add: {} = {{ gloss = \"TODO\", owner = \"{}\", resolves = \"concept\" }}",
                tok, files.len(), tok, owner
            ),
        ));
    }
    out
}

// ---------------------------------------------------------------- 7. aliases

/// `corpus.registry.refusal.alias-in-a-statement`. A registered alias is often an
/// ordinary English word (`grant`, `render`, `pin`), so the alias arm fires only where
/// the alias is used *as a term* — backticked — and is not itself a canonical spelling.
pub fn aliases(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut alias_of: HashMap<String, String> = HashMap::new();
    for (key, op) in &c.reg.operations {
        for a in &op.aliases {
            alias_of.entry(a.to_ascii_lowercase()).or_insert(key.clone());
        }
    }
    for (name, t) in &c.reg.terms {
        for a in &t.aliases {
            alias_of.entry(a.to_ascii_lowercase()).or_insert(name.clone());
        }
    }
    // A unit alias (`bytes`, `MB`) is a unit spelling only beside a numeral; standing
    // alone in a code span it is a type name, so it is held separately.
    let mut unit_alias_of: HashMap<String, String> = HashMap::new();
    for (name, u) in &c.reg.units {
        for a in &u.aliases {
            unit_alias_of.entry(a.to_ascii_lowercase()).or_insert(name.clone());
        }
    }
    let canonical: HashSet<String> = c
        .reg
        .terms
        .keys()
        .chain(c.reg.errors.keys())
        .chain(c.reg.units.keys())
        .chain(c.reg.limits.keys())
        .chain(c.reg.contracts.keys())
        .map(|s| s.to_ascii_lowercase())
        .chain(c.reg.operations.values().map(|o| o.operation.to_ascii_lowercase()))
        .collect();

    for d in c.docs_for("aliases") {
        for cl in &d.clauses {
            let plain = without_ticks(&cl.statement);
            for (uni, canon) in &unit_alias_of {
                let re = Regex::new(&format!(r"(?i)\b\d+(?:\.\d+)?\s*{}\b", regex::escape(uni)))
                    .unwrap();
                if re.is_match(&plain) {
                    out.push(Finding::new(
                        "aliases", &cl.file, cl.line, "SpecAliasUsed",
                        format!("the unit is spelled `{}`; the canonical spelling is `{}`", uni, canon),
                    ));
                }
            }
            for (_, tok) in tick_spans(&cl.statement) {
                let low = tok.trim().to_ascii_lowercase();
                if canonical.contains(&low) {
                    continue;
                }
                if let Some(canon) = alias_of.get(&low) {
                    out.push(Finding::new(
                        "aliases", &cl.file, cl.line, "SpecAliasUsed",
                        format!("`{}` is a registered alias; the canonical spelling is `{}`", tok.trim(), canon),
                    ));
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------- 8. foreign

pub fn foreign(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let contracts: HashSet<&str> = c.reg.contracts.keys().map(|s| s.as_str()).collect();
    let status = Regex::new(r"\b(?:HTTP\s+)?([45][0-9][0-9])\b").unwrap();

    for d in c.docs_for("foreign") {
        for cl in &d.clauses {
            // Arm (a) — the subject segment names a term another contract owns.
            if let Some(t) = c.reg.terms.get(&cl.subject) {
                if !t.owner.is_empty() && t.owner != cl.contract && contracts.contains(t.owner.as_str()) {
                    out.push(Finding::new(
                        "foreign", &cl.file, cl.line, "SpecForeignAssertion",
                        format!("subject segment `{}` is a term `{}` owns", cl.subject, t.owner),
                    ));
                }
            }

            let stmt = &cl.statement;
            let spans = tick_spans(stmt);
            let foreign_terms: Vec<(usize, String, String)> = spans
                .iter()
                .filter_map(|(at, tok)| {
                    c.reg.terms.get(tok.trim()).and_then(|t| {
                        if !t.owner.is_empty()
                            && t.owner != cl.contract
                            && contracts.contains(t.owner.as_str())
                        {
                            Some((*at, tok.trim().to_string(), t.owner.clone()))
                        } else {
                            None
                        }
                    })
                })
                .collect();

            // Arm (b) — the registered term nearest before the statement's modal.
            if let Some((mpos, modal)) = modal_hits(stmt).into_iter().next() {
                let nearest = spans
                    .iter()
                    .filter(|(at, _)| *at < mpos)
                    .filter(|(_, tok)| c.reg.terms.contains_key(tok.trim()))
                    .last();
                if let Some((_, tok)) = nearest {
                    let t = &c.reg.terms[tok.trim()];
                    if !t.owner.is_empty() && t.owner != cl.contract && contracts.contains(t.owner.as_str()) {
                        out.push(Finding::new(
                            "foreign", &cl.file, cl.line, "SpecForeignAssertion",
                            format!(
                                "the term nearest before \"{}\" is `{}`, which `{}` owns",
                                modal, tok.trim(), t.owner
                            ),
                        ));
                    }
                }
            }

            // Arm (c) — a foreign term sharing the statement with a bound, an error
            // identifier or a status code this clause does not own.
            if let Some((_, tok, owner)) = foreign_terms.first() {
                let mut companion = None;
                if let Some(u) = numeral_unit_pairs(c, stmt).first() {
                    companion = Some(format!("the bound {} {}", u.0, u.1));
                }
                if companion.is_none() {
                    for (_, t) in &spans {
                        if let Some(e) = c.reg.errors.get(t.trim()) {
                            if e.owner != cl.contract {
                                companion = Some(format!("the error identifier `{}`", t.trim()));
                                break;
                            }
                        }
                    }
                }
                if companion.is_none() {
                    for (_, t) in &spans {
                        if let Some(m) = status.captures(t.trim()) {
                            companion = Some(format!("the status code {}", &m[1]));
                            break;
                        }
                    }
                }
                if let Some(comp) = companion {
                    out.push(Finding::new(
                        "foreign", &cl.file, cl.line, "SpecForeignAssertion",
                        format!("`{}`, which `{}` owns, shares the statement with {}", tok, owner, comp),
                    ));
                }
            }
        }
    }
    out
}

/// A value in its unit family's base unit, so "64 KiB" and 65536 B compare equal.
fn normalize(v: f64, unit: &str) -> Option<f64> {
    let f = match unit {
        "B" => 1.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        "ms" => 1.0,
        "s" => 1000.0,
        "min" => 60_000.0,
        "h" => 3_600_000.0,
        "d" => 86_400_000.0,
        _ => return None,
    };
    Some(v * f)
}

/// Numeral-and-unit pairs in a statement, reading past backticked spans.
fn numeral_unit_pairs(c: &Corpus, stmt: &str) -> Vec<(String, String)> {
    let plain = without_ticks(stmt);
    let re = Regex::new(r"(\d+(?:\.\d+)?)\s*(%|[A-Za-z]+)").unwrap();
    let mut out = Vec::new();
    for m in re.captures_iter(&plain) {
        if let Some(u) = c.reg.unit_of(&m[2]) {
            out.push((m[1].to_string(), u.to_string()));
        }
    }
    out
}

// ---------------------------------------------------------------- 9. literals

/// `corpus.registry.invariant.one-named-bound-one-owner`. Matching is on the numeral
/// alone — an author writes "900 lines" where the entry records `unit = "rows"`, and
/// holding the prose to the registry's unit token is the `limits` check's job.
pub fn literals(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let ids = c.clause_ids();
    let re = Regex::new(r"(\d+(?:\.\d+)?)\s*(%|[A-Za-z]+)").unwrap();

    for d in c.docs_for("literals") {
        for cl in &d.clauses {
            if cl.kind != "limit" {
                continue;
            }
            let plain = without_ticks(&cl.statement);
            let mut reported: HashSet<String> = HashSet::new();
            for m in re.captures_iter(&plain) {
                let num: f64 = match m[1].parse() {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let unit = match c.reg.unit_of(&m[2]) {
                    Some(u) => u.to_string(),
                    None => continue, // not a numeral-and-unit pair; `limits` owns that arm
                };
                if !reported.insert(format!("{} {}", &m[1], unit)) {
                    continue;
                }
                let want = normalize(num, &unit);
                let owned: Vec<&LimitEntry> = c
                    .reg
                    .limits
                    .values()
                    .filter(|e| {
                        e.owner == cl.id
                            && match (want, normalize(e.value, &e.unit)) {
                                (Some(a), Some(b)) => (a - b).abs() < 1e-6,
                                _ => (e.value - num).abs() < f64::EPSILON,
                            }
                    })
                    .collect();
                if owned.is_empty() {
                    out.push(Finding::new(
                        "literals", &cl.file, cl.line, "SpecUnmeasuredLimit",
                        format!(
                            "the pair \"{} {}\" resolves to no limit.toml entry owned by `{}`",
                            &m[1], &m[2], cl.id
                        ),
                    ));
                } else if owned.len() > 1 {
                    out.push(Finding::new(
                        "literals", &cl.file, cl.line, "SpecUnmeasuredLimit",
                        format!(
                            "the pair \"{} {}\" resolves to {} entries owned by `{}`: {}",
                            &m[1], &m[2], owned.len(), cl.id,
                            owned.iter().map(|e| e.name.as_str()).collect::<Vec<_>>().join(", ")
                        ),
                    ));
                }
            }
        }
    }

    // Every named bound is asserted by one clause that extracts.
    for e in c.reg.limits.values() {
        if e.owner.is_empty() {
            out.push(Finding::new(
                "literals", "spec/terms/limit.toml", 1, "SpecUnmeasuredLimit",
                format!("named bound `{}` carries no owner", e.name),
            ));
        } else if !ids.contains(&e.owner) {
            out.push(Finding::new(
                "literals", "spec/terms/limit.toml", 1, "SpecUnmeasuredLimit",
                format!("named bound `{}` names owner `{}`, which extracts from no file", e.name, e.owner),
            ));
        }
    }
    out
}

// ---------------------------------------------------------------- 10. limits

pub fn limits(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let words = [
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
        "eleven", "twelve", "twenty", "thirty", "sixty", "hundred", "thousand",
    ];
    let unit_tokens: Vec<String> = c
        .reg
        .units
        .iter()
        .flat_map(|(n, u)| {
            std::iter::once(n.clone()).chain(u.aliases.iter().cloned())
        })
        .collect();
    let unit_re = word_re(&unit_tokens.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    let word_num = Regex::new(&format!(
        r"(?i)\b({})\s+({})\b",
        words.join("|"),
        unit_tokens.iter().map(|s| regex::escape(s)).collect::<Vec<_>>().join("|")
    ))
    .unwrap();

    for d in c.docs_for("limits") {
        for cl in &d.clauses {
            if cl.kind != "limit" {
                continue;
            }
            let plain = without_ticks(&cl.statement);
            if !has_numeral(&plain) {
                out.push(Finding::new(
                    "limits", &cl.file, cl.line, "SpecUnmeasuredLimit",
                    format!("`{}` is a limit row carrying no numeral outside a code span", cl.id),
                ));
                continue;
            }
            if !unit_re.is_match(&plain) {
                out.push(Finding::new(
                    "limits", &cl.file, cl.line, "SpecUnmeasuredLimit",
                    format!("`{}` carries a numeral and no unit registered in unit.toml", cl.id),
                ));
            }
            if let Some(m) = word_num.captures(&plain) {
                out.push(Finding::new(
                    "limits", &cl.file, cl.line, "SpecUnmeasuredLimit",
                    format!("`{}` spells a numeral as a word: \"{}\"", cl.id, &m[0]),
                ));
            }
        }
    }
    out
}

// ---------------------------------------------------------------- 11. errors

pub fn errors(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let camel = Regex::new(r"\b[A-Z][A-Za-z0-9]{4,}\b").unwrap();
    for d in c.docs_for("errors") {
        for cl in &d.clauses {
            if cl.kind != "refusal" {
                continue;
            }
            let named = tick_spans(&cl.statement)
                .iter()
                .any(|(_, t)| c.reg.errors.contains_key(t.trim()))
                || camel
                    .find_iter(&cl.statement)
                    .any(|m| c.reg.errors.contains_key(m.as_str()));
            if !named {
                out.push(Finding::new(
                    "errors", &cl.file, cl.line, "SpecUnnamedRefusal",
                    format!("`{}` is a refusal row naming no identifier in error.toml", cl.id),
                ));
            }
        }
    }
    out
}

// ---------------------------------------------------------------- 12. shingles

pub fn shingles(c: &Corpus) -> Vec<Finding> {
    let n = c.scope.check_int("shingles", "gram").unwrap_or(8) as usize;
    let mut out = Vec::new();
    let mut index: HashMap<String, (String, String, usize)> = HashMap::new();
    let mut reported: HashSet<(String, String)> = HashSet::new();

    for d in c.docs_for("shingles") {
        for cl in &d.clauses {
            let toks = normalize_tokens(&cl.statement);
            if toks.len() < n {
                continue;
            }
            let mut local: HashSet<String> = HashSet::new();
            for w in toks.windows(n) {
                let gram = w.join(" ");
                if !local.insert(gram.clone()) {
                    continue;
                }
                match index.get(&gram) {
                    Some((other_id, other_file, other_line)) if other_id != &cl.id => {
                        let pair = if other_id < &cl.id {
                            (other_id.clone(), cl.id.clone())
                        } else {
                            (cl.id.clone(), other_id.clone())
                        };
                        if reported.insert(pair) {
                            out.push(Finding::new(
                                "shingles", &cl.file, cl.line, "SpecRestatement",
                                format!(
                                    "`{}` shares the {}-gram \"{}\" with `{}` ({}:{})",
                                    cl.id, n, gram, other_id, other_file, other_line
                                ),
                            ));
                        }
                    }
                    Some(_) => {}
                    None => {
                        index.insert(gram, (cl.id.clone(), cl.file.clone(), cl.line));
                    }
                }
            }
        }
    }
    out
}

fn normalize_tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect()
}

// ---------------------------------------------------------------- 13. prose

pub fn prose(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let only_re = Regex::new(r"(?i)\bonly\b").unwrap();
    for d in c.docs_for("prose") {
        for l in &d.lines {
            if l.authored.is_empty() || l.is_clause_row || l.is_unsettled {
                continue;
            }
            let plain = without_ticks(&l.authored);
            for (_, modal) in modal_hits(&l.authored) {
                out.push(Finding::new(
                    "prose", &d.rel, l.no, "SpecStrayModal",
                    format!("\"{}\" outside a clause cell", modal),
                ));
            }
            if only_re.is_match(&plain) {
                let carries_term = tick_spans(&l.authored)
                    .iter()
                    .any(|(_, t)| c.reg.terms.contains_key(t.trim()));
                if carries_term {
                    out.push(Finding::new(
                        "prose", &d.rel, l.no, "SpecStrayModal",
                        "\"only\" in a sentence that also carries a registered term".into(),
                    ));
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------- 14. links

pub fn links(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let ids = c.clause_ids();
    let trans = Regex::new(r"\{\{([^}]+)\}\}").unwrap();
    let termlink = Regex::new(r"\[\[([^\]]+)\]\]").unwrap();
    // A pointer sentence is one that names another file's section.
    let pointer = Regex::new(r"spec/[0-9A-Za-z\-]+\.md\s*§").unwrap();

    for d in c.docs_for("links") {
        for l in &d.lines {
            if l.is_front_matter || l.in_fence {
                continue;
            }
            let bare = without_ticks(&l.raw);
            for m in trans.captures_iter(&bare) {
                let id = m[1].trim();
                if !ids.contains(id) {
                    out.push(Finding::new(
                        "links", &d.rel, l.no, "SpecDanglingReference",
                        format!("{{{{{}}}}} names no clause", id),
                    ));
                }
            }
            for m in termlink.captures_iter(&bare) {
                let t = m[1].trim();
                if !c.reg.terms.contains_key(t)
                    && !c.reg.errors.contains_key(t)
                    && !c.reg.limits.contains_key(t)
                    && !c.reg.operations.contains_key(t)
                    && !c.reg.contracts.contains_key(t)
                {
                    out.push(Finding::new(
                        "links", &d.rel, l.no, "SpecDanglingReference",
                        format!("[[{}]] names no registry entry", t),
                    ));
                }
            }
            if l.authored.is_empty() || l.is_clause_row {
                continue;
            }
            if pointer.is_match(&l.authored) {
                let plain = without_ticks(&l.authored);
                let mut why: Vec<String> = Vec::new();
                if has_numeral(&plain) {
                    why.push("a numeral".into());
                }
                if let Some((_, modal)) = modal_hits(&l.authored).into_iter().next() {
                    why.push(format!("the modal \"{}\"", modal));
                }
                if let Some((_, e)) = tick_spans(&l.authored)
                    .into_iter()
                    .find(|(_, t)| c.reg.errors.contains_key(t.trim()))
                {
                    why.push(format!("the error identifier `{}`", e.trim()));
                }
                if !why.is_empty() {
                    out.push(Finding::new(
                        "links", &d.rel, l.no, "SpecPointerSentence",
                        format!("a pointer sentence carrying {}", why.join(" and ")),
                    ));
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------- 15. rationale

pub fn rationale(c: &Corpus) -> Vec<Finding> {
    let tokens = [
        "because", "so that", "in order to", "the reason", "which is why", "judged on",
        "at the cost of", "trade-off",
    ];
    let re = word_re(&tokens);
    let mut out = Vec::new();
    for d in c.docs_for("rationale") {
        for l in &d.lines {
            if l.authored.is_empty() {
                continue;
            }
            let plain = without_ticks(&l.authored);
            for m in re.find_iter(&plain) {
                out.push(Finding::new(
                    "rationale", &d.rel, l.no, "SpecRationaleLeak",
                    format!("\"{}\" outside spec/decisions/", m.as_str()),
                ));
            }
        }
    }
    out
}

// ---------------------------------------------------------------- 16. citations

pub fn citations(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let records: HashSet<String> = c
        .records()
        .iter()
        .filter_map(|d| {
            d.rel
                .rsplit('/')
                .next()
                .and_then(|f| f.split('-').next())
                .map(|s| s.to_string())
        })
        .collect();
    let four = Regex::new(r"^[0-9]{4}$").unwrap();

    for d in c.docs_for("citations") {
        for cl in &d.clauses {
            let cite = cl.decided_by.trim();
            match cl.kind.as_str() {
                "refusal" => {
                    if cite.is_empty() {
                        out.push(Finding::new(
                            "citations", &cl.file, cl.line, "SpecUncitedRefusal",
                            format!("`{}` is a refusal row with an empty `decided-by` cell", cl.id),
                        ));
                    } else if !four.is_match(cite) {
                        out.push(Finding::new(
                            "citations", &cl.file, cl.line, "SpecUncitedRefusal",
                            format!("`{}` cites \"{}\", which is not a four-digit record", cl.id, cite),
                        ));
                    } else if !records.contains(cite) {
                        out.push(Finding::new(
                            "citations", &cl.file, cl.line, "SpecUncitedRefusal",
                            format!("`{}` cites record {}, which is absent from spec/decisions/", cl.id, cite),
                        ));
                    }
                }
                "limit" => {
                    if !cite.is_empty() {
                        if four.is_match(cite) && !records.contains(cite) {
                            out.push(Finding::new(
                                "citations", &cl.file, cl.line, "SpecUncitedRefusal",
                                format!("`{}` cites record {}, which is absent from spec/decisions/", cl.id, cite),
                            ));
                        }
                        continue;
                    }
                    // Transitive: the citation is inherited through the named bound.
                    let owned: Vec<&LimitEntry> =
                        c.reg.limits.values().filter(|e| e.owner == cl.id).collect();
                    if owned.is_empty() {
                        out.push(Finding::new(
                            "citations", &cl.file, cl.line, "SpecUncitedRefusal",
                            format!(
                                "`{}` carries no citation and owns no limit.toml entry to inherit one through",
                                cl.id
                            ),
                        ));
                    }
                }
                _ => {
                    if !cite.is_empty() && four.is_match(cite) && !records.contains(cite) {
                        out.push(Finding::new(
                            "citations", &cl.file, cl.line, "SpecUncitedRefusal",
                            format!("`{}` cites record {}, which is absent from spec/decisions/", cl.id, cite),
                        ));
                    }
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------- record parsing

pub struct Record {
    pub rel: String,
    pub number: String,
    pub decides: Vec<String>,
    pub decides_line: usize,
    pub status_line: Option<(usize, String)>,
    pub headings: Vec<String>,
    pub superseded_by: Option<String>,
}

pub fn records(c: &Corpus) -> Vec<Record> {
    let cite = Regex::new(r"`([a-z0-9\-]+(?:\.[a-z0-9\-]+){3})`").unwrap();
    let sup = Regex::new(r"(?i)superseded by\s+`?([0-9]{4})").unwrap();
    c.records()
        .iter()
        .map(|d| {
            let number = d
                .rel
                .rsplit('/')
                .next()
                .and_then(|f| f.split('-').next())
                .unwrap_or("")
                .to_string();
            let mut decides = Vec::new();
            let mut decides_line = 1;
            let mut status_line = None;
            let mut superseded_by = None;
            for l in &d.lines {
                let t = l.raw.trim();
                if t.starts_with("**Decides:**") {
                    decides_line = l.no;
                    for m in cite.captures_iter(t) {
                        decides.push(m[1].to_string());
                    }
                }
                if t.starts_with("**Status:**") {
                    status_line = Some((l.no, t.to_string()));
                    if let Some(m) = sup.captures(t) {
                        superseded_by = Some(m[1].to_string());
                    }
                }
            }
            Record {
                rel: d.rel.clone(),
                number,
                decides,
                decides_line,
                status_line,
                headings: d.headings.iter().map(|(_, _, t)| t.clone()).collect(),
                superseded_by,
            }
        })
        .collect()
}

// ---------------------------------------------------------------- 17. adr-orphans

pub fn adr_orphans(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let ids = c.clause_ids();
    let cited: HashSet<String> = c
        .all_clauses()
        .iter()
        .map(|cl| cl.decided_by.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let recs = records(c);
    let superseding: HashSet<String> = recs
        .iter()
        .filter(|r| cited.contains(&r.number))
        .filter_map(|r| r.superseded_by.clone())
        .collect();

    for r in &recs {
        if !cited.contains(&r.number) && !superseding.contains(&r.number) {
            out.push(Finding::new(
                "adr-orphans", &r.rel, 1, "SpecOrphanRecord",
                format!("record {} is cited by no clause and superseded by no citing record", r.number),
            ));
        }
        for id in &r.decides {
            if !ids.contains(id) {
                out.push(Finding::new(
                    "adr-orphans", &r.rel, r.decides_line, "SpecOrphanRecord",
                    format!("`Decides:` names `{}`, which extracts from no file", id),
                ));
            }
        }
        let spans: BTreeSet<&str> = r
            .decides
            .iter()
            .filter_map(|id| id.split('.').next())
            .collect();
        if spans.len() > 2 {
            out.push(Finding::new(
                "adr-orphans", &r.rel, r.decides_line, "SpecOrphanRecord",
                format!(
                    "`Decides:` spans {} contracts: {}",
                    spans.len(),
                    spans.into_iter().collect::<Vec<_>>().join(", ")
                ),
            ));
        }
    }
    out
}

// ---------------------------------------------------------------- 18. adr-shape

pub fn adr_shape(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let dated = Regex::new(r"\b[0-9]{4}-[0-9]{2}-[0-9]{2}\b").unwrap();
    let want = ["Context", "Decision", "Options considered", "Criteria", "Consequences"];
    for d in c.docs_for("adr-shape") {
        let r = records(c).into_iter().find(|r| r.rel == d.rel);
        let r = match r {
            Some(r) => r,
            None => continue,
        };
        match &r.status_line {
            None => out.push(Finding::new(
                "adr-shape", &d.rel, 1, "SpecRecordAnatomy",
                "no `**Status:**` line".into(),
            )),
            Some((no, text)) => {
                if !dated.is_match(text) {
                    out.push(Finding::new(
                        "adr-shape", &d.rel, *no, "SpecRecordAnatomy",
                        "the Status line carries no date".into(),
                    ));
                }
            }
        }
        for w in want {
            if !r.headings.iter().any(|h| h.eq_ignore_ascii_case(w)) {
                out.push(Finding::new(
                    "adr-shape", &d.rel, 1, "SpecRecordAnatomy",
                    format!("no `{}` section", w),
                ));
            }
        }
        // Every rejected option names the criterion it lost on.
        let start = d
            .headings
            .iter()
            .find(|(_, _, t)| t.eq_ignore_ascii_case("Options considered"))
            .map(|(n, _, _)| *n);
        if let Some(start) = start {
            let mut seen_header = false;
            for l in d.lines.iter().filter(|l| l.no > start) {
                if l.raw.starts_with("## ") {
                    break;
                }
                if !l.is_table_row {
                    continue;
                }
                if !seen_header {
                    seen_header = true;
                    continue;
                }
                if l.raw.contains("---") {
                    continue;
                }
                let cells = split_cells(&l.raw);
                if cells.len() < 3 {
                    continue;
                }
                if cells[0].contains("(chosen)") || cells[1].contains("(chosen)") {
                    continue;
                }
                let cost = cells.last().unwrap().to_ascii_lowercase();
                let names_criterion =
                    Regex::new(r"\b(?:lost|loses)\s+(?:\w+\s+)?on\b").unwrap().is_match(&cost);
                if !names_criterion {
                    out.push(Finding::new(
                        "adr-shape", &d.rel, l.no, "SpecRecordAnatomy",
                        format!(
                            "rejected option {} names no criterion it lost on",
                            cells[0].trim()
                        ),
                    ));
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------- 19. tense

pub fn tense(c: &Corpus) -> Vec<Finding> {
    let tokens = [
        "will", "shall", "would", "planned", "not yet", "currently", "today", "shipped",
        "implemented", "previously", "used to", "legacy", "migration", "TODO", "FIXME", "WIP",
    ];
    let re = word_re(&tokens);
    let milestone = Regex::new(r"\bM[0-9]\b").unwrap();
    let mut out = Vec::new();
    for d in c.docs_for("tense") {
        for l in &d.lines {
            if l.authored.is_empty() {
                continue;
            }
            let plain = without_ticks(&l.authored);
            for m in re.find_iter(&plain) {
                out.push(Finding::new(
                    "tense", &d.rel, l.no, "SpecDatedProse",
                    format!("\"{}\" in authored text", m.as_str()),
                ));
            }
            for m in milestone.find_iter(&plain) {
                out.push(Finding::new(
                    "tense", &d.rel, l.no, "SpecDatedProse",
                    format!("milestone token \"{}\"", m.as_str()),
                ));
            }
        }
    }
    out
}

// ---------------------------------------------------------------- 20. unsettled

pub fn unsettled(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let headings = ["Open questions", "Out of scope", "See also"];
    for d in c.docs_for("unsettled") {
        for (no, _, text) in &d.headings {
            if headings.iter().any(|h| text.eq_ignore_ascii_case(h)) {
                out.push(Finding::new(
                    "unsettled", &d.rel, *no, "SpecAppendixHeading",
                    format!("the heading \"{}\" is refused; an unknown lives where it applies", text),
                ));
            }
        }
        for (no, text) in &d.unsettled {
            let plain = without_ticks(text);
            let body = plain.trim_start_matches("unsettled:");
            let question = body.split("owner:").next().unwrap_or("").trim();
            if !question.contains('?') {
                out.push(Finding::new(
                    "unsettled", &d.rel, *no, "SpecUnsettledLine",
                    "the unsettled line carries no question ending in a question mark".into(),
                ));
            }
            if !plain.contains("owner:") {
                out.push(Finding::new(
                    "unsettled", &d.rel, *no, "SpecUnsettledLine",
                    "the unsettled line names no `owner:`".into(),
                ));
            }
            if has_numeral(&plain) {
                out.push(Finding::new(
                    "unsettled", &d.rel, *no, "SpecUnsettledLine",
                    "the unsettled line carries a numeral".into(),
                ));
            }
            if let Some((_, modal)) = modal_hits(text).into_iter().next() {
                out.push(Finding::new(
                    "unsettled", &d.rel, *no, "SpecUnsettledLine",
                    format!("the unsettled line carries the modal \"{}\"", modal),
                ));
            }
            match plain.split("affects:").nth(1) {
                None => out.push(Finding::new(
                    "unsettled", &d.rel, *no, "SpecUnsettledLine",
                    "the unsettled line names no `affects:` operation".into(),
                )),
                Some(rest) => {
                    let op = rest.split_whitespace().next().unwrap_or("").trim_matches('`');
                    // `affects: land` inside the derive file names `derive.land`
                    // (`corpus.rationale.shape.unsettled-line` fixes no qualification).
                    let qualified = format!("{}.{}", d.contract.clone().unwrap_or_default(), op);
                    if !c.reg.operations.contains_key(op) && !c.reg.operations.contains_key(&qualified) {
                        out.push(Finding::new(
                            "unsettled", &d.rel, *no, "SpecUnsettledLine",
                            format!("`affects: {}` names no operation.toml key", op),
                        ));
                    }
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------- 21. banned

pub fn banned(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let nouns = word_re(&["seam", "seams", "load-bearing", "wedge", "rung", "land-grab"]);
    let axis = Regex::new(r"(?i)\baxis\b").unwrap();
    let issue = Regex::new(r"(?:^|[\s(\[])#[0-9]+\b").unwrap();
    let pr = Regex::new(r"(?i)https?://[^\s)]*/(?:pull|pulls)/[0-9]+").unwrap();
    let leaks = Regex::new(r"(?i)\bhakiri\b|\bhk1\b").unwrap();
    let proper = Regex::new(r"\bNest\b|\bTrails\b").unwrap();

    for d in c.docs_for("banned") {
        for l in &d.lines {
            if l.authored.is_empty() {
                continue;
            }
            // The law quotes every banned spelling backticked; a real use is prose.
            let plain = &without_ticks(&l.authored);
            for m in nouns.find_iter(plain) {
                out.push(Finding::new(
                    "banned", &d.rel, l.no, "SpecBannedNoun",
                    format!("the noun \"{}\"", m.as_str()),
                ));
            }
            // `axis` is legal in a chart caption alone.
            if axis.is_match(plain) && !plain.to_lowercase().contains("chart") {
                out.push(Finding::new(
                    "banned", &d.rel, l.no, "SpecBannedNoun",
                    "the noun \"axis\" outside a chart caption".into(),
                ));
            }
            if let Some(m) = issue.find(plain) {
                out.push(Finding::new(
                    "banned", &d.rel, l.no, "SpecProvenanceLeak",
                    format!("a bare issue reference \"{}\"", m.as_str().trim()),
                ));
            }
            if let Some(m) = pr.find(plain) {
                out.push(Finding::new(
                    "banned", &d.rel, l.no, "SpecProvenanceLeak",
                    format!("a pull-request link \"{}\"", m.as_str()),
                ));
            }
            for m in leaks.find_iter(plain) {
                out.push(Finding::new(
                    "banned", &d.rel, l.no, "SpecProvenanceLeak",
                    format!("the token \"{}\"", m.as_str()),
                ));
            }
            for m in proper.find_iter(plain) {
                out.push(Finding::new(
                    "banned", &d.rel, l.no, "SpecProvenanceLeak",
                    format!("the proper noun \"{}\"", m.as_str()),
                ));
            }
        }
    }
    out
}

// ---------------------------------------------------------------- 22. paths

pub fn paths(c: &Corpus) -> Vec<Finding> {
    // A bare prefix inside a code span is the law quoting its own pattern; a real
    // local path carries a component after it (`corpus.render.refusal.absolute-local-path`).
    let re = Regex::new(r"(/Users/|/home/|\$HOME/|~/)[A-Za-z0-9._-]").unwrap();
    let mut out = Vec::new();
    for d in c.docs_for("paths") {
        for l in &d.lines {
            if l.is_front_matter {
                continue;
            }
            if let Some(m) = re.find(&l.raw) {
                out.push(Finding::new(
                    "paths", &d.rel, l.no, "SpecLocalPath",
                    format!("an absolute local path beginning \"{}\"", m.as_str()),
                ));
            }
        }
    }
    out
}

// ---------------------------------------------------------------- 23. diagrams

pub fn diagrams(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let box_drawing = |s: &str| s.chars().any(|ch| ('\u{2500}'..='\u{257F}').contains(&ch));
    for d in c.docs_for("diagrams") {
        for l in &d.lines {
            if l.is_front_matter {
                continue;
            }
            let is_mermaid = l.in_fence && l.fence_info.starts_with("mermaid");
            if is_mermaid {
                continue;
            }
            if box_drawing(&l.raw) {
                out.push(Finding::new(
                    "diagrams", &d.rel, l.no, "SpecAsciiDiagram",
                    "box-drawing characters outside a mermaid fence".into(),
                ));
                continue;
            }
            // Pipe-and-dash art is refused only inside a non-mermaid fence — a
            // markdown table is not ASCII art (`corpus.render.shape.diagram`).
            if l.in_fence && !l.raw.trim_start().starts_with("```") {
                let t = l.raw.trim();
                let art = t.len() >= 3
                    && t.contains('|')
                    && t.chars().all(|ch| matches!(ch, '|' | '-' | '+' | ' ' | '='));
                if art {
                    out.push(Finding::new(
                        "diagrams", &d.rel, l.no, "SpecAsciiDiagram",
                        "pipe-and-dash art inside a fence that is not `mermaid`".into(),
                    ));
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------- 24. pins

pub fn pins(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let path = c.root.join("spec/pins.toml");
    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(_) => return out, // no pins file: every clause computes `committed`.
    };
    let v: toml::Value = match raw.parse() {
        Ok(v) => v,
        Err(e) => {
            out.push(Finding::new("pins", "spec/pins.toml", 1, "SpecBrokenPin", format!("{}", e)));
            return out;
        }
    };
    let ids = c.clause_ids();
    let kind_of: HashMap<&str, &str> = c
        .all_clauses()
        .iter()
        .map(|cl| (cl.id.as_str(), cl.kind.as_str()))
        .collect::<HashMap<_, _>>();
    let mut per_contract: BTreeMap<String, usize> = BTreeMap::new();

    if let Some(t) = v.get("pin").and_then(|x| x.as_table()) {
        for (id, entry) in t {
            let line = raw
                .lines()
                .position(|l| l.contains(id))
                .map(|i| i + 1)
                .unwrap_or(1);
            if !ids.contains(id) {
                out.push(Finding::new(
                    "pins", "spec/pins.toml", line, "SpecBrokenPin",
                    format!("pin names `{}`, which extracts from no file", id),
                ));
                continue;
            }
            let (kind, target) = pin_target(entry);
            if let Some(target) = &target {
                if !pin_resolves(&c.root, kind.as_deref(), target) {
                    out.push(Finding::new(
                        "pins", "spec/pins.toml", line, "SpecBrokenPin",
                        format!("`{}` pins `{}`, which is absent from the tree", id, target),
                    ));
                }
            }
            if kind.as_deref() == Some("type") {
                if let Some(k) = kind_of.get(id.as_str()) {
                    if *k == "refusal" || *k == "limit" {
                        out.push(Finding::new(
                            "pins", "spec/pins.toml", line, "SpecPresencePinOnBound",
                            format!("`{}` is a {} clause carrying a type-path pin", id, k),
                        ));
                    }
                }
            }
            if let Some(cn) = id.split('.').next() {
                *per_contract.entry(cn.to_string()).or_default() += 1;
            }
        }
    }

    if let Some(f) = v.get("floor").and_then(|x| x.as_table()) {
        for (contract, want) in f {
            let want = want.as_integer().unwrap_or(0) as usize;
            let live = per_contract.get(contract).copied().unwrap_or(0);
            if live < want {
                out.push(Finding::new(
                    "pins", "spec/pins.toml", 1, "SpecCoverageRegression",
                    format!("`{}` pins {} clauses against a floor of {}", contract, live, want),
                ));
            }
        }
    }
    out
}

fn pin_target(v: &toml::Value) -> (Option<String>, Option<String>) {
    if let Some(s) = v.as_str() {
        return (None, Some(s.to_string()));
    }
    let kind = v.get("kind").and_then(|x| x.as_str()).map(|s| s.to_string());
    for key in ["test", "theorem", "type", "path"] {
        if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
            return (kind.or(Some(key.to_string())), Some(s.to_string()));
        }
    }
    (kind, None)
}

fn pin_resolves(root: &std::path::Path, _kind: Option<&str>, target: &str) -> bool {
    // A pin resolves when its leading path component exists, or when its final
    // identifier is defined somewhere in the tree.
    let file = target.split("::").next().unwrap_or(target);
    if root.join(file).exists() {
        return true;
    }
    let ident = target.rsplit("::").next().unwrap_or(target);
    if ident.is_empty() {
        return false;
    }
    let out = std::process::Command::new("grep")
        .arg("-rqI")
        .arg("--")
        .arg(ident)
        .arg(root)
        .output();
    matches!(out, Ok(o) if o.status.success())
}

// ---------------------------------------------------------------- 25. roadmap

pub fn roadmap(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let doc = match c.docs.iter().find(|d| d.role == "plan") {
        Some(d) => d,
        None => return out,
    };
    let ids = c.clause_ids();
    let id_re = Regex::new(r"`([a-z0-9\-]+(?:\.[a-z0-9\-]+){3})`").unwrap();
    let mut milestone = String::new();
    let mut placed: HashMap<String, (String, usize)> = HashMap::new();

    for l in &doc.lines {
        if let Some(h) = l.raw.strip_prefix("## ") {
            milestone = h.trim().to_string();
        }
        for m in id_re.captures_iter(&l.raw) {
            let id = m[1].to_string();
            if !ids.contains(&id) {
                out.push(Finding::new(
                    "roadmap", &doc.rel, l.no, "SpecRoadmapUnknownClause",
                    format!("milestone \"{}\" names `{}`, which extracts from no file", milestone, id),
                ));
                continue;
            }
            if let Some((other, other_line)) = placed.get(&id) {
                if other != &milestone {
                    out.push(Finding::new(
                        "roadmap", &doc.rel, l.no, "SpecRoadmapDoubleSchedule",
                        format!(
                            "`{}` sits under \"{}\" and under \"{}\" ({}:{})",
                            id, milestone, other, doc.rel, other_line
                        ),
                    ));
                }
            } else {
                placed.insert(id, (milestone.clone(), l.no));
            }
        }
    }
    out
}

// ---------------------------------------------------------------- driver

pub fn run(c: &Corpus, name: &str) -> Vec<Finding> {
    match name {
        "ids" => ids(c),
        "owns" => owns(c),
        "anatomy" => anatomy(c),
        "titles" => titles(c),
        "size" => size(c),
        "terms" => terms(c),
        "aliases" => aliases(c),
        "foreign" => foreign(c),
        "literals" => literals(c),
        "limits" => limits(c),
        "errors" => errors(c),
        "shingles" => shingles(c),
        "prose" => prose(c),
        "links" => links(c),
        "rationale" => rationale(c),
        "citations" => citations(c),
        "adr-orphans" => adr_orphans(c),
        "adr-shape" => adr_shape(c),
        "tense" => tense(c),
        "unsettled" => unsettled(c),
        "banned" => banned(c),
        "paths" => paths(c),
        "diagrams" => diagrams(c),
        "pins" => pins(c),
        "roadmap" => roadmap(c),
        _ => Vec::new(),
    }
}
