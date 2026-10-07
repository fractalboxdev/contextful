//! Every rule `spec/00-corpus.md` states, one function per operation of the
//! `corpus` contract.

use crate::corpus::*;
use crate::util::*;
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::LazyLock;

pub const CHECKS: [&str; 10] = ["address", "anatomy", "registry", "reference", "rationale", "state", "render", "diagram", "targets", "guide"];

pub fn run(c: &Corpus, name: &str) -> Vec<Finding> {
    match name {
        "address" => address(c),
        "anatomy" => anatomy(c),
        "registry" => registry(c),
        "reference" => reference(c),
        "rationale" => rationale(c),
        "state" => state(c),
        "render" => render(c),
        "diagram" => crate::diagram::check(c),
        "targets" => crate::targets::check(c),
        "guide" => guide(c),
        other => vec![Finding::new("lint", "", 0, "SpecUnknownCheck", format!("no check named `{other}`"))],
    }
}

static SEGMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9-]+$").unwrap());
static RECORD_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(P[0-9]+|A-[a-z0-9-]+)$").unwrap());
static RECORD_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^spec/adr/(?:(P[0-9]+)-[a-z0-9-]+|(A-[a-z0-9-]+))\.md$").unwrap());
static RAISES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"raises? `([A-Za-z0-9_]+)`").unwrap());
static NUM_UNIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[^A-Za-z0-9_.])([0-9][0-9_,]*(?:\.[0-9]+)?)\s?([A-Za-z%]+)\b").unwrap());
static UNSETTLED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^unsettled: .+\? owner: \S+ affects: ([a-z0-9-]+)\.([a-z0-9-]+)$").unwrap()
});
static SCENARIO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^WHEN .+, THEN .+$").unwrap());
static ISO_DATE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[0-9]{4}-[0-9]{2}-[0-9]{2}\b").unwrap());
static BARE_ISSUE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(^|\s)#[0-9]+\b|/pull/[0-9]+").unwrap());
static EXT_LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\]\((https?://|[^)]*references/)|<https?://[^>]+>").unwrap());

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
        // every owned section opens with a lede, then at most one contiguous clause list
        let mut breaks: BTreeMap<String, usize> = BTreeMap::new();
        let mut cur = String::new();
        let mut last: Option<usize> = None; // line of the latest item or Why line in `cur`
        for (n, l, k) in d.each() {
            match k {
                LineKind::Heading if l.starts_with("## ") => {
                    cur = l[3..].trim().to_string();
                    last = None;
                }
                LineKind::ClauseItem => {
                    if matches!(last, Some(p) if p + 1 != n) {
                        breaks.entry(cur.clone()).or_insert(n);
                    }
                    last = Some(n);
                }
                LineKind::ClauseWhy => last = Some(n),
                LineKind::BadClause => out.push(a(n, "clause item is not `` - `<subject>` — <statement> ``".into())),
                _ => {}
            }
        }
        for op in &d.owns {
            let line = sections.iter().find(|s| &s.1 == op).map(|s| s.0).unwrap_or(1);
            if let Some(n) = breaks.get(op) {
                out.push(a(*n, format!("section `{op}` clause list is broken by other lines")));
            }
            if d.ledes.get(op).map(|t| t.is_empty()).unwrap_or(true) {
                out.push(a(line, format!("section `{op}` opens without a lede")));
            }
        }
        for cl in &d.clauses {
            let w = word_count(&cl.statement);
            if w > 40 {
                out.push(a(cl.line, format!("`{}` statement is {w} words, over 40", cl.id)));
            }
        }
    }
    let ids: BTreeSet<String> = c.clauses().map(|cl| cl.id.clone()).collect();
    for sc in c.scenarios() {
        let own = ids.contains(&sc.clause) && sc.clause.split('.').nth(1) == Some(sc.operation.as_str());
        if !own {
            out.push(f("anatomy", &sc.file, sc.line, "SpecScenario", format!("scenario names `{}`, not a clause of `{}`", sc.clause, sc.operation)));
        }
        let when_then = SCENARIO.is_match(&sc.text);
        let fixture = sc.text.starts_with("`tests/fixtures/") && sc.text.ends_with('`');
        if !(when_then || fixture) {
            out.push(f("anatomy", &sc.file, sc.line, "SpecScenario", format!("scenario for `{}` is neither `WHEN … THEN …` nor a `tests/fixtures/` path", sc.clause)));
        }
    }
    out
}

// ---------------------------------------------------------------- guide

