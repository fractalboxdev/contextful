//! `contextful-spec slice`: the context pack for an operation, a contract or a milestone.

use crate::Scratch;
use std::collections::BTreeSet;

fn slice(s: &Scratch, target: &str) -> serde_json::Value {
    let out = s.cmd(&["slice", target, "--json"]);
    assert!(out.status.success(), "slice {target}: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

fn ids(v: &serde_json::Value, key: &str) -> BTreeSet<String> {
    v[key].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap().to_string()).collect()
}

fn operation(id: &str) -> String {
    id.split('.').take(2).collect::<Vec<_>>().join(".")
}

#[test]
fn an_operation_slice_carries_its_rows_and_a_row_another_contract_owns() {
    let s = Scratch::copy();
    let v = slice(&s, "store.bound-time");
    let rows = ids(&v, "clauses");
    assert!(rows.contains("store.bound-time.pin-bound"), "{rows:?}");
    assert!(rows.iter().all(|id| id.starts_with("store.bound-time.")), "{rows:?}");
    let pointed = ids(&v, "pointed");
    assert!(pointed.contains("read.resolve-pin.earlier-bound-wins"), "{pointed:?}");
    assert!(v["summary"]["pointers"].as_u64().unwrap() >= 1);
}

#[test]
fn an_operation_slice_carries_the_records_its_rows_cite_and_the_errors_they_own() {
    let s = Scratch::copy();
    let v = slice(&s, "store.reconcile");
    let records: BTreeSet<&str> = v["records"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert!(records.contains("A-store"), "{records:?}");
    let a_store = v["records"].as_array().unwrap().iter().find(|r| r["id"] == "A-store").unwrap();
    assert!(a_store["text"].as_str().unwrap().starts_with("# A-store"));
    let errors: BTreeSet<&str> = v["errors"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap()).collect();
    assert!(errors.contains("StoreSchemaIncompatible"), "{errors:?}");
    assert!(!errors.contains("StoreUnknownTable"), "{errors:?}");
}

#[test]
fn a_milestone_slice_carries_exactly_its_operations_plus_their_closure() {
    let s = Scratch::copy();
    let v = slice(&s, "2");
    let listed: BTreeSet<String> = ["lay-out", "declare", "reserve", "reconcile", "fold", "index", "bound-time", "encrypt"]
        .iter()
        .map(|o| format!("store.{o}"))
        .collect();
    let rows = ids(&v, "clauses");
    let ops: BTreeSet<String> = rows.iter().map(|id| operation(id)).collect();
    assert_eq!(ops, listed);

    let lock: serde_json::Value = serde_json::from_str(&s.read("spec/spec.lock.json")).unwrap();
    let known: BTreeSet<String> =
        lock["clauses"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap().to_string()).collect();
    let every_row: BTreeSet<String> = known.iter().filter(|id| listed.contains(&operation(id))).cloned().collect();
    assert_eq!(rows, every_row);
    let edges: Vec<(String, String)> = lock["pointers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p[0].as_str().unwrap().to_string(), p[1].as_str().unwrap().to_string()))
        .filter(|(_, to)| known.contains(to))
        .collect();
    let mut reach = rows.clone();
    loop {
        let next: Vec<String> = edges.iter().filter(|(a, b)| reach.contains(a) && !reach.contains(b)).map(|(_, b)| b.clone()).collect();
        if next.is_empty() {
            break;
        }
        reach.extend(next);
    }
    let closure: BTreeSet<String> = reach.difference(&rows).cloned().collect();
    assert!(!closure.is_empty());
    assert_eq!(ids(&v, "pointed"), closure);

    assert!(v["milestone"]["reach"].as_str().unwrap().starts_with("A table lands"));
    assert_eq!(v["milestone"]["acceptance"], "contextful_acceptance::m02::m02_store");
    assert_eq!(v["summary"]["clauses"].as_u64().unwrap() as usize, rows.len());
}

#[test]
fn a_contract_slice_carries_every_operation_of_the_contract() {
    let s = Scratch::copy();
    let v = slice(&s, "store.*");
    let ops: BTreeSet<String> = ids(&v, "clauses").iter().map(|id| operation(id)).collect();
    assert!(ops.contains("store.lease") && ops.contains("store.lay-out"), "{ops:?}");
    assert!(ops.iter().all(|o| o.starts_with("store.")), "{ops:?}");
}

#[test]
fn the_markdown_slice_ends_with_its_counts() {
    let s = Scratch::copy();
    let out = s.cmd(&["slice", "store.bound-time"]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("store.bound-time.pin-bound"), "{text}");
    let last = text.trim_end().lines().last().unwrap();
    assert!(last.contains("clauses") && last.contains("pointers followed") && last.contains("records"), "{last}");
}

#[test]
fn an_unknown_target_exits_non_zero_naming_it() {
    let s = Scratch::copy();
    for target in ["store.no-such-op", "nowhere.*", "99"] {
        let out = s.cmd(&["slice", target]);
        assert!(!out.status.success(), "{target} succeeded");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(target), "{target}: {err}");
    }
}
