//! Size ratchets: each contract's and each contract ADR's words
//! (`corpus.anatomy.word-ceilings`), the long-operation advisory
//! (`corpus.anatomy.long-operation`) and unowned clauses (`corpus.state.unowned`).

use crate::checks::{open_operations, pins_by_clause, record_id};
use crate::corpus::*;
use crate::util::tick_spans;
use crate::ratchet::{compare, ceiling, Live};
use regex::Regex;
use std::collections::BTreeMap;
use std::sync::LazyLock;

static UNSETTLED_AFFECTS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"affects: ([a-z0-9-]+\.[a-z0-9-]+)\s*$").unwrap());

/// Each contract's statement, Why and lede words, keyed `contract-words.<contract>`.
pub fn contract_words(c: &Corpus) -> Vec<Live> {
    let mut words: BTreeMap<String, usize> = c.reg.contracts.keys().map(|k| (k.clone(), 0)).collect();
    for d in c.contracts() {
        let Some(contract) = d.contract.as_ref() else { continue };
        let ledes: usize = d.ledes.values().chain(d.groups.values()).map(|t| word_count(t)).sum();
        let clauses: usize = d.clauses.iter().map(|cl| word_count(&cl.statement) + word_count(&cl.why)).sum();
        *words.entry(contract.clone()).or_default() += ledes + clauses;
    }
    words.into_iter().map(|(k, count)| Live { key: format!("contract-words.{k}"), count, check: "anatomy", code: "SpecWordCeiling" }).collect()
}

/// Each contract ADR's words outside fences, a table's pipes and rule rows, keyed
/// `adr-words.A-<contract>`.
pub fn adr_words(c: &Corpus) -> Vec<Live> {
    c.records()
        .filter_map(|d| {
            let id = record_id(&d.rel).filter(|id| id.starts_with("A-"))?;
            let count = d
                .each()
                .filter(|(_, l, k)| !matches!(k, LineKind::Code | LineKind::Fence) && !is_separator(l))
                .map(|(_, l, _)| l.replace('|', " ").split_whitespace().count())
                .sum();
            Some(Live { key: format!("adr-words.{id}"), count, check: "rationale", code: "SpecWordCeiling" })
        })
        .collect()
}

/// The words a contract and its ADR hold, against their ceilings.
pub fn word_findings(c: &Corpus, check: &str) -> Vec<Finding> {
    let lives = if check == "anatomy" { contract_words(c) } else { adr_words(c) };
    lives.iter().filter_map(|l| compare(c, l)).collect()
}

/// One advisory line per operation holding more clauses than `corpus-operation-clauses`.
pub fn advisories(c: &Corpus) -> Vec<String> {
    let bound = c
        .reg
        .fragments
        .get("corpus")
        .and_then(|fr| fr.limit.get("corpus-operation-clauses"))
        .and_then(|l| l.value.as_integer())
        .unwrap_or(30) as usize;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for cl in c.clauses() {
        *counts.entry(format!("{}.{}", cl.contract, cl.operation)).or_default() += 1;
    }
    counts.into_iter().filter(|(_, n)| *n > bound).map(|(op, n)| format!("advisory: `{op}` holds {n} clauses, over {bound}")).collect()
}

/// Every unowned clause: unpinned, in no operation an open milestone names, and with
/// no `unsettled:` line of its operation naming its subject.
fn unowned_clauses(c: &Corpus) -> Vec<&Clause> {
    let pinned = pins_by_clause(c);
    let open = open_operations(c);
    let mut questions: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for d in c.contracts() {
        for (_, l, k) in d.each() {
            if k != LineKind::Unsettled {
                continue;
            }
            if let Some(cap) = UNSETTLED_AFFECTS.captures(l.trim()) {
                questions.entry(cap[1].to_string()).or_default().push(l.to_string());
            }
        }
    }
    c.clauses()
        .filter(|cl| !pinned.contains_key(&cl.id))
        .filter(|cl| {
            let op = format!("{}.{}", cl.contract, cl.operation);
            let named = questions.get(&op).is_some_and(|qs| qs.iter().any(|q| tick_spans(q).iter().any(|(_, t)| *t == cl.subject || *t == cl.id)));
            !open.contains(&op) && !named
        })
        .collect()
}

const UNOWNED: &str = "clause-unowned";

/// The `clause-unowned` ratchet.
pub fn unowned_live(c: &Corpus) -> Live {
    Live { key: UNOWNED.into(), count: unowned_clauses(c).len(), check: "state", code: "SpecClauseUnowned" }
}

/// Each unowned clause when their count passes the ceiling, else the ratchet's own finding.
pub fn unowned_findings(c: &Corpus) -> Vec<Finding> {
    let clauses = unowned_clauses(c);
    let live = Live { key: UNOWNED.into(), count: clauses.len(), check: "state", code: "SpecClauseUnowned" };
    match ceiling(c, UNOWNED) {
        Some(cap) if clauses.len() <= cap => compare(c, &live).into_iter().collect(),
        _ => clauses
            .iter()
            .map(|cl| {
                let msg = format!("`{}` is unowned: unpinned, in no open milestone's operation, and named by no `unsettled:` line", cl.id);
                Finding::new("state", &cl.file, cl.line, "SpecClauseUnowned", msg)
            })
            .collect(),
    }
}
