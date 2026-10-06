#[path = "support.rs"]
mod support;
use support::{at, decl, Fixture};
use contextful_context::fold::fold;
use contextful_core::store::bound_time::Bounds;
use serde_json::json;
use std::time::Instant;

#[test]
#[ignore]
fn issue_88_single_row_baseline() {
    let n: usize = std::env::var("ISSUE88_N").unwrap().parse().unwrap();
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    let started = Instant::now();
    for i in 0..n {
        f.land(&d, &format!("run-{i}"), json!([{ "id": i as i64 }]), "2030-01-01T00:00:00Z").unwrap();
    }
    let landed = started.elapsed();
    let started = Instant::now();
    let rows = f.query(&d, Bounds::default(), "SELECT count(*) FROM t");
    let read = started.elapsed();
    assert_eq!(rows, [[Some(n.to_string())]]);
    let started = Instant::now();
    let outcome = fold(&f.store, &d, at("2030-01-02T00:00:00Z")).unwrap();
    let folded = started.elapsed();
    println!("issue88 n={n} land_ms={} read_ms={} fold_ms={}", landed.as_secs_f64() * 1e3, read.as_secs_f64() * 1e3, folded.as_secs_f64() * 1e3);
    println!("issue88 outcome={outcome:?}");
}