static GUIDE_ITEM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^- `[^`]+` — ").unwrap());
static GUIDE_LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\]\([^)]*guide/").unwrap());

fn guide(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let g = |file: &str, line: usize, msg: String| f("guide", file, line, "SpecGuide", msg);
    let cap = c
        .reg
        .fragments
        .get("corpus")
        .and_then(|fr| fr.limit.get("corpus-guide-words"))
        .and_then(|l| l.value.as_integer())
        .unwrap_or(700) as usize;
    let errors: BTreeSet<&str> = c.reg.fragments.values().flat_map(|fr| fr.error.keys().map(String::as_str)).collect();
    let word = Regex::new(r"[A-Za-z][A-Za-z0-9_]*").unwrap();
    for name in c.reg.contracts.keys() {
        let rel = format!("spec/guide/{name}.md");
        if !c.guides().any(|d| d.rel == rel) {
            out.push(g(&rel, 0, format!("contract `{name}` has no guide")));
        }
    }
    for d in c.guides() {
        let stem = d.rel.trim_start_matches("spec/guide/").trim_end_matches(".md");
        let entry = c.reg.contracts.get(stem);
        match (&d.contract, entry) {
            (_, None) => out.push(g(&d.rel, 1, format!("`{stem}` is no registered contract"))),
            (Some(fm), Some(_)) if fm != stem => out.push(g(&d.rel, 1, format!("front matter says `{fm}`, file names `{stem}`"))),
            (None, Some(_)) => out.push(g(&d.rel, 1, "no front-matter `contract`".into())),
            _ => {}
        }
        let titles: Vec<(usize, &str)> =
            d.each().filter(|(_, l, k)| *k == LineKind::Heading && l.starts_with("# ")).map(|(n, l, _)| (n, l[2..].trim())).collect();
        match (titles.as_slice(), entry) {
            ([(n, t)], Some(e)) if *t != e.title => out.push(g(&d.rel, *n, format!("title `{t}` differs from registry `{}`", e.title))),
            ([_], _) => {}
            _ => out.push(g(&d.rel, 1, format!("{} `# ` titles, want 1", titles.len()))),
        }
        let words = words_of(d.each().filter(|(_, _, k)| !matches!(k, LineKind::Code | LineKind::Fence | LineKind::Front)).map(|(_, l, _)| l));
        if words > cap {
            out.push(g(&d.rel, 1, format!("{words} words exceed {cap}")));
        }
        for (n, l, k) in d.each() {
            if matches!(k, LineKind::Code | LineKind::Fence | LineKind::Front) {
                continue;
            }
            if GUIDE_ITEM.is_match(l) {
                out.push(g(&d.rel, n, "clause item in a guide".into()));
            }
            let plain = strip_pointers(l, " ");
            for m in word.find_iter(&plain) {
                if errors.contains(m.as_str()) {
                    out.push(g(&d.rel, n, format!("guide names error `{}`; point at its clause", m.as_str())));
                }
            }
        }
    }
    for d in c.contracts() {
        for (n, l, k) in d.each() {
            if k != LineKind::Code && GUIDE_LINK.is_match(l) {
                out.push(g(&d.rel, n, "contract file links a guide".into()));
            }
        }
    }
    out
}

// ---------------------------------------------------------------- registry

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
            if !n.gloss.trim().is_empty() {
                out.push(f("registry", &frel, 0, "SpecRegistry", format!("operation `{op}` carries a gloss; its lede in the contract file is its description")));
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
    out
}

/// Blank backticks but keep their content (a bound inside ticks still counts).
fn without_ticks_keep(s: &str) -> String {
    s.replace('`', " ")
}

// ---------------------------------------------------------------- reference

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
    out
}

// ---------------------------------------------------------------- rationale

static APPENDIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^#+\s*(open questions|out of scope|see also)\s*$").unwrap());

pub fn why_ok(why: &str) -> Result<Vec<String>, String> {
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
        Err(format!("Why `{w}` is neither record ids nor a `because` clause"))
    }
}

