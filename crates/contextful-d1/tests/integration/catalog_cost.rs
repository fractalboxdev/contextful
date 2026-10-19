//! The D1 requests one received landing issues: the catalog calls the engine makes for a
//! landing, recorded by the engine suite, replayed against `D1Catalog`.

use crate::Fixture;
use contextful_core::coordinate::{Catalog, CursorRow, ExecutionAdmission, ExecutionExpectation, Lease, LeaseKey};
use contextful_core::run::backfill::ChunkRow;
use contextful_core::run::own::{ExecutionOwner, OwnerPins, OwnerScope};
use contextful_core::run::record::{RunRow, Window};
use contextful_d1::{D1Catalog, D1};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::BTreeMap;

/// The engine's catalog calls for one warm-up landing and one measured landing.
const TRACE: &str = include_str!("../../../contextful-engine/tests/fixtures/catalog/receive-landing.json");

fn arg<T: DeserializeOwned>(args: &Value, name: &str) -> T {
    serde_json::from_value(args[name].clone()).unwrap_or_else(|e| panic!("trace argument `{name}` of {args}: {e}"))
}

fn lease_key(v: &Value) -> LeaseKey {
    let text = |k: &str| v[k].as_str().map(str::to_string);
    if let Some(p) = text("pipeline") {
        LeaseKey::Pipeline(p)
    } else if let Some(t) = text("compaction") {
        LeaseKey::Compaction(t)
    } else if let Some(d) = text("cadence") {
        LeaseKey::Cadence(d)
    } else {
        LeaseKey::Partition { pipeline: v["partition"][0].as_str().unwrap().into(), partition: v["partition"][1].as_str().unwrap().into() }
    }
}

/// Replays a trace's calls in order, standing each traced lease in for the one this catalog granted under its key.
struct Replay<'a> {
    catalog: &'a D1Catalog,
    leases: BTreeMap<String, Lease>,
}

