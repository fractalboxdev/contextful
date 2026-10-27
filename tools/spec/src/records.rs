//! Contract ADR sections (`corpus.rationale.contract-adr`): each `## <slug> — <decision>`
//! section, the clauses citing it as `A-<contract>#<slug>`, the threshold a section
//! answers to, the `why-unscoped` ratchet, `why-migrate`, and the text a slice inlines.

use crate::checks::{record_id, why_ok};
use crate::corpus::*;
use crate::ratchet::{ceiling, compare, Live};
use crate::util::tick_spans;
use anyhow::{bail, Result};
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

pub static HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^## ([a-z0-9-]{1,40}) — (.+)$").unwrap());

/// One `## <slug> — <decision>` section of a contract ADR.
pub struct Section {
    pub slug: String,
    pub line: usize,
    pub text: String,
    pub revisit: bool,
}

/// The sections of contract ADR `d`, each heading a well-formed slug.
pub fn sections(d: &Doc) -> Vec<Section> {
    let heads: Vec<(usize, &str)> = d.each().filter(|(_, l, k)| *k == LineKind::Heading && l.starts_with("## ")).map(|(n, l, _)| (n, l)).collect();
    heads
        .iter()
        .enumerate()
        .filter_map(|(i, (n, l))| {
            let slug = HEADING.captures(l)?[1].to_string();
            let end = heads.get(i + 1).map(|h| h.0).unwrap_or(d.lines.len() + 1);
            let body: Vec<&str> = d.lines[n - 1..end - 1].iter().map(String::as_str).collect();
            let revisit = body.iter().any(|l| l.starts_with("Revisit:"));
            Some(Section { slug, line: *n, text: body.join("\n").trim_end().to_string(), revisit })
        })
        .collect()
}

/// Every contract ADR's sections, keyed `A-<contract>#<slug>`.
pub fn all_sections(c: &Corpus) -> BTreeMap<String, (String, Section)> {
    let mut out = BTreeMap::new();
    for d in c.records() {
        let Some(id) = record_id(&d.rel).filter(|id| id.starts_with("A-")) else { continue };
        for s in sections(d) {
            out.insert(format!("{id}#{}", s.slug), (d.rel.clone(), s));
        }
    }
    out
}

/// The clauses citing each record id, as written: `P3`, `A-store` or `A-store#<slug>`.
pub fn citations(c: &Corpus) -> BTreeMap<String, Vec<&Clause>> {
    let mut out: BTreeMap<String, Vec<&Clause>> = BTreeMap::new();
    for cl in c.clauses() {
        for id in why_ok(&cl.why).unwrap_or_default() {
            out.entry(id).or_default().push(cl);
        }
    }
    out
}

/// The clause ids citing each contract ADR section, keyed `A-<contract>#<slug>`; a
/// section no clause cites maps to an empty list.
pub fn cited_by(c: &Corpus) -> BTreeMap<String, Vec<String>> {
    let cites = citations(c);
    all_sections(c).into_keys().map(|key| {
        let ids = cites.get(&key).map(|cls| cls.iter().map(|cl| cl.id.clone()).collect()).unwrap_or_default();
        (key, ids)
    }).collect()
}

const UNSCOPED: &str = "why-unscoped";

fn bare(id: &str) -> bool {
    id.starts_with("A-") && !id.contains('#')
}

/// The `why-unscoped` ratchet: Why citations naming a contract ADR without a section.
pub fn unscoped_live(c: &Corpus) -> Live {
    let count = c.clauses().map(|cl| why_ok(&cl.why).unwrap_or_default().iter().filter(|id| bare(id)).count()).sum();
    Live { key: UNSCOPED.into(), count, check: "rationale", code: "SpecWhyUnscoped" }
}

/// Section citations naming no section, sections under `corpus.rationale.record-threshold`
/// in each ADR every citation of which names a section, and the `why-unscoped` ratchet.
pub fn check(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let r = |file: &str, line: usize, msg: String| Finding::new("rationale", file, line, "SpecRecord", msg);
    let sections = all_sections(c);
    let cites = citations(c);
    let mut scoped_files: BTreeMap<String, bool> = BTreeMap::new();
    for (id, cls) in &cites {
        let Some(file_id) = id.split('#').next().filter(|f| f.starts_with("A-")) else { continue };
        let fully = scoped_files.entry(file_id.to_string()).or_insert(true);
        if bare(id) {
            *fully = false;
        } else if !sections.contains_key(id) {
            for cl in cls {
                out.push(r(&cl.file, cl.line, format!("`{}` cites missing section {id}", cl.id)));
            }
        }
    }
    for (key, (file, s)) in &sections {
        let (file_id, _) = key.split_once('#').unwrap_or_default();
        if !scoped_files.get(file_id).copied().unwrap_or(false) {
            continue;
        }
        let citing = cites.get(key).map(Vec::as_slice).unwrap_or_default();
        let own = file_id.trim_start_matches("A-");
        let foreign = citing.iter().any(|cl| cl.contract != own);
        if citing.len() < 2 && !foreign && !s.revisit {
            let n = citing.len();
            out.push(r(file, s.line, format!("section `{key}` is cited by {n} clause{} of its own contract and names no `Revisit:`; its decision belongs in a `because` cell", if n == 1 { "" } else { "s" })));
        }
    }
    let live = unscoped_live(c);
    match ceiling(c, UNSCOPED) {
        Some(cap) if live.count <= cap => out.extend(compare(c, &live)),
        _ => {
            for cl in c.clauses() {
                for id in why_ok(&cl.why).unwrap_or_default().iter().filter(|id| bare(id)) {
                    out.push(Finding::new("rationale", &cl.file, cl.line, "SpecWhyUnscoped", format!("`{}` cites {id} without a section", cl.id)));
                }
            }
        }
    }
    out
}