pub fn record_id(rel: &str) -> Option<String> {
    RECORD_FILE.captures(rel).and_then(|c| c.get(1).or(c.get(2)).map(|m| m.as_str().to_string()))
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
    }
    let mut records: BTreeSet<String> = BTreeSet::new();
    for d in c.records() {
        match record_id(&d.rel) {
            None => out.push(f("rationale", &d.rel, 1, "SpecRecord", "record file name is not P<n>-<slug>.md or A-<contract>.md".into())),
            Some(id) => {
                if !cited.contains_key(&id) {
                    out.push(f("rationale", &d.rel, 1, "SpecRecord", format!("record {id} is cited by no clause")));
                }
                if let Some(contract) = id.strip_prefix("A-") {
                    if !c.reg.contracts.contains_key(contract) {
                        out.push(f("rationale", &d.rel, 1, "SpecRecord", format!("{id} names no contract")));
                    }
                    contract_adr_anatomy(d, &id, &mut out);
                } else {
                    record_anatomy(d, &id, &mut out);
                }
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
            match k {
                LineKind::Heading => {
                    if APPENDIX.is_match(l) {
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
    let Some(start) = sections.iter().find(|s| s.1 == "Options").map(|s| s.0) else { return };
    options_table(d, start, "Options", out);
}

fn words_of<'a>(lines: impl Iterator<Item = &'a str>) -> usize {
    lines.filter(|l| !is_separator(l)).map(|l| l.replace('|', " ").split_whitespace().count()).sum()
}

/// `A-<contract>.md`: a title, a Status line, then one `## <decision>` section per decision,
/// each holding one options table within its word limit.
fn contract_adr_anatomy(d: &Doc, id: &str, out: &mut Vec<Finding>) {
    let r = |line: usize, msg: String| f("rationale", &d.rel, line, "SpecRecord", msg);
    let first = d.lines.first().cloned().unwrap_or_default();
    if !first.starts_with(&format!("# {id} — ")) {
        out.push(r(1, format!("first line is not `# {id} — <title>`")));
    }
    if !d.lines.iter().any(|l| l.starts_with("**Status:**")) {
        out.push(r(1, "no `**Status:**` line".into()));
    }
    let heads: Vec<(usize, String)> = d
        .each()
        .filter(|(_, _, k)| *k == LineKind::Heading)
        .filter_map(|(n, l, _)| l.strip_prefix("## ").map(|t| (n, t.trim().to_string())))
        .collect();
    if heads.is_empty() {
        out.push(r(1, "holds no `## <decision>` section".into()));
    }
    for (i, (n, title)) in heads.iter().enumerate() {
        let end = heads.get(i + 1).map(|h| h.0).unwrap_or(usize::MAX);
        let words = words_of(d.each().filter(|(m, _, _)| *m > *n && *m < end).map(|(_, l, _)| l));
        if words > 250 {
            out.push(r(*n, format!("section `{title}` is {words} words, over 250")));
        }
        options_table(d, *n, title, out);
    }
}

/// The table after line `start` and before the next `## `: `Option | Lost on | Cost`,
/// two to five rows, one `*(chosen)*` with `—`, every other row naming its criterion.
fn options_table(d: &Doc, start: usize, section: &str, out: &mut Vec<Finding>) {
    let r = |line: usize, msg: String| f("rationale", &d.rel, line, "SpecRecord", msg);
    let mut rows = Vec::new();
    let mut header = None;
    for (n, l, _) in d.each().filter(|(n, _, _)| *n > start) {
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
        Some((n, h)) if h != ["Option", "Lost on", "Cost"] => out.push(r(n, format!("`{section}`: Options table is not headed Option | Lost on | Cost"))),
        None => {
            out.push(r(start, format!("`{section}`: holds no Options table")));
            return;
        }
        _ => {}
    }
    if !(2..=5).contains(&rows.len()) {
        out.push(r(start, format!("`{section}`: Options holds {} rows, want 2 to 5", rows.len())));
    }
    let chosen = rows.iter().filter(|(_, c)| c.first().map(|s| s.contains("*(chosen)*")).unwrap_or(false)).count();
    if chosen != 1 {
        out.push(r(start, format!("`{section}`: {chosen} rows marked *(chosen)*, want 1")));
    }
    for (n, row) in &rows {
        let is_chosen = row.first().map(|s| s.contains("*(chosen)*")).unwrap_or(false);
        let lost = row.get(1).map(|s| s.trim()).unwrap_or("");
        if is_chosen && lost != "—" {
            out.push(r(*n, format!("`{section}`: the chosen row's Lost on is not `—`")));
        }
        if !is_chosen && (lost.is_empty() || lost == "—") {
            out.push(r(*n, format!("`{section}`: a rejected option names no criterion it lost on")));
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

/// Where a pinned artifact's final segment is defined, if anywhere.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum Resolution {
    Absent,
    Ignored,
    Unfinished,
    Defined,
}

/// Resolve an artifact path under the given roots: `Defined` when its final segment
/// is defined there, `Ignored` when that definition is a test carrying `#[ignore]`.
pub fn resolve(c: &Corpus, kind: &str, path: &str, roots: &[&str]) -> Resolution {
    let last = path.rsplit([':', '.']).next().unwrap_or(path);
    let pat = match kind {
        "test" => format!(r"\bfn\s+{}\b", regex::escape(last)),
        "theorem" => format!(r"\b(theorem|lemma)\s+{}\b", regex::escape(last)),
        _ => format!(r"\b(fn|struct|enum|trait|type|const|static|mod)\s+{}\b", regex::escape(last)),
    };
    let re = Regex::new(&pat).unwrap();
    for root in roots {
        let dir = c.root.join(root);
        let walk = walkdir::WalkDir::new(dir).into_iter().filter_entry(|e| {
            let n = e.file_name().to_string_lossy();
            n != "target" && n != "node_modules"
        });
        for e in walk.flatten() {
            let p = e.path();
            if !p.extension().map(|x| x == "rs" || x == "lean").unwrap_or(false) {
                continue;
            }
            let Ok(s) = std::fs::read_to_string(p) else { continue };
            if let Some(m) = re.find(&s) {
                return if kind == "theorem" && lean_unfinished(&s[m.start()..]) {
                    Resolution::Unfinished
                } else if kind != "test" {
                    Resolution::Defined
                } else if ignored(&s[..m.start()]) {
                    Resolution::Ignored
                } else if unfinished(&s[m.start()..]) {
                    Resolution::Unfinished
                } else {
                    Resolution::Defined
                };
            }
        }
    }
    Resolution::Absent
}

/// Whether the attribute block directly above a definition holds `#[ignore`.
fn ignored(before: &str) -> bool {
    for l in before.lines().rev().map(str::trim) {
        if l.is_empty() || l.starts_with("fn") || l.starts_with("pub") || l.starts_with("async") {
            continue;
        }
        if !(l.starts_with("#[") || l.starts_with("///") || l.starts_with("//")) {
            return false;
        }
        if l.starts_with("#[ignore") {
            return true;
        }
    }
    false
}

pub const PIN_ROOTS: [&str; 3] = ["crates", "tools", "formal"];
pub const ACCEPTANCE_ROOT: &str = "crates/acceptance";

/// Whether the Lean declaration starting `decl` — up to the next line at column zero —
/// holds `sorry` or `admit`.
fn lean_unfinished(decl: &str) -> bool {
    let mut text = String::new();
    for (i, l) in decl.lines().enumerate() {
        if i > 0 && !l.is_empty() && !l.starts_with([' ', '\t']) {
            break;
        }
        text.push_str(l);
        text.push('\n');
    }
    SORRY.is_match(&text)
}

/// Whether the body of the function starting `def` holds a line opening with `todo!`.
fn unfinished(def: &str) -> bool {
    let Some(open) = def.find('{') else { return false };
    let mut depth = 0usize;
    let mut end = def.len();
    for (i, ch) in def[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = open + i;
                    break;
                }
            }
            _ => {}
        }
    }
    def[open + 1..end].lines().any(|l| l.trim_start().starts_with("todo!"))
}

/// A milestone's acceptance test: `absent`, `open` when ignored or holding `todo!`,
/// `passing` otherwise.
pub fn acceptance_verdict(c: &Corpus, path: Option<&str>) -> &'static str {
    match path.map(|p| resolve(c, "test", p, &[ACCEPTANCE_ROOT])) {
        Some(Resolution::Defined) => "passing",
        Some(Resolution::Ignored | Resolution::Unfinished) => "open",
        _ => "absent",
    }
}

// ---------------------------------------------------------------- pins, both forms

static SORRY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(sorry|admit)\b").unwrap());
static LEAN_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^--\s*spec:\s*(\S+?)@(\S*)\s*$").unwrap());
static LEAN_DECL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:(?:private|protected|noncomputable)\s+)*(theorem|lemma|def|abbrev|structure|inductive|instance)\s+([^\s:({\[]+)").unwrap()
});
static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^//\s*spec:\s*(\S+?)@(\S*)\s*$").unwrap());
static FN_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(?:r#)?([A-Za-z_][A-Za-z0-9_]*)").unwrap()
});
static TS_ASSERTION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(?:assert\.[A-Za-z_]+|expect)\s*\(").unwrap());
static TS_DISABLED_OPTION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(?:skip|todo)\s*:\s*true\b").unwrap());
static TS_FALSE_BRANCH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bif\s*\(\s*false\s*\)").unwrap());
static TS_NODE_TEST_IMPORT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?m)^\s*import\s+(test|\{[^}]*\})\s+from\s+['"]node:test['"]"#).unwrap());
static TS_TEST_SHADOW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(?:const|let|var|function)\s+test\b").unwrap());

/// A tag's view of the function it sits above.
#[derive(Clone, Debug)]
pub struct Tagged {
    pub rev: String,
    pub function: Option<String>,
    pub ignored: bool,
    pub unfinished: bool,
}

/// One pin: a `spec/pins.toml` entry, or a `// spec: <id>@<rev>` tag above a test.
#[derive(Clone, Debug)]
pub struct Pin {
    pub clause: String,
    pub kind: String,
    pub path: String,
    pub file: String,
    pub line: usize,
    pub tag: Option<Tagged>,
}

impl Pin {
    /// The test function this pin names, when it names one.
    pub fn test_name(&self) -> Option<&str> {
        self.name_of("test")
    }

    /// The theorem this pin names, when it names one.
    pub fn theorem_name(&self) -> Option<&str> {
        self.name_of("theorem")
    }

    fn name_of(&self, kind: &str) -> Option<&str> {
        if self.kind != kind {
            return None;
        }
        match &self.tag {
            Some(t) => t.function.as_deref(),
            None => self.path.rsplit([':', '.']).next(),
        }
    }

    /// Where the pin is written: `spec/pins.toml`, or the tagged file and function.
    /// No line number, so a render survives edits above the test.
    pub fn site(&self) -> String {
        match &self.tag {
            Some(t) => format!("{}::{}", self.file, t.function.as_deref().unwrap_or("?")),
            None => self.file.clone(),
        }
    }
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum PinState {
    Performed,
    Absent,
    Ignored,
    Unfinished,
    Unattached,
    Stale,
}

/// Every `// spec:` tag under Rust, Lean and TypeScript surface roots, in path and line order.
pub fn scan_tags(c: &Corpus) -> Vec<Pin> {
    let mut out = Vec::new();
    for root in PIN_ROOTS.into_iter().chain(["apps", "packages"]) {
        let walk = walkdir::WalkDir::new(c.root.join(root)).sort_by_file_name().into_iter().filter_entry(|e| {
            let n = e.file_name().to_string_lossy();
            n != "target" && n != "node_modules" && n != "dist"
        });
        for e in walk.flatten() {
            let p = e.path();
            let ext = p.extension().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
            if ext != "rs" && ext != "lean" && ext != "ts" && ext != "tsx" {
                continue;
            }
            let Ok(s) = std::fs::read_to_string(p) else { continue };
            let rel = p.strip_prefix(&c.root).unwrap_or(p).to_string_lossy().replace('\\', "/");
            if ext == "lean" {
                out.extend(lean_tags(&s, &rel));
                continue;
            }
            if ext == "ts" || ext == "tsx" {
                out.extend(ts_tags(c, &s, &rel));
                continue;
            }
            let mut offsets = Vec::new();
            let mut at = 0;
            for l in s.split_inclusive('\n') {
                offsets.push(at);
                at += l.len();
            }
            let lines: Vec<&str> = s.lines().collect();
            for (i, l) in lines.iter().enumerate() {
                let Some(cap) = TAG.captures(l.trim()) else { continue };
                let mut tagged = Tagged { rev: cap[2].to_string(), function: None, ignored: false, unfinished: false };
                for (j, next) in lines.iter().enumerate().skip(i + 1) {
                    let t = next.trim();
                    if t.is_empty() || t.starts_with("//") || t.starts_with("#[") {
                        continue;
                    }
                    if let Some(f) = FN_LINE.captures(t) {
                        let def = offsets[j];
                        tagged.function = Some(f[1].to_string());
                        tagged.ignored = ignored(&s[..def]);
                        tagged.unfinished = unfinished(&s[def..]);
                    }
                    break;
                }
                out.push(Pin {
                    clause: cap[1].to_string(),
                    kind: "test".into(),
                    path: tagged.function.clone().unwrap_or_default(),
                    file: rel.clone(),
                    line: i + 1,
                    tag: Some(tagged),
                });
            }
        }
    }
    out
}

fn ts_tags(c: &Corpus, s: &str, rel: &str) -> Vec<Pin> {
    let lines: Vec<&str> = s.lines().collect();
    let parts: Vec<&str> = rel.split('/').collect();
    let package = if parts.len() >= 4 && matches!(parts[0], "apps" | "packages") {
        c.root.join(parts[0]).join(parts[1]).join("package.json")
    } else {
        c.root.join("missing-package.json")
    };
    let runnable = std::fs::read_to_string(package).ok().and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok()).is_some();
    let test_file = parts.get(2).is_some_and(|part| matches!(*part, "test" | "tests")) && rel.ends_with(".test.ts");
    let native_test = TS_NODE_TEST_IMPORT.captures_iter(s).any(|found| {
        &found[1] == "test" || found[1].trim_start_matches('{').trim_end_matches('}').split(',').any(|part| part.trim() == "test")
    }) && !TS_TEST_SHADOW.is_match(&ts_code(s));
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some((clause, rev)) = crate::native_ts::tag(line) else { continue };
        let mut tagged = Tagged { rev: rev.to_string(), function: None, ignored: false, unfinished: false };
        for (j, next) in lines.iter().enumerate().skip(i + 1) {
            let trimmed = next.trim();
            if trimmed.is_empty() || trimmed.starts_with("//") { continue }
            if let Some(test) = crate::native_ts::test_call(trimmed) {
                tagged.function = Some(format!("{rel}::{}", test.title));
                tagged.ignored = test.disabled;
                let unique = lines.iter().filter_map(|line| crate::native_ts::test_call(line)).filter(|found| found.title == test.title).count() == 1;
                tagged.unfinished = !runnable || !test_file || !native_test || !unique || ts_unfinished(&lines[j..].join("\n"));
            }
            break;
        }
        out.push(Pin {
            clause: clause.to_string(), kind: "test".into(),
            path: tagged.function.clone().unwrap_or_default(),
            file: rel.to_string(), line: i + 1, tag: Some(tagged),
        });
    }
    out
}

