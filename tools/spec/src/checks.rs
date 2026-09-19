//! Every rule `spec/00-corpus.md` states, one function per operation of the
//! `corpus` contract.

use crate::corpus::*;
use crate::util::*;
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::LazyLock;

pub const CHECKS: [&str; 7] = ["address", "anatomy", "registry", "reference", "rationale", "state", "render"];

pub fn run(c: &Corpus, name: &str) -> Vec<Finding> {
    match name {
        "address" => address(c),
        "anatomy" => anatomy(c),
        "registry" => registry(c),
        "reference" => reference(c),
        "rationale" => rationale(c),
        "state" => state(c),
        "render" => render(c),
        other => vec![Finding::new("lint", "", 0, "SpecUnknownCheck", format!("no check named `{other}`"))],
    }
}

static SEGMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9-]+$").unwrap());
static RECORD_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(P[0-9]+|D[0-9]{2})$").unwrap());
static RECORD_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^spec/decisions/(P[0-9]+|D[0-9]{2})-[a-z0-9-]+\.md$").unwrap());
static RAISES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"raises? `([A-Za-z0-9_]+)`").unwrap());
static NUM_UNIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[^A-Za-z0-9_.])([0-9][0-9_,]*(?:\.[0-9]+)?)\s?([A-Za-z%]+)\b").unwrap());
static IDENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_-]*$").unwrap());
static UNSETTLED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^unsettled: .+\? owner: \S+ affects: ([a-z0-9-]+)\.([a-z0-9-]+)$").unwrap()
});
static ISO_DATE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[0-9]{4}-[0-9]{2}-[0-9]{2}\b").unwrap());
static BARE_ISSUE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(^|\s)#[0-9]+\b|/pull/[0-9]+").unwrap());
static EXT_LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\]\((https?://|[^)]*references/)").unwrap());

fn f(check: &str, file: &str, line: usize, code: &str, msg: String) -> Finding {
    Finding::new(check, file, line, code, msg)
}

fn word_re(words: &[&str]) -> Regex {
    let alts: Vec<String> = words.iter().map(|w| regex::escape(w)).collect();
    Regex::new(&format!(r"(?i)\b({})\b", alts.join("|"))).unwrap()
}

// ---------------------------------------------------------------- address

fn address(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut seen: HashMap<String, (String, usize)> = HashMap::new();
    for d in c.contracts() {
        let contract = d.contract.clone().unwrap_or_default();
        for cl in &d.clauses {
            let segs: Vec<&str> = cl.id.split('.').collect();
            let bad = if segs.len() != 3 || !segs.iter().all(|s| SEGMENT.is_match(s)) {
                Some("is not three `[a-z0-9-]+` segments".to_string())
            } else if segs[0] != contract {
                Some(format!("names contract `{}` in a `{}` file", segs[0], contract))
            } else if !d.owns.iter().any(|o| o == segs[1]) {
                Some(format!("names operation `{}` this file does not own", segs[1]))
            } else if segs[2].len() > 40 {
                Some("subject exceeds 40 chars".into())
            } else {
                None
            };
            if let Some(why) = bad {
                out.push(f("address", &d.rel, cl.line, "SpecMalformedId", format!("`{}` {}", cl.id, why)));
            }
            if let Some((file, line)) = seen.get(&cl.id) {
                out.push(f(
                    "address",
                    &d.rel,
                    cl.line,
                    "SpecDuplicateId",
                    format!("`{}` also at {}:{}", cl.id, file, line),
                ));
            } else {
                seen.insert(cl.id.clone(), (d.rel.clone(), cl.line));
            }
        }
    }
    out
}

// ---------------------------------------------------------------- anatomy

fn anatomy(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut listed: BTreeMap<String, (String, String)> = BTreeMap::new();
    for (name, e) in &c.reg.contracts {
        if e.files.len() != e.titles.len() {
            out.push(f("anatomy", "spec/terms/contract.toml", 0, "SpecAnatomy", format!("`{name}` lists {} files and {} titles", e.files.len(), e.titles.len())));
        }
        for (i, file) in e.files.iter().enumerate() {
            listed.insert(file.clone(), (name.clone(), e.titles.get(i).cloned().unwrap_or_default()));
            if !c.root.join(file).exists() {
                out.push(f("anatomy", "spec/terms/contract.toml", 0, "SpecAnatomy", format!("`{name}` lists missing file {file}")));
            }
        }
    }
    for d in c.contracts() {
        let a = |line: usize, msg: String| f("anatomy", &d.rel, line, "SpecAnatomy", msg);
        let Some(contract) = d.contract.clone() else {
            out.push(a(1, "no front-matter `contract`".into()));
            continue;
        };
        match listed.get(&d.rel) {
            None => out.push(a(1, "file is listed by no contract in spec/terms/contract.toml".into())),
            Some((owner, _)) if *owner != contract => {
                out.push(a(1, format!("front matter says `{contract}`, contract.toml lists it under `{owner}`")))
            }
            _ => {}
        }
        if d.lines.len() > 900 {
            out.push(a(d.lines.len(), format!("{} lines exceed 900", d.lines.len())));
        }
        let mut titles = Vec::new();
        let mut sections = Vec::new();
        for (n, l, k) in d.each() {
            if k != LineKind::Heading {
                continue;
            }
            if let Some(t) = l.strip_prefix("# ") {
                titles.push((n, t.trim().to_string()));
            } else if let Some(t) = l.strip_prefix("## ") {
                sections.push((n, t.trim().to_string()));
            }
        }
        match titles.as_slice() {
            [(n, t)] => {
                if let Some((_, want)) = listed.get(&d.rel) {
                    if t != want {
                        out.push(a(*n, format!("title `{t}` differs from registry `{want}`")));
                    }
                }
            }
            _ => out.push(a(1, format!("{} `# ` titles, want 1", titles.len()))),
        }
        let mut want: Vec<String> = d.owns.clone();
        let names: Vec<String> = sections.iter().map(|s| s.1.clone()).collect();
        if names.last().map(|s| s == "Shapes").unwrap_or(false) {
            want.push("Shapes".into());
        }
        if names != want {
            out.push(a(
                sections.first().map(|s| s.0).unwrap_or(1),
                format!("sections [{}] differ from owns order [{}]", names.join(", "), want.join(", ")),
            ));
        }
        // every owned section carries one clause table
        let mut tables: BTreeMap<String, usize> = BTreeMap::new();
        let mut cur = String::new();
        for (_, l, k) in d.each() {
            if k == LineKind::Heading {
                if let Some(t) = l.strip_prefix("## ") {
                    cur = t.trim().to_string();
                }
            }
            if k == LineKind::ClauseHeader && !is_separator(l) {
                *tables.entry(cur.clone()).or_default() += 1;
            }
        }
        for op in &d.owns {
            let n = tables.get(op).copied().unwrap_or(0);
            if n != 1 {
                let line = sections.iter().find(|s| &s.1 == op).map(|s| s.0).unwrap_or(1);
                out.push(a(line, format!("section `{op}` holds {n} clause tables, want 1")));
            }
        }
        if tables.contains_key("Shapes") {
            out.push(a(1, "`Shapes` holds a clause table".into()));
        }
        for cl in &d.clauses {
            let w = word_count(&cl.statement);
            if w > 40 {
                out.push(a(cl.line, format!("`{}` statement is {w} words, over 40", cl.id)));
            }
        }
    }
    out
}

// ---------------------------------------------------------------- registry

fn normalize(s: &str) -> String {
    let mut t: String = s.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
    for p in ["max", "min", "the"] {
        if s.to_lowercase().starts_with(&format!("{p}-")) || s.to_lowercase().starts_with(&format!("{p}_")) {
            t = t[p.len()..].to_string();
        }
    }
    if t.len() > 5 && t.ends_with('s') {
        t.pop();
    }
    t
}

fn registry(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    for (file, e) in &c.reg.load_errors {
        out.push(f("registry", file, 0, "SpecRegistry", e.clone()));
    }
    let clauses = c.clause_map();
    // operations: registered <-> owned by exactly one file of the contract
    let mut owned: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for d in c.contracts() {
        let contract = d.contract.clone().unwrap_or_default();
        for o in &d.owns {
            owned.entry((contract.clone(), o.clone())).or_default().push(d.rel.clone());
            let reg = c.reg.fragments.get(&contract).map(|fr| fr.operation.contains_key(o)).unwrap_or(false);
            if !reg {
                out.push(f("registry", &d.rel, 1, "SpecRegistry", format!("owned operation `{contract}.{o}` is not registered")));
            }
        }
    }
    for (contract, fr) in &c.reg.fragments {
        let frel = format!("spec/terms/{contract}.toml");
        for (op, n) in &fr.operation {
            let files = owned.get(&(contract.clone(), op.clone())).cloned().unwrap_or_default();
            if files.len() != 1 {
                out.push(f("registry", &frel, 0, "SpecRegistry", format!("operation `{contract}.{op}` is owned by {} files", files.len())));
            }
            if n.gloss.trim().is_empty() {
                out.push(f("registry", &frel, 0, "SpecRegistry", format!("operation `{op}` has no gloss")));
            }
        }
        for (id, e) in &fr.error {
            match clauses.get(&e.clause) {
                None => out.push(f("registry", &frel, 0, "SpecRegistry", format!("error `{id}` names missing clause `{}`", e.clause))),
                Some(cl) => {
                    if cl.contract != *contract {
                        out.push(f("registry", &frel, 0, "SpecRegistry", format!("error `{id}` names clause `{}` of another contract", e.clause)));
                    }
                    if !cl.statement.contains(&format!("`{id}`")) {
                        out.push(f("registry", &cl.file, cl.line, "SpecRegistry", format!("`{}` does not name its error `{id}`", cl.id)));
                    }
                }
            }
        }
        for (id, l) in &fr.limit {
            match clauses.get(&l.clause) {
                None => out.push(f("registry", &frel, 0, "SpecRegistry", format!("bound `{id}` names missing clause `{}`", l.clause))),
                Some(cl) => {
                    let v = l.value_text();
                    let st = without_ticks_keep(&cl.statement);
                    let re = Regex::new(&format!(r"(^|[^0-9.]){}\s?{}\b", regex::escape(&v), regex::escape(&l.unit))).unwrap();
                    if !re.is_match(&st) {
                        out.push(f("registry", &cl.file, cl.line, "SpecRegistry", format!("`{}` does not carry its bound `{id}` as `{v} {}`", cl.id, l.unit)));
                    }
                }
            }
            if !c.reg.units.contains_key(&l.unit) {
                out.push(f("registry", &frel, 0, "SpecRegistry", format!("bound `{id}` unit `{}` is not a canonical unit", l.unit)));
            }
            let basis_ok = l.basis == "chosen" || l.basis.starts_with("measured:") || l.basis.starts_with("standard:");
            if !basis_ok {
                out.push(f("registry", &frel, 0, "SpecRegistry", format!("bound `{id}` basis `{}` is not chosen, measured:<b> or standard:<n>", l.basis)));
            }
        }
    }
    // each statement: errors it names are its own; numerals with units are its bounds
    let mut limit_by_clause: BTreeMap<&str, Vec<&LimitEntry>> = BTreeMap::new();
    for fr in c.reg.fragments.values() {
        for l in fr.limit.values() {
            limit_by_clause.entry(l.clause.as_str()).or_default().push(l);
        }
    }
    for cl in c.clauses() {
        for cap in RAISES.captures_iter(&cl.statement) {
            let id = &cap[1];
            if c.reg.error_owner(id).is_none() {
                out.push(f("registry", &cl.file, cl.line, "SpecRegistry", format!("`{}` raises unregistered error `{id}`", cl.id)));
            }
        }
        for (_, tok) in tick_spans(&cl.statement) {
            if let Some(e) = c.reg.error_owner(&tok) {
                if e.clause != cl.id {
                    out.push(f("registry", &cl.file, cl.line, "SpecRegistry", format!("`{}` names error `{tok}` owned by `{}`; point with {{{{{}}}}}", cl.id, e.clause, e.clause)));
                }
            }
        }
        let text = strip_pointers(&without_ticks(&cl.statement), " ");
        let mine = limit_by_clause.get(cl.id.as_str()).cloned().unwrap_or_default();
        for cap in NUM_UNIT.captures_iter(&text) {
            let num = cap[1].replace([',', '_'], "");
            let Some(unit) = c.reg.unit_of(&cap[2]) else { continue };
            let ok = mine.iter().any(|l| l.value_text().replace([',', '_'], "") == num && l.unit == unit);
            if !ok {
                out.push(f("registry", &cl.file, cl.line, "SpecRegistry", format!("`{}` states `{} {}` with no named bound", cl.id, &cap[1], &cap[2])));
            }
        }
    }
    // shared identifiers
    let mut by_token: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut first: BTreeMap<String, (String, usize)> = BTreeMap::new();
    for cl in c.clauses() {
        for (_, tok) in tick_spans(&cl.statement) {
            if !IDENT.is_match(&tok) || tok.len() < 3 {
                continue;
            }
            by_token.entry(tok.clone()).or_default().insert(cl.contract.clone());
            first.entry(tok).or_insert((cl.file.clone(), cl.line));
        }
    }
    for (tok, cs) in &by_token {
        if cs.len() >= 2 && !c.reg.is_registered(tok) {
            let (file, line) = &first[tok];
            out.push(f("registry", file, *line, "SpecRegistry", format!("`{tok}` appears in {} contracts ({}) and is not registered", cs.len(), cs.iter().cloned().collect::<Vec<_>>().join(", "))));
        }
    }
    // spelling collisions and aliases
    let mut spell: BTreeMap<String, String> = BTreeMap::new();
    let mut aliases: BTreeMap<String, String> = BTreeMap::new();
    let mut add = |kind: &str, owner: &str, name: &str, out: &mut Vec<Finding>| {
        let key = normalize(name);
        let label = format!("{kind} `{name}` ({owner})");
        if let Some(prev) = spell.get(&key) {
            out.push(f("registry", &format!("spec/terms/{owner}.toml"), 0, "SpecSpellingCollision", format!("{label} collides with {prev}")));
        } else {
            spell.insert(key, label);
        }
    };
    for (owner, fr) in &c.reg.fragments {
        for k in fr.error.keys() {
            add("error", owner, k, &mut out);
        }
        for k in fr.limit.keys() {
            add("bound", owner, k, &mut out);
        }
        for (k, n) in &fr.term {
            add("term", owner, k, &mut out);
            for a in &n.aliases {
                aliases.insert(a.clone(), k.clone());
            }
        }
    }
    for k in c.reg.wire.keys() {
        add("wire", "wire", k, &mut out);
    }
    for cl in c.clauses() {
        for (_, tok) in tick_spans(&cl.statement) {
            if let Some(canon) = aliases.get(&tok) {
                out.push(f("registry", &cl.file, cl.line, "SpecSpellingCollision", format!("`{}` uses alias `{tok}`; write `{canon}`", cl.id)));
            }
        }
    }
    out
}

/// Blank backticks but keep their content (a bound inside ticks still counts).
fn without_ticks_keep(s: &str) -> String {
    s.replace('`', " ")
}

// ---------------------------------------------------------------- reference

fn shingles(s: &str) -> BTreeSet<String> {
    let clean: String = strip_pointers(s, " ")
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { ' ' })
        .collect();
    let w: Vec<&str> = clean.split_whitespace().collect();
    let mut out = BTreeSet::new();
    if w.len() >= 8 {
        for i in 0..=w.len() - 8 {
            out.insert(w[i..i + 8].join(" "));
        }
    }
    out
}

fn reference(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let ids: BTreeSet<String> = c.clauses().map(|cl| cl.id.clone()).collect();
    for d in &c.docs {
        for (n, l, k) in d.each() {
            if k == LineKind::Code || k == LineKind::Fence {
                continue;
            }
            for p in pointers(&without_ticks(l)) {
                if !ids.contains(&p) {
                    out.push(f("reference", &d.rel, n, "SpecDanglingReference", format!("{{{{{p}}}}} names no clause")));
                }
            }
            if EXT_LINK.is_match(l) {
                out.push(f("reference", &d.rel, n, "SpecExternalLink", "link to an external document or references/".into()));
            }
        }
    }
    let mut owner: HashMap<String, String> = HashMap::new();
    let mut reported: BTreeSet<(String, String)> = BTreeSet::new();
    for cl in c.clauses() {
        for s in shingles(&cl.statement) {
            match owner.get(&s) {
                Some(other) if *other != cl.id => {
                    let key = (other.clone(), cl.id.clone());
                    if reported.insert(key) {
                        out.push(f("reference", &cl.file, cl.line, "SpecRestatement", format!("`{}` restates `{other}`: \"{s}\"", cl.id)));
                    }
                }
                Some(_) => {}
                None => {
                    owner.insert(s, cl.id.clone());
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------- rationale

static LEAK: LazyLock<Regex> = LazyLock::new(|| {
    word_re(&["because", "so that", "in order to", "the reason", "which is why", "judged on", "at the cost of", "trade-off"])
});
static MODAL: LazyLock<Regex> =
    LazyLock::new(|| word_re(&["must", "never", "refuses", "is refused", "at most", "at least", "always"]));
static APPENDIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^#+\s*(open questions|out of scope|see also)\s*$").unwrap());

fn why_ok(why: &str) -> Result<Vec<String>, String> {
    let w = why.trim();
    if w.is_empty() || w == "—" {
        return Ok(vec![]);
    }
    if let Some(rest) = w.strip_prefix("because ") {
        let n = rest.split_whitespace().count();
        return if n <= 30 { Ok(vec![]) } else { Err(format!("`because` cell is {n} words, over 30")) };
    }
    let ids: Vec<String> = w.split(',').map(|s| s.trim().to_string()).collect();
    if ids.iter().all(|i| RECORD_ID.is_match(i)) {
        Ok(ids)
    } else {
        Err(format!("Why cell `{w}` is neither record ids nor a `because` clause"))
    }
}

fn record_id(rel: &str) -> Option<String> {
    RECORD_FILE.captures(rel).map(|c| c[1].to_string())
}

fn rationale(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut cited: BTreeMap<String, (String, usize)> = BTreeMap::new();
    for cl in c.clauses() {
        match why_ok(&cl.why) {
            Ok(ids) => {
                for i in ids {
                    cited.entry(i).or_insert((cl.file.clone(), cl.line));
                }
            }
            Err(e) => out.push(f("rationale", &cl.file, cl.line, "SpecRecord", format!("`{}`: {e}", cl.id))),
        }
        if cl.kind == "refusal" && matches!(cl.why.trim(), "" | "—") {
            out.push(f("rationale", &cl.file, cl.line, "SpecRecord", format!("refusal `{}` carries no Why", cl.id)));
        }
        if LEAK.is_match(&without_ticks(&cl.statement)) {
            out.push(f("rationale", &cl.file, cl.line, "SpecRationaleLeak", format!("`{}` statement carries rationale", cl.id)));
        }
    }
    let mut records: BTreeSet<String> = BTreeSet::new();
    for d in c.records() {
        match record_id(&d.rel) {
            None => out.push(f("rationale", &d.rel, 1, "SpecRecord", "record file name is not P<n>-<slug>.md or D<nn>-<slug>.md".into())),
            Some(id) => {
                if !cited.contains_key(&id) {
                    out.push(f("rationale", &d.rel, 1, "SpecRecord", format!("record {id} is cited by no clause")));
                }
                record_anatomy(d, &id, &mut out);
                records.insert(id);
            }
        }
    }
    for (id, (file, line)) in &cited {
        if !records.contains(id) {
            out.push(f("rationale", file, *line, "SpecRecord", format!("Why cites missing record {id}")));
        }
    }
    let ops: BTreeSet<(String, String)> = c
        .reg
        .fragments
        .iter()
        .flat_map(|(k, fr)| fr.operation.keys().map(move |o| (k.clone(), o.clone())))
        .collect();
    for d in c.contracts() {
        for (n, l, k) in d.each() {
            let plain = without_ticks(l);
            match k {
                LineKind::Prose | LineKind::TableRow | LineKind::Heading => {
                    if LEAK.is_match(&plain) {
                        out.push(f("rationale", &d.rel, n, "SpecRationaleLeak", "rationale outside a Why cell".into()));
                    }
                    if MODAL.is_match(&plain) {
                        out.push(f("rationale", &d.rel, n, "SpecStrayModal", format!("modal `{}` outside a clause row", MODAL.find(&plain).unwrap().as_str())));
                    }
                    if k == LineKind::Heading && APPENDIX.is_match(l) {
                        out.push(f("rationale", &d.rel, n, "SpecUnsettled", "appendix heading".into()));
                    }
                }
                LineKind::Unsettled => match UNSETTLED.captures(l.trim()) {
                    None => out.push(f("rationale", &d.rel, n, "SpecUnsettled", "unsettled line is not `unsettled: <q>? owner: <h> affects: <c>.<op>`".into())),
                    Some(cap) => {
                        if !ops.contains(&(cap[1].to_string(), cap[2].to_string())) {
                            out.push(f("rationale", &d.rel, n, "SpecUnsettled", format!("affects `{}.{}` names no operation", &cap[1], &cap[2])));
                        }
                    }
                },
                _ => {}
            }
        }
    }
    out
}

fn record_anatomy(d: &Doc, id: &str, out: &mut Vec<Finding>) {
    let r = |line: usize, msg: String| f("rationale", &d.rel, line, "SpecRecord", msg);
    let first = d.lines.first().cloned().unwrap_or_default();
    if !first.starts_with(&format!("# {id} — ")) {
        out.push(r(1, format!("first line is not `# {id} — <title>`")));
    }
    if !d.lines.iter().any(|l| l.starts_with("**Status:**")) {
        out.push(r(1, "no `**Status:**` line".into()));
    }
    let sections: Vec<(usize, String)> = d
        .each()
        .filter(|(_, _, k)| *k == LineKind::Heading)
        .filter_map(|(n, l, _)| l.strip_prefix("## ").map(|t| (n, t.trim().to_string())))
        .collect();
    let order = ["Context", "Decision", "Options", "Consequences", "Revisit"];
    let names: Vec<&str> = sections.iter().map(|s| s.1.as_str()).collect();
    let mut last = 0usize;
    let mut ok = true;
    for n in &names {
        match order.iter().position(|o| o == n) {
            Some(p) if p >= last => last = p + 1,
            _ => ok = false,
        }
    }
    for req in ["Decision", "Options", "Consequences"] {
        if !names.contains(&req) {
            ok = false;
        }
    }
    if !ok {
        out.push(r(1, format!("sections [{}] are not Context?, Decision, Options, Consequences, Revisit?", names.join(", "))));
    }
    let words: usize = d
        .lines
        .iter()
        .filter(|l| !is_separator(l))
        .map(|l| l.replace('|', " ").split_whitespace().count())
        .sum();
    if words > 400 {
        out.push(r(1, format!("{words} words, over 400")));
    }
    // options table
    let Some(start) = sections.iter().find(|s| s.1 == "Options").map(|s| s.0) else { return };
    let mut rows = Vec::new();
    let mut header = None;
    for (n, l, _) in d.each().skip(start) {
        if l.starts_with("## ") {
            break;
        }
        if l.trim_start().starts_with('|') {
            if header.is_none() {
                header = Some((n, cells(l)));
            } else if !is_separator(l) {
                rows.push((n, cells(l)));
            }
        }
    }
    match header {
        Some((n, h)) if h != ["Option", "Lost on", "Cost"] => out.push(r(n, "Options table is not headed Option | Lost on | Cost".into())),
        None => out.push(r(start, "Options holds no table".into())),
        _ => {}
    }
    if !(2..=5).contains(&rows.len()) {
        out.push(r(start, format!("Options holds {} rows, want 2 to 5", rows.len())));
    }
    let chosen: Vec<_> = rows.iter().filter(|(_, c)| c.first().map(|s| s.contains("*(chosen)*")).unwrap_or(false)).collect();
    if chosen.len() != 1 {
        out.push(r(start, format!("{} rows marked *(chosen)*, want 1", chosen.len())));
    }
    for (n, row) in &rows {
        let is_chosen = row.first().map(|s| s.contains("*(chosen)*")).unwrap_or(false);
        let lost = row.get(1).map(|s| s.trim()).unwrap_or("");
        if is_chosen && lost != "—" {
            out.push(r(*n, "the chosen row's Lost on is not `—`".into()));
        }
        if !is_chosen && (lost.is_empty() || lost == "—") {
            out.push(r(*n, "a rejected option names no criterion it lost on".into()));
        }
    }
}

// ---------------------------------------------------------------- state

#[derive(serde::Deserialize, Default)]
pub struct PinsFile {
    #[serde(default)]
    pub pin: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    pub floor: BTreeMap<String, i64>,
}

pub fn load_pins(c: &Corpus) -> PinsFile {
    std::fs::read_to_string(c.root.join("spec/pins.toml"))
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

/// `performed` when the artifact's final segment is defined under `crates/`.
pub fn verdict(c: &Corpus, pin: &BTreeMap<String, String>) -> &'static str {
    let Some((kind, path)) = pin.iter().next() else { return "broken" };
    let last = path.rsplit([':', '.']).next().unwrap_or(path);
    let pat = match kind.as_str() {
        "test" => format!(r"\bfn\s+{}\b", regex::escape(last)),
        "theorem" => format!(r"\b(theorem|lemma)\s+{}\b", regex::escape(last)),
        _ => format!(r"\b(fn|struct|enum|trait|type|const|static|mod)\s+{}\b", regex::escape(last)),
    };
    let re = Regex::new(&pat).unwrap();
    let crates = c.root.join("crates");
    if !crates.exists() {
        return "broken";
    }
    for e in walkdir::WalkDir::new(crates).into_iter().flatten() {
        let p = e.path();
        if p.extension().map(|x| x == "rs" || x == "lean").unwrap_or(false) {
            if let Ok(s) = std::fs::read_to_string(p) {
                if re.is_match(&s) {
                    return "performed";
                }
            }
        }
    }
    "broken"
}

/// A roadmap milestone: its heading, its line, and each named operation with its line.
pub type Milestone = (String, usize, Vec<(usize, String)>);

pub fn roadmap(c: &Corpus) -> Vec<Milestone> {
    let mut out: Vec<Milestone> = Vec::new();
    let Some(d) = c.docs.iter().find(|d| d.role == Role::Plan) else { return out };
    let re = Regex::new(r"^[a-z0-9-]+\.([a-z0-9-]+|\*)$").unwrap();
    for (n, l, k) in d.each() {
        if k == LineKind::Heading && l.starts_with("## ") {
            out.push((l[3..].trim().to_string(), n, vec![]));
        } else if k == LineKind::TableRow && !is_separator(l) {
            if let Some(m) = out.last_mut() {
                let first = cells(l).into_iter().next().unwrap_or_default();
                for (_, t) in tick_spans(&first) {
                    if re.is_match(&t) {
                        m.2.push((n, t));
                    }
                }
            }
        }
    }
    out
}

pub fn expand_roadmap(c: &Corpus) -> (BTreeMap<String, String>, Vec<Finding>) {
    let mut claim: BTreeMap<String, String> = BTreeMap::new();
    let mut out = Vec::new();
    let ms = roadmap(c);
    let ops = |contract: &str| -> Vec<String> {
        c.reg.fragments.get(contract).map(|f| f.operation.keys().cloned().collect()).unwrap_or_default()
    };
    let mut explicit = BTreeSet::new();
    for (_, _, names) in &ms {
        for (_, t) in names {
            if !t.ends_with(".*") {
                explicit.insert(t.clone());
            }
        }
    }
    let mut stars = BTreeSet::new();
    for (m, _, names) in &ms {
        for (n, t) in names {
            let (contract, op) = t.split_once('.').unwrap();
            if op == "*" {
                if ops(contract).is_empty() {
                    out.push(f("state", "spec/roadmap.md", *n, "SpecRoadmap", format!("`{t}` names no contract")));
                }
                if !stars.insert(contract.to_string()) {
                    out.push(f("state", "spec/roadmap.md", *n, "SpecRoadmap", format!("`{t}` scheduled twice")));
                }
                for o in ops(contract) {
                    let full = format!("{contract}.{o}");
                    if !explicit.contains(&full) {
                        claim.insert(full, m.clone());
                    }
                }
            } else if !ops(contract).contains(&op.to_string()) {
                out.push(f("state", "spec/roadmap.md", *n, "SpecRoadmap", format!("`{t}` names no operation")));
            } else if let Some(prev) = claim.get(t) {
                if prev != m {
                    out.push(f("state", "spec/roadmap.md", *n, "SpecRoadmap", format!("`{t}` claimed by two milestones")));
                }
            } else {
                claim.insert(t.clone(), m.clone());
            }
        }
    }
    (claim, out)
}

fn state(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let pins = load_pins(c);
    let clauses = c.clause_map();
    let mut count: BTreeMap<String, i64> = BTreeMap::new();
    for (id, pin) in &pins.pin {
        let Some(cl) = clauses.get(id) else {
            out.push(f("state", "spec/pins.toml", 0, "SpecBrokenPin", format!("pin `{id}` names no clause")));
            continue;
        };
        if pin.contains_key("item") && cl.kind != "behavior" {
            out.push(f("state", "spec/pins.toml", 0, "SpecBrokenPin", format!("{} `{id}` takes a test or theorem, not an item", cl.kind)));
        }
        if verdict(c, pin) == "broken" {
            out.push(f("state", "spec/pins.toml", 0, "SpecBrokenPin", format!("pin for `{id}` does not resolve under crates/")));
        } else {
            *count.entry(cl.contract.clone()).or_default() += 1;
        }
    }
    for (contract, floor) in &pins.floor {
        let live = count.get(contract).copied().unwrap_or(0);
        if live < *floor {
            out.push(f("state", "spec/pins.toml", 0, "SpecCoverageRegression", format!("`{contract}` holds {live} performed pins under floor {floor}")));
        }
        if !c.reg.contracts.contains_key(contract) {
            out.push(f("state", "spec/pins.toml", 0, "SpecBrokenPin", format!("floor names unknown contract `{contract}`")));
        }
    }
    out.extend(expand_roadmap(c).1);
    out
}

// ---------------------------------------------------------------- render

static DATED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(planned|not yet|currently|today|shipped|implemented|previously|used to|legacy)\b|\b(TODO|FIXME|WIP)\b").unwrap()
});
static COUNTERFACTUAL: LazyLock<Regex> = LazyLock::new(|| word_re(&["will", "would", "shall"]));
static BANNED: LazyLock<Regex> = LazyLock::new(|| word_re(&["seam", "seams", "load-bearing", "wedge", "rung", "rungs", "land-grab"]));

fn render(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    for (rel, want) in [("spec/status.md", crate::status_text(c)), ("spec/spec.lock.json", crate::lock_text(c))] {
        let have = std::fs::read_to_string(c.root.join(rel)).unwrap_or_default();
        if have != want {
            out.push(f("render", rel, 0, "SpecStaleRender", "differs from regeneration; run `contextful-spec state` and `extract`".into()));
        }
    }
    let local = Regex::new(r"(/Users/|/home/|\$HOME/|(^|[\s(`])~/)").unwrap();
    let boxes = |s: &str| s.chars().any(|ch| ('\u{2500}'..='\u{257F}').contains(&ch));
    for d in &c.docs {
        let mut fence_lang_mermaid = false;
        for (n, l, k) in d.each() {
            if k == LineKind::Fence {
                fence_lang_mermaid = l.trim().starts_with("```mermaid");
            }
            if local.is_match(&without_ticks(l)) {
                out.push(f("render", &d.rel, n, "SpecLocalPath", "absolute local path".into()));
            }
            if matches!(k, LineKind::Code | LineKind::Fence | LineKind::Front) {
                let _ = fence_lang_mermaid;
                continue;
            }
            if boxes(l) {
                out.push(f("render", &d.rel, n, "SpecAsciiDiagram", "box-drawing outside a mermaid fence".into()));
            }
            let plain = without_ticks(l);
            if d.role != Role::Plan && (DATED.is_match(&plain) || ISO_DATE.is_match(&plain)) {
                out.push(f("render", &d.rel, n, "SpecDatedProse", "build-state or dated vocabulary".into()));
            }
            if d.role == Role::Contract && COUNTERFACTUAL.is_match(&plain) {
                out.push(f("render", &d.rel, n, "SpecCounterfactual", format!("`{}` in a contract file", COUNTERFACTUAL.find(&plain).unwrap().as_str())));
            }
            if BANNED.is_match(&plain) || BARE_ISSUE.is_match(&plain) {
                out.push(f("render", &d.rel, n, "SpecBannedWord", "banned noun or provenance link".into()));
            }
            for (i, w) in plain.split(|ch: char| !ch.is_alphanumeric() && ch != '-').enumerate() {
                if w.is_empty() {
                    continue;
                }
                if let Some(r) = c.reg.refused.get(&sha256_lower(w)) {
                    let capital = w.chars().next().map(|ch| ch.is_uppercase()).unwrap_or(false);
                    if !r.proper || (capital && i > 0) {
                        out.push(f("render", &d.rel, n, "SpecBannedWord", format!("refused name ({})", r.note)));
                    }
                }
            }
        }
    }
    let terms = c.root.join("spec/terms");
    if let Ok(rd) = std::fs::read_dir(&terms) {
        for e in rd.flatten() {
            let rel = format!("spec/terms/{}", e.file_name().to_string_lossy());
            if let Ok(s) = std::fs::read_to_string(e.path()) {
                for (i, l) in s.lines().enumerate() {
                    if local.is_match(l) {
                        out.push(f("render", &rel, i + 1, "SpecLocalPath", "absolute local path".into()));
                    }
                }
            }
        }
    }
    out
}
