//! `corpus.topic`: topic pages under `spec/topics/`, their entries in
//! `spec/terms/topic.toml`, and the bounds that keep each a route into the contracts.

use crate::corpus::*;
use crate::util::*;
use regex::Regex;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

pub const REGISTRY: &str = "spec/terms/topic.toml";

static CLAUSE_ITEM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^- `[^`]+` — ").unwrap());
static TOPIC_LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\]\([^)]*topics/").unwrap());
static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z][A-Za-z0-9_]*").unwrap());

#[derive(Deserialize, Clone, Debug)]
pub struct TopicEntry {
    pub title: String,
    #[serde(default)]
    pub answers: Vec<String>,
}

#[derive(Deserialize, Default)]
struct TopicFile {
    #[serde(default)]
    topic: BTreeMap<String, TopicEntry>,
}

/// The registered topics by slug, or the parse error.
pub fn registry(c: &Corpus) -> Result<BTreeMap<String, TopicEntry>, String> {
    match std::fs::read_to_string(c.root.join(REGISTRY)) {
        Err(_) => Ok(BTreeMap::new()),
        Ok(text) => toml::from_str::<TopicFile>(&text).map(|f| f.topic).map_err(|e| e.to_string().replace('\n', " ")),
    }
}

fn bound(c: &Corpus, name: &str, default: usize) -> usize {
    c.reg.fragments.get("corpus").and_then(|fr| fr.limit.get(name)).and_then(|l| l.value.as_integer()).map_or(default, |v| v as usize)
}

fn t(file: &str, line: usize, msg: String) -> Finding {
    Finding::new("topic", file, line, "SpecTopic", msg)
}

/// `corpus.topic.bad-topic`.
pub fn check(c: &Corpus) -> Vec<Finding> {
    let mut out = Vec::new();
    let entries = match registry(c) {
        Ok(e) => e,
        Err(e) => return vec![t(REGISTRY, 0, e)],
    };
    let pages: Vec<&Doc> = c.topics().collect();
    let words_cap = bound(c, "corpus-topic-words", 600);
    let pages_cap = bound(c, "corpus-topic-pages", 8);
    let pointer_cap = bound(c, "corpus-topic-contract-pointers", 3);
    let clauses = c.clause_map();
    let errors: BTreeSet<&str> = c.reg.fragments.values().flat_map(|fr| fr.error.keys().map(String::as_str)).collect();
    let stems: BTreeSet<&str> = pages.iter().map(|d| stem(d)).collect();
    let count = stems.len().max(entries.len());
    if count > pages_cap {
        out.push(t(REGISTRY, 0, format!("{count} topic pages exceed {pages_cap}")));
    }
    for slug in entries.keys().filter(|slug| !stems.contains(slug.as_str())) {
        out.push(t(REGISTRY, 0, format!("topic `{slug}` has no page spec/topics/{slug}.md")));
    }
    for d in &pages {
        let slug = stem(d);
        let Some(entry) = entries.get(slug) else {
            out.push(t(&d.rel, 1, format!("topic page `{slug}` has no entry in {REGISTRY}")));
            continue;
        };
        if d.lines.first().map(|l| l.trim()) != Some("---") || !d.lines.iter().any(|l| l.trim() == format!("topic: {slug}")) {
            out.push(t(&d.rel, 1, format!("front matter does not say `topic: {slug}`")));
        }
        let titles: Vec<(usize, &str)> =
            d.each().filter(|(_, l, k)| *k == LineKind::Heading && l.starts_with("# ")).map(|(n, l, _)| (n, l[2..].trim())).collect();
        match titles.as_slice() {
            [(n, title)] if *title != entry.title => out.push(t(&d.rel, *n, format!("title `{title}` differs from registry `{}`", entry.title))),
            [_] => {}
            _ => out.push(t(&d.rel, 1, format!("{} `# ` titles, want 1", titles.len()))),
        }
        let prose: Vec<(usize, &str)> =
            d.each().filter(|(_, _, k)| !matches!(k, LineKind::Code | LineKind::Fence | LineKind::Front)).map(|(n, l, _)| (n, l)).collect();
        let words = crate::checks::words_of(prose.iter().map(|(_, l)| *l));
        if words > words_cap {
            out.push(t(&d.rel, 1, format!("{words} words exceed {words_cap}")));
        }
        let mut points: BTreeSet<String> = BTreeSet::new();
        for (n, l) in &prose {
            if CLAUSE_ITEM.is_match(l) {
                out.push(t(&d.rel, *n, "clause item in a topic page".into()));
            }
            for m in WORD.find_iter(&strip_pointers(l, " ")) {
                if errors.contains(m.as_str()) {
                    out.push(t(&d.rel, *n, format!("topic page names error `{}`; point at its clause", m.as_str())));
                }
            }
            points.extend(pointers(&without_ticks(l)));
        }
        let mut by_contract: BTreeMap<&str, usize> = BTreeMap::new();
        for p in &points {
            *by_contract.entry(p.split('.').next().unwrap_or_default()).or_default() += 1;
        }
        for (contract, n) in by_contract.into_iter().filter(|(_, n)| *n > pointer_cap) {
            out.push(t(&d.rel, 1, format!("{n} pointers into `{contract}` exceed {pointer_cap}")));
        }
        let mut reach: BTreeSet<String> = points.clone();
        for p in &points {
            if let Some(cl) = clauses.get(p) {
                reach.extend(pointers(&cl.statement));
            }
        }
        for answer in &entry.answers {
            if !clauses.contains_key(answer) {
                out.push(t(REGISTRY, 0, format!("topic `{slug}` answers missing clause `{answer}`")));
            } else if !reach.contains(answer) {
                out.push(t(&d.rel, 1, format!("answer `{answer}` is not within one pointer step of the page")));
            }
        }
    }
    for d in c.contracts() {
        for (n, l, k) in d.each() {
            if k != LineKind::Code && TOPIC_LINK.is_match(l) {
                out.push(t(&d.rel, n, "contract file links a topic page".into()));
            }
        }
    }
    out
}

fn stem(d: &Doc) -> &str {
    d.rel.trim_start_matches("spec/topics/").trim_end_matches(".md")
}