fn ts_unfinished(test: &str) -> bool {
    let Some(arrow) = test.find("=>") else { return true };
    if TS_DISABLED_OPTION.is_match(&test[..arrow]) { return true }
    let Some(open) = test[arrow..].find('{').map(|n| arrow + n) else { return true };
    let mut depth = 0usize;
    let mut close = None;
    for (offset, ch) in test[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 { close = Some(open + offset); break; }
            }
            _ => {}
        }
    }
    let Some(close) = close else { return true };
    let body = test[open + 1..close].trim();
    let code = ts_code(body);
    let first_assertion = TS_ASSERTION.find(&code).map(|found| found.start());
    let nested_before_assertion = code.find("=>").is_some_and(|arrow| first_assertion.is_none_or(|assertion| arrow < assertion));
    body.is_empty() || body.contains("TODO") || body.contains("todo(") || body.contains("not implemented")
        || code.contains("assert.ok(true)") || TS_FALSE_BRANCH.is_match(&code) || nested_before_assertion || first_assertion.is_none()
}

fn ts_code(body: &str) -> String {
    let mut output = String::new();
    let mut chars = body.chars().peekable();
    let mut quote = None;
    while let Some(ch) = chars.next() {
        if let Some(delimiter) = quote {
            if ch == '\\' { chars.next(); }
            else if ch == delimiter { quote = None; }
            output.push(' ');
        } else if matches!(ch, '\'' | '"' | '`') {
            quote = Some(ch);
            output.push(' ');
        } else if ch == '/' && chars.peek() == Some(&'/') {
            chars.by_ref().take_while(|next| *next != '\n').for_each(|_| {});
            output.push('\n');
        } else if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            while let Some(next) = chars.next() {
                if next == '*' && chars.peek() == Some(&'/') { chars.next(); break; }
            }
            output.push(' ');
        } else {
            output.push(ch);
        }
    }
    output
}