impl Replay<'_> {
    fn lease(&self, traced: &Value) -> Option<Lease> {
        let traced: Option<Lease> = serde_json::from_value(traced.clone()).unwrap();
        traced.map(|l| self.leases.get(&l.key).cloned().unwrap_or_else(|| panic!("the trace names lease `{}` before acquiring it", l.key)))
    }

    fn call(&mut self, call: &Value) {
        let (op, a) = (call["op"].as_str().unwrap(), &call["args"]);
        let c = self.catalog;
        let scope = || arg::<OwnerScope>(a, "scope");
        let text = |name: &str| a[name].as_str().unwrap().to_string();
        let ok = |r: Result<(), contextful_core::run::Failure>| r.unwrap_or_else(|e| panic!("replaying `{op}`: {e}"));
        match op {
            "supports_strict_execution_guards" => drop(c.supports_strict_execution_guards()),
            "now" => ok(c.now().map(drop)),
            "acquire" => {
                let lease = c.acquire(&lease_key(&a["key"]), &text("holder"), a["ttl_secs"].as_u64().unwrap()).unwrap().expect("the replayed acquisition holds");
                self.leases.insert(lease.key.clone(), lease);
            }
            "release" => {
                let lease = self.lease(&a["lease"]).unwrap();
                ok(c.release(&lease));
                self.leases.remove(&lease.key);
            }
            "renew" => {
                let lease = self.lease(&a["lease"]).unwrap();
                if let Some(renewed) = c.renew(&lease, a["ttl_secs"].as_u64().unwrap()).unwrap() {
                    self.leases.insert(renewed.key.clone(), renewed);
                }
            }
            "lease_holds" => ok(c.lease_holds(&self.lease(&a["lease"]).unwrap()).map(drop)),
            "lease_row" => ok(c.lease_row(&lease_key(&a["key"])).map(drop)),
            "cursor_at" => ok(c.cursor_at(&scope()).map(drop)),
            "cursor_cas_at" => ok(c.cursor_cas_at(&scope(), a["expected_version"].as_u64().unwrap(), arg::<CursorRow>(a, "next"), self.lease(&a["fence"]).as_ref()).map(drop)),
            "cursor" => ok(c.cursor(&text("pipeline_id"), &text("table")).map(drop)),
            "cursor_cas" => ok(c.cursor_cas(&text("pipeline_id"), &text("table"), a["expected_version"].as_u64().unwrap(), arg::<CursorRow>(a, "next"), self.lease(&a["fence"]).as_ref()).map(drop)),
            "owner_at" => ok(c.owner_at(&scope()).map(drop)),
            "owner" => ok(c.owner(&text("pipeline_id"), &text("table")).map(drop)),
            "put_owner" => ok(c.put_owner(&arg::<ExecutionOwner>(a, "owner"))),
            "retire_at" => ok(c.retire_at(&scope(), &text("execution_id"), arg::<Option<(CursorRow, u64)>>(a, "cursor"), self.lease(&a["fence"]).as_ref()).map(drop)),
            "retire" => ok(c.retire(&text("pipeline_id"), &text("table"), &text("execution_id"), arg::<Option<(CursorRow, u64)>>(a, "cursor"), self.lease(&a["fence"]).as_ref()).map(drop)),
            "retired_at" => ok(c.retired_at(&scope()).map(drop)),
            "chunk_at" => ok(c.chunk_at(&scope()).map(drop)),
            "chunks" => ok(c.chunks(&text("pipeline_id"), &text("table")).map(drop)),
            "put_chunk" => ok(c.put_chunk(&arg::<ChunkRow>(a, "row"))),
            "delete_chunks" => ok(c.delete_chunks(&text("pipeline_id"), &text("table")).map(drop)),
            "put_run" => ok(c.put_run(&arg::<RunRow>(a, "row"))),
            "run" => ok(c.run(&text("run_id")).map(drop)),
            "runs" => ok(c.runs(a["pipeline_id"].as_str()).map(drop)),
            "history" => ok(c.history(a["pipeline_id"].as_str(), &arg::<Window>(a, "window")).map(drop)),
            "last_run_start" => ok(c.last_run_start(&text("pipeline_id")).map(drop)),
            "update_run" => {
                let after: RunRow = arg(a, "after");
                let out = c.update_run(&text("run_id"), &mut |row| {
                    *row = after.clone();
                    Ok(())
                });
                assert!(matches!(out, Ok(Some(Ok(_)))), "replaying `update_run`: {out:?}");
            }
            "execution_guard" => {
                let owners = a["owners"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|o| ExecutionExpectation { scope: arg(o, "scope"), execution_id: arg(o, "execution_id"), pins: arg::<OwnerPins>(o, "pins"), run_id: arg(o, "run_id") })
                    .collect();
                let leases = a["leases"].as_array().unwrap().iter().map(|l| self.lease(l).unwrap()).collect();
                let admission = ExecutionAdmission::new(owners, leases, a["publication"].as_bool().unwrap()).unwrap();
                // The guard holds through the engine's callback; the replay releases it at once.
                drop(c.execution_guard(&admission).unwrap_or_else(|e| panic!("replaying `execution_guard`: {e}")));
            }
            other => panic!("the replay knows no catalog call `{other}`"),
        }
    }
}

/// The requests the fixture served, `GET /stats` excluded.
fn served(endpoint: &str) -> u64 {
    let text = ureq::get(format!("{endpoint}/stats")).call().unwrap().body_mut().read_to_string().unwrap();
    let stats: Value = serde_json::from_str(&text).unwrap();
    stats["total"].as_u64().unwrap()
}

/// One warm-up landing, then the measured one, replayed against D1: the figure is the measured landing's requests.
#[test]
fn a_received_landing_issues_its_d1_requests() {
    let trace: Value = serde_json::from_str(TRACE).unwrap();
    let fixture = Fixture::start();
    D1::connect(&fixture.endpoint).unwrap().initialize_recovery("sample", "generation").unwrap();
    let catalog = D1Catalog::connect(&fixture.endpoint).unwrap();
    let mut replay = Replay { catalog: &catalog, leases: BTreeMap::new() };
    for call in trace["warmup"].as_array().unwrap() {
        replay.call(call);
    }
    let landing = trace["landing"].as_array().unwrap();
    let before = served(&fixture.endpoint);
    for call in landing {
        replay.call(call);
    }
    let requests = served(&fixture.endpoint) - before;
    assert!(requests >= 1, "a landing reaches D1");
    contextful_eval::record::emit("catalog-d1-requests-per-landing", requests as f64, 1, 0);
}
