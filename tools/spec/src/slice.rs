//! `contextful-spec slice`: a self-contained context pack for one operation, one
//! contract or one roadmap milestone — its clauses and operation ledes, the `{{id}}` closure they
//! point into, the records their Why lines cite, and the errors and bounds they own.

use crate::checks;
use crate::corpus::*;
use anyhow::{bail, Result};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

#[derive(Serialize)]
pub struct Row {
    pub id: String,
    pub statement: String,
    pub why: String,
    pub file: String,
    pub line: usize,
}

#[derive(Serialize)]
pub struct Record {
    pub id: String,
    pub file: String,
    pub text: String,
}

#[derive(Serialize)]
pub struct Owned {
    pub id: String,
    pub clause: String,
    pub gloss: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

#[derive(Serialize)]
pub struct MilestoneLine {
    pub heading: String,
    pub reach: Option<String>,
    pub acceptance: Option<String>,
}

#[derive(Serialize)]
pub struct Summary {
    pub clauses: usize,
    pub pointed: usize,
    pub pointers: usize,
    pub records: usize,
}

#[derive(Serialize)]
pub struct Slice {
    pub target: String,
    pub operations: Vec<String>,
    /// Each named operation's lede, by `<contract>.<operation>`.
    pub ledes: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub milestone: Option<MilestoneLine>,
    pub clauses: Vec<Row>,
    pub pointed: Vec<Row>,
    pub errors: Vec<Owned>,
    pub limits: Vec<Owned>,
    pub records: Vec<Record>,
    pub summary: Summary,
}

/// The operations a target names, and the milestone when the target is one.
fn resolve(c: &Corpus, target: &str) -> Result<(Vec<String>, Option<MilestoneLine>)> {
    let ops_of = |contract: &str| -> Vec<String> {
        c.reg
            .fragments
            .get(contract)
            .map(|f| f.operation.keys().map(|o| format!("{contract}.{o}")).collect())
            .unwrap_or_default()
    };
    if let Some((contract, op)) = target.split_once('.') {
        let ops = ops_of(contract);
        if op == "*" {
            if ops.is_empty() {
                bail!("slice: `{target}` names no contract");
            }
            return Ok((ops, None));
        }
        if !ops.iter().any(|o| o == target) {
            bail!("slice: `{target}` names no operation");
        }
        return Ok((vec![target.to_string()], None));
    }
    let number = |h: &str| h.split_whitespace().next().unwrap_or("").to_string();
    let Some(m) = checks::milestone_lines(c).into_iter().find(|m| number(&m.heading) == target) else {
        bail!("slice: `{target}` names no operation, contract or milestone");
    };
    let (claim, _) = checks::expand_roadmap(c);
    let ops = claim.into_iter().filter(|(_, h)| *h == m.heading).map(|(o, _)| o).collect();
    Ok((ops, Some(MilestoneLine { heading: m.heading, reach: m.reach, acceptance: m.acceptance })))
}

fn row(cl: &Clause) -> Row {
    Row { id: cl.id.clone(), statement: cl.statement.clone(), why: cl.why.clone(), file: cl.file.clone(), line: cl.line }
}

pub fn build(c: &Corpus, target: &str) -> Result<Slice> {
    let (operations, milestone) = resolve(c, target)?;
    let ops: BTreeSet<&str> = operations.iter().map(|s| s.as_str()).collect();
    let all: Vec<&Clause> = c.clauses().collect();
    let by_id: BTreeMap<&str, &Clause> = all.iter().map(|cl| (cl.id.as_str(), *cl)).collect();
    let own: Vec<&Clause> =
        all.iter().filter(|cl| ops.contains(format!("{}.{}", cl.contract, cl.operation).as_str())).copied().collect();

    let mut seen: BTreeSet<&str> = own.iter().map(|cl| cl.id.as_str()).collect();
    let mut queue: Vec<&Clause> = own.clone();
    let mut edges: BTreeSet<(String, String)> = BTreeSet::new();
    let mut pointed: Vec<&Clause> = Vec::new();
    while let Some(cl) = queue.pop() {
        for p in pointers(&cl.statement) {
            let Some(to) = by_id.get(p.as_str()) else { continue };
            edges.insert((cl.id.clone(), p.clone()));
            if seen.insert(to.id.as_str()) {
                pointed.push(to);
                queue.push(to);
            }
        }
    }
    pointed.sort_by_key(|cl| (cl.file.clone(), cl.line));

    let owned_ids: BTreeSet<&str> = own.iter().map(|cl| cl.id.as_str()).collect();
    let mut errors = Vec::new();
    let mut limits = Vec::new();
    for f in c.reg.fragments.values() {
        for (id, e) in &f.error {
            if owned_ids.contains(e.clause.as_str()) {
                errors.push(Owned { id: id.clone(), clause: e.clause.clone(), gloss: e.gloss.clone(), value: None, unit: None });
            }
        }
        for (id, l) in &f.limit {
            if owned_ids.contains(l.clause.as_str()) {
                limits.push(Owned {
                    id: id.clone(),
                    clause: l.clause.clone(),
                    gloss: l.gloss.clone(),
                    value: Some(l.value_text()),
                    unit: Some(l.unit.clone()),
                });
            }
        }
    }

    let cited: BTreeSet<String> = own.iter().flat_map(|cl| checks::why_ok(&cl.why).unwrap_or_default()).collect();
    let records: Vec<Record> = c
        .records()
        .filter_map(|d| {
            let id = checks::record_id(&d.rel)?;
            cited.contains(&id).then(|| Record { id, file: d.rel.clone(), text: d.lines.join("\n") })
        })
        .collect();

    let all_ledes = c.ledes();
    let ledes = operations.iter().filter_map(|o| all_ledes.get(o).map(|t| (o.clone(), t.to_string()))).collect();
    let summary = Summary { clauses: own.len(), pointed: pointed.len(), pointers: edges.len(), records: records.len() };
    Ok(Slice {
        target: target.to_string(),
        operations,
        ledes,
        milestone,
        clauses: own.into_iter().map(row).collect(),
        pointed: pointed.into_iter().map(row).collect(),
        errors,
        limits,
        records,
        summary,
    })
}

fn table(s: &mut String, rows: &[Row]) {
    s.push_str("| Clause | Statement | Why | Home |\n| --- | --- | --- | --- |\n");
    for r in rows {
        let _ = writeln!(s, "| `{}` | {} | {} | {}:{} |", r.id, r.statement, r.why, r.file, r.line);
    }
    s.push('\n');
}

pub fn markdown(sl: &Slice) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# Slice `{}`\n", sl.target);
    if let Some(m) = &sl.milestone {
        let _ = writeln!(s, "## Milestone {}\n", m.heading);
        if let Some(r) = &m.reach {
            let _ = writeln!(s, "Reach: {r}\n");
        }
        if let Some(a) = &m.acceptance {
            let _ = writeln!(s, "Acceptance: `{a}`\n");
        }
    }
    s.push_str("## Operations\n\n");
    for o in &sl.operations {
        match sl.ledes.get(o) {
            Some(t) => {
                let _ = writeln!(s, "- `{o}` — {t}");
            }
            None => {
                let _ = writeln!(s, "- `{o}`");
            }
        }
    }
    s.push('\n');
    s.push_str("## Clauses\n\n");
    table(&mut s, &sl.clauses);
    if !sl.pointed.is_empty() {
        s.push_str("## Pointed-to clauses\n\n");
        table(&mut s, &sl.pointed);
    }
    if !sl.errors.is_empty() {
        s.push_str("## Errors\n\n| Error | Clause | Gloss |\n| --- | --- | --- |\n");
        for e in &sl.errors {
            let _ = writeln!(s, "| `{}` | `{}` | {} |", e.id, e.clause, e.gloss);
        }
        s.push('\n');
    }
    if !sl.limits.is_empty() {
        s.push_str("## Bounds\n\n| Bound | Value | Unit | Clause | Gloss |\n| --- | --- | --- | --- | --- |\n");
        for l in &sl.limits {
            let _ = writeln!(
                s,
                "| `{}` | {} | {} | `{}` | {} |",
                l.id,
                l.value.as_deref().unwrap_or(""),
                l.unit.as_deref().unwrap_or(""),
                l.clause,
                l.gloss
            );
        }
        s.push('\n');
    }
    if !sl.records.is_empty() {
        s.push_str("## Records\n\n");
        for r in &sl.records {
            let _ = writeln!(s, "<!-- {} · {} -->\n", r.id, r.file);
            s.push_str(r.text.trim_end());
            s.push_str("\n\n");
        }
    }
    let m = &sl.summary;
    let _ = writeln!(
        s,
        "---\n{} clauses · {} pointed-to clauses · {} pointers followed · {} records",
        m.clauses, m.pointed, m.pointers, m.records
    );
    s
}