/// `-- spec: <id>@<rev>` tags in one Lean file, each attached to the declaration below it
/// once blank lines, comments, docstrings and attributes are skipped.
fn lean_tags(s: &str, rel: &str) -> Vec<Pin> {
    let mut out = Vec::new();
    let lines: Vec<&str> = s.lines().collect();
    let mut offsets = Vec::new();
    let mut at = 0;
    for l in s.split_inclusive('\n') {
        offsets.push(at);
        at += l.len();
    }
    for (i, l) in lines.iter().enumerate() {
        let Some(cap) = LEAN_TAG.captures(l.trim()) else { continue };
        let mut tagged = Tagged { rev: cap[2].to_string(), function: None, ignored: false, unfinished: false };
        let mut kind = "theorem";
        let mut in_doc = false;
        for (j, next) in lines.iter().enumerate().skip(i + 1) {
            let t = next.trim();
            if in_doc {
                in_doc = !t.contains("-/");
                continue;
            }
            if t.starts_with("/-") {
                in_doc = !t.contains("-/");
                continue;
            }
            if t.is_empty() || t.starts_with("--") || t.starts_with("@[") {
                continue;
            }
            if let Some(d) = LEAN_DECL.captures(t) {
                if !matches!(&d[1], "theorem" | "lemma") {
                    kind = "item";
                }
                tagged.function = Some(d[2].to_string());
                tagged.unfinished = lean_unfinished(&s[offsets[j]..]);
            }
            break;
        }
        out.push(Pin {
            clause: cap[1].to_string(),
            kind: kind.into(),
            path: tagged.function.clone().unwrap_or_default(),
            file: rel.to_string(),
            line: i + 1,
            tag: Some(tagged),
        });
    }
    out
}

