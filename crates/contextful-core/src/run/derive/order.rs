//! `run.select.derive-order` and `run.select.derive-cycle`: which derive pipelines read
//! another derive pipeline's output, and the refusal of a set whose reads chain back to
//! their own output. Ordering is pure over the pipeline set; the scheduler holds a child
//! until each parent's fire ends.

use crate::pipeline::declare::PipelineSpec;
use crate::run::RunError;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The source name a derive pipeline declares.
pub const DERIVE_SOURCE: &str = "derive";

/// Each derive pipeline's derive parents, by id: the derive pipelines among `specs` whose
/// destination table its `source_table` names. A derive pipeline reading no derive output
/// maps to an empty set; a pipeline of another source has no entry. Reads chaining back to
/// a pipeline, one naming its own output included, raise `DeriveCycle`, naming every
/// pipeline on the cycle (`run.select.derive-cycle`).
pub fn derive_parents<'a>(specs: impl IntoIterator<Item = &'a PipelineSpec>) -> Result<BTreeMap<String, BTreeSet<String>>, RunError> {
    let derives: Vec<&PipelineSpec> = specs.into_iter().filter(|s| s.source.name == DERIVE_SOURCE).collect();
    let mut owner: BTreeMap<String, &str> = BTreeMap::new();
    for s in &derives {
        for t in &s.tables {
            owner.insert(s.table_name(t.name()), &s.id);
        }
    }
    let mut parents: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for s in &derives {
        let read = s.source.config.get("source_table").and_then(Value::as_str).map(str::trim);
        let set = read.and_then(|t| owner.get(t)).map(|p| BTreeSet::from([p.to_string()])).unwrap_or_default();
        parents.insert(s.id.clone(), set);
    }
    if let Some(cycle) = find_cycle(&parents) {
        let mut path: Vec<String> = cycle.iter().map(|id| format!("`{id}`")).collect();
        path.push(format!("`{}`", cycle[0]));
        return Err(RunError::DeriveCycle(format!(
            "derive pipelines read their own output through a cycle, {}, each reading the output of the next; a tick has no parent to run first",
            path.join(" -> ")
        )));
    }
    Ok(parents)
}

/// The first cycle of `parents` by id, each pipeline followed by the parent it reads.
fn find_cycle(parents: &BTreeMap<String, BTreeSet<String>>) -> Option<Vec<String>> {
    let mut done: BTreeSet<&str> = BTreeSet::new();
    for start in parents.keys() {
        let mut path: Vec<&str> = Vec::new();
        if let Some(cycle) = visit(start, parents, &mut done, &mut path) {
            return Some(cycle);
        }
    }
    None
}

fn visit<'a>(id: &'a str, parents: &'a BTreeMap<String, BTreeSet<String>>, done: &mut BTreeSet<&'a str>, path: &mut Vec<&'a str>) -> Option<Vec<String>> {
    if done.contains(id) {
        return None;
    }
    if let Some(at) = path.iter().position(|p| *p == id) {
        return Some(path[at..].iter().map(|p| p.to_string()).collect());
    }
    path.push(id);
    for p in parents.get(id).into_iter().flatten() {
        if let Some(cycle) = visit(p, parents, done, path) {
            return Some(cycle);
        }
    }
    path.pop();
    done.insert(id);
    None
}