/// The sections of ADR `adr` naming `cl`: those holding its id, else those holding its
/// `<contract>.<operation>`, each as a backticked token.
fn candidates<'a>(cl: &Clause, adr: &'a [(String, BTreeSet<String>)]) -> Vec<&'a str> {
    let op = format!("{}.{}", cl.contract, cl.operation);
    let by_id: Vec<&str> = adr.iter().filter(|(_, ticks)| ticks.contains(&cl.id)).map(|(s, _)| s.as_str()).collect();
    if !by_id.is_empty() {
        return by_id;
    }
    adr.iter().filter(|(_, ticks)| ticks.contains(&op) || ticks.iter().any(|t| t.starts_with(&format!("{op}.")))).map(|(s, _)| s.as_str()).collect()
}

/// `contextful-spec why-migrate <A-id>...`: rewrite each bare citation of a named ADR
/// whose clause one section alone names, and print the rest per contract.
pub fn migrate(c: &Corpus, ids: &[String]) -> Result<()> {
    // Each clause's Why as written and its citations after every rewrite so far, keyed by
    // file and line, so two named ADRs on one Why rewrite it in turn.
    let mut rewrites: BTreeMap<(String, usize), (String, Vec<String>)> = BTreeMap::new();
    let mut unresolved: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut total = 0;
    for id in ids {
        let Some(d) = c.records().find(|d| record_id(&d.rel).as_deref() == Some(id.as_str())) else { bail!("why-migrate: `{id}` names no contract ADR") };
        let adr: Vec<(String, BTreeSet<String>)> =
            sections(d).into_iter().map(|s| (s.slug, tick_spans(&s.text).into_iter().map(|(_, t)| t).collect())).collect();
        for cl in c.clauses() {
            let cited = why_ok(&cl.why).unwrap_or_default();
            if !cited.iter().any(|w| w == id) {
                continue;
            }
            match candidates(cl, &adr).as_slice() {
                [one] => {
                    let (_, current) = rewrites.entry((cl.file.clone(), cl.line)).or_insert_with(|| (cl.why.clone(), cited));
                    for w in current.iter_mut().filter(|w| *w == id) {
                        *w = format!("{id}#{one}");
                        total += 1;
                    }
                }
                many => {
                    let names = if many.is_empty() { "no section".to_string() } else { many.iter().map(|s| format!("#{s}")).collect::<Vec<_>>().join(" ") };
                    unresolved.entry(cl.contract.clone()).or_default().push(format!("`{}` cites {id}: {names}", cl.id));
                }
            }
        }
    }
    // Every file is rewritten in memory first, so a line that no longer matches leaves the
    // corpus untouched.
    let mut files: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for ((rel, line), (old, new)) in &rewrites {
        if !files.contains_key(rel) {
            let text = std::fs::read_to_string(c.root.join(rel))?;
            files.insert(rel.clone(), text.lines().map(str::to_string).collect());
        }
        let lines = files.get_mut(rel).expect("read above");
        // the Why sits on the line after its item
        let want = format!("  *{old}*");
        match lines.get_mut(*line) {
            Some(why) if *why == want => *why = format!("  *{}*", new.join(", ")),
            _ => bail!("why-migrate: {rel}:{} is not `{want}`", line + 1),
        }
    }
    for (rel, lines) in &files {
        std::fs::write(c.root.join(rel), lines.join("\n") + "\n")?;
    }
    println!("rewrote {total} citations");
    for (contract, rest) in &unresolved {
        println!("\n{contract}: {} unresolved", rest.len());
        for r in rest {
            println!("  {r}");
        }
    }
    Ok(())
}

/// The text a slice inlines for a cited record: one section for `A-<contract>#<slug>`,
/// the whole file otherwise.
pub fn record_text(c: &Corpus, id: &str) -> Option<(String, String)> {
    match id.split_once('#') {
        Some(_) => all_sections(c).remove(id).map(|(file, s)| (file, s.text)),
        None => c.records().find(|d| record_id(&d.rel).as_deref() == Some(id)).map(|d| (d.rel.clone(), d.lines.join("\n"))),
    }
}