/// Every pin, `spec/pins.toml` entries first, grouped by the clause id each names.
pub fn pins_by_clause(c: &Corpus) -> BTreeMap<String, Vec<Pin>> {
    let mut out: BTreeMap<String, Vec<Pin>> = BTreeMap::new();
    for (id, pin) in load_pins(c).pin {
        let (kind, path) = pin.into_iter().next().unwrap_or_default();
        out.entry(id.clone()).or_default().push(Pin { clause: id, kind, path, file: "spec/pins.toml".into(), line: 0, tag: None });
    }
    for pin in scan_tags(c) {
        out.entry(pin.clause.clone()).or_default().push(pin);
    }
    out
}

pub fn pin_state(c: &Corpus, pin: &Pin) -> PinState {
    match &pin.tag {
        None => match resolve(c, &pin.kind, &pin.path, &PIN_ROOTS) {
            Resolution::Defined => PinState::Performed,
            Resolution::Ignored => PinState::Ignored,
            Resolution::Unfinished => PinState::Unfinished,
            Resolution::Absent => PinState::Absent,
        },
        Some(t) => {
            let current = c.clauses().find(|cl| cl.id == pin.clause).map(|cl| statement_rev(&cl.statement));
            if t.function.is_none() {
                PinState::Unattached
            } else if current.as_deref() != Some(t.rev.as_str()) {
                PinState::Stale
            } else if t.ignored {
                PinState::Ignored
            } else if t.unfinished {
                PinState::Unfinished
            } else {
                PinState::Performed
            }
        }
    }
}

/// The distinct test functions pinning one clause.
pub fn pinned_tests(pins: &[Pin]) -> BTreeSet<&str> {
    pins.iter().filter_map(Pin::test_name).collect()
}

/// The distinct theorems pinning one clause.
pub fn pinned_theorems(pins: &[Pin]) -> BTreeSet<&str> {
    pins.iter().filter_map(Pin::theorem_name).collect()
}

/// `performed` when every pin of the clause performs and at most one test and one
/// theorem pin it; `broken` otherwise.
pub fn clause_verdict(c: &Corpus, pins: &[Pin]) -> &'static str {
    let single = pinned_tests(pins).len() <= 1 && pinned_theorems(pins).len() <= 1;
    if single && pins.iter().all(|p| pin_state(c, p) == PinState::Performed) {
        "performed"
    } else {
        "broken"
    }
}

/// A milestone's `Reach:` and `Acceptance:` lines: heading, heading line, reach text, acceptance test path.
pub struct MilestoneLines {
    pub heading: String,
    pub line: usize,
    pub reach: Option<String>,
    pub acceptance: Option<String>,
    /// A `Depth: operation` line: the milestone admits only refusal and limit clauses.
    pub operation_depth: bool,
}

pub fn milestone_lines(c: &Corpus) -> Vec<MilestoneLines> {
    let mut out: Vec<MilestoneLines> = Vec::new();
    let Some(d) = c.docs.iter().find(|d| d.role == Role::Plan) else { return out };
    for (n, l, k) in d.each() {
        if k == LineKind::Heading && l.starts_with("## ") {
            out.push(MilestoneLines { heading: l[3..].trim().to_string(), line: n, reach: None, acceptance: None, operation_depth: false });
        } else if let Some(m) = out.last_mut() {
            if let Some(rest) = l.strip_prefix("Reach: ") {
                m.reach = Some(rest.trim().to_string());
            } else if l.trim() == "Depth: operation" {
                m.operation_depth = true;
            } else if let Some(rest) = l.strip_prefix("Acceptance: ") {
                m.acceptance = tick_spans(rest).into_iter().next().map(|(_, t)| t);
            }
        }
    }
    out
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
    let all = pins_by_clause(c);
    let clauses = c.clause_map();
    let mut count: BTreeMap<String, i64> = BTreeMap::new();
    for (id, group) in &all {
        let Some(cl) = clauses.get(id) else {
            for p in group {
                let what = if p.tag.is_some() { "tag" } else { "pin" };
                out.push(f("state", &p.file, p.line, "SpecBrokenPin", format!("{what} `{id}` names no clause")));
            }
            continue;
        };
        for p in group {
            if p.kind == "item" && cl.kind != "behavior" {
                out.push(f("state", &p.file, p.line, "SpecBrokenPin", format!("{} `{id}` takes a test or theorem, not an item", cl.kind)));
            }
            let msg = match pin_state(c, p) {
                PinState::Performed => continue,
                PinState::Ignored => format!("pin for `{id}` names a test carrying `#[ignore]`"),
                PinState::Unfinished if p.kind == "test" => format!("pin for `{id}` names a test whose body still opens a line with `todo!`"),
                PinState::Unfinished => format!("pin for `{id}` names a declaration still holding `sorry`"),
                PinState::Absent => format!("pin for `{id}` does not resolve under crates/, tools/ or formal/"),
                PinState::Unattached => format!("tag for `{id}` sits above no function"),
                PinState::Stale => {
                    let rev = p.tag.as_ref().map(|t| t.rev.as_str()).unwrap_or("");
                    let msg = format!("tag for `{id}` records rev `{rev}`; the statement's rev is `{}`", statement_rev(&cl.statement));
                    out.push(f("state", &p.file, p.line, "SpecStalePin", msg));
                    continue;
                }
            };
            out.push(f("state", &p.file, p.line, "SpecBrokenPin", msg));
        }
        let theorems = pinned_theorems(group);
        if theorems.len() > 1 {
            let names: Vec<&str> = theorems.into_iter().collect();
            out.push(f("state", &group[0].file, group[0].line, "SpecBrokenPin", format!("`{id}` is pinned to two theorems, {}", names.join(" and "))));
        }
        let tests = pinned_tests(group);
        if tests.len() > 1 {
            let names: Vec<&str> = tests.into_iter().collect();
            let sites: Vec<String> = group.iter().map(Pin::site).collect();
            out.push(f(
                "state",
                &group[0].file,
                group[0].line,
                "SpecBrokenPin",
                format!("`{id}` is pinned to two tests, {} ({})", names.join(" and "), sites.join(", ")),
            ));
        }
        if clause_verdict(c, group) == "performed" {
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
    let (claim, found) = expand_roadmap(c);
    out.extend(found);
    for m in milestone_lines(c) {
        if m.reach.is_none() {
            out.push(f("state", "spec/roadmap.md", m.line, "SpecRoadmap", format!("milestone `{}` carries no `Reach:` line", m.heading)));
        }
        let Some(test) = m.acceptance.as_deref() else {
            out.push(f("state", "spec/roadmap.md", m.line, "SpecRoadmap", format!("milestone `{}` carries no `Acceptance:` line", m.heading)));
            continue;
        };
        let pinned = all.keys().filter_map(|id| clauses.get(id)).any(|cl| {
            claim.get(&format!("{}.{}", cl.contract, cl.operation)) == Some(&m.heading)
        });
        if m.operation_depth {
            for cl in c.clauses().filter(|cl| cl.kind == "behavior") {
                if claim.get(&format!("{}.{}", cl.contract, cl.operation)) == Some(&m.heading) {
                    out.push(f(
                        "state",
                        &cl.file,
                        cl.line,
                        "SpecDeferredBehavior",
                        format!("behavior clause `{}` in milestone `{}`, which carries `Depth: operation`", cl.id, m.heading),
                    ));
                }
            }
        }
        if pinned && acceptance_verdict(c, Some(test)) == "absent" {
            out.push(f(
                "state",
                "spec/roadmap.md",
                m.line,
                "SpecAcceptanceMissing",
                format!("milestone `{}` holds a pinned clause while `{test}` is not defined under {ACCEPTANCE_ROOT}/", m.heading),
            ));
        }
    }
    out
}

// ---------------------------------------------------------------- render

static DATED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(planned|not yet|currently|today|shipped|implemented|previously|used to|legacy)\b|\b(TODO|FIXME|WIP)\b").unwrap()
});
static COUNTERFACTUAL: LazyLock<Regex> = LazyLock::new(|| word_re(&["will", "would", "shall"]));
static BANNED: LazyLock<Regex> = LazyLock::new(|| word_re(&["seam", "seams", "load-bearing", "wedge", "rung", "rungs", "land-grab", "axiom", "axioms"]));

fn render(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut generated = vec![
        ("spec/status.md".to_string(), crate::status_text(c)),
        ("spec/spec.lock.json".to_string(), crate::lock_text(c)),
        ("spec/targets.md".to_string(), crate::targets::page(c)),
    ];
    for (contract, text) in crate::cards::pages(c) {
        generated.push((format!("spec/cards/{contract}.md"), text));
    }
    for (rel, want) in generated {
        let rel = rel.as_str();
        let have = std::fs::read_to_string(c.root.join(rel)).unwrap_or_default();
        if have != want {
            out.push(f("render", rel, 0, "SpecStaleRender", format!("{rel} differs from regeneration; run `contextful-spec state` and `extract`")));
        }
    }
    let local = Regex::new(r"(/Users/|/home/|\$HOME/|(^|[\s(`])~/)").unwrap();
    for d in &c.docs {
        for (n, l, k) in d.each() {
            if local.is_match(&without_ticks(l)) {
                out.push(f("render", &d.rel, n, "SpecLocalPath", "absolute local path".into()));
            }
            if matches!(k, LineKind::Code | LineKind::Fence | LineKind::Front) {
                continue;
            }
            let plain = without_ticks(l);
            if d.role != Role::Plan && (DATED.is_match(&plain) || ISO_DATE.is_match(&plain)) {
                out.push(f("render", &d.rel, n, "SpecDatedProse", "build-state or dated vocabulary".into()));
            }
            if matches!(d.role, Role::Contract | Role::Guide) && COUNTERFACTUAL.is_match(&plain) {
                out.push(f("render", &d.rel, n, "SpecCounterfactual", format!("`{}` in a contract file or guide", COUNTERFACTUAL.find(&plain).unwrap().as_str())));
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
