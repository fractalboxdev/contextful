//! `contextful-ci deploy parity`: `topology.deploy.parity-divergence`'s byte comparison of
//! each target's table parts against the reference target's.

use crate::{stderr, Repo};
use std::process::Output;

/// A store root holding a flat table and a nested one, each with two runs whose parts share
/// a file name, as the reference target writes it.
fn reference(r: &Repo, dir: &str) {
    for table in ["events", "users", "ns/events"] {
        for run in ["r1", "r2"] {
            r.write(&format!("{dir}/tables/{table}/data/runs/{run}/n1/part-00000.parquet"), &format!("PAR1 {table} {run} PAR1"));
        }
    }
}

fn copy(r: &Repo, from: &str, to: &str) {
    for entry in walk(&r.root.join(from)) {
        let rel = entry.strip_prefix(r.root.join(from)).unwrap().to_string_lossy().to_string();
        r.write(&format!("{to}/{rel}"), &std::fs::read_to_string(&entry).unwrap());
    }
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() { out.extend(walk(&p)) } else { out.push(p) }
    }
    out
}

fn flip_byte(r: &Repo, rel: &str) {
    let p = r.root.join(rel);
    let mut bytes = std::fs::read(&p).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0x01;
    std::fs::write(p, bytes).unwrap();
}

fn parity(r: &Repo, targets: &[&str]) -> Output {
    let mut args = vec!["deploy", "parity", "--reference", "ref"];
    for t in targets {
        args.extend(["--target", t]);
    }
    r.run_ci(&args)
}

// spec: topology.deploy.parity@d164f9be
/// A byte difference from the reference output raises `ParityDivergence`, naming the
/// target, the table and the first differing part by its path under `data/`.
// spec: topology.deploy.parity-divergence@f94f7052
#[test]
fn a_flipped_byte_names_the_target_the_table_and_the_first_differing_part() {
    let r = Repo::init();
    reference(&r, "ref");
    let same = parity(&r, &["local=ref"]);
    assert!(same.status.success(), "{}", stderr(&same));
    let out = String::from_utf8_lossy(&same.stdout).to_string();
    assert!(out.contains("local") && out.contains("6 parts") && out.contains("0 divergent"), "{out}");

    copy(&r, "ref", "aws");
    copy(&r, "ref", "cloudflare");
    flip_byte(&r, "aws/tables/users/data/runs/r2/n1/part-00000.parquet");
    let o = parity(&r, &["cloudflare=cloudflare", "aws=aws"]);
    assert!(!o.status.success());
    let err = stderr(&o);
    assert!(err.contains("ParityDivergence"), "{err}");
    assert!(err.contains("`aws`") && err.contains("`users`") && err.contains("`runs/r2/n1/part-00000.parquet`"), "{err}");
    assert!(!err.contains("runs/r1"), "the part is named by its full path: {err}");
    assert!(!err.contains("`cloudflare`"), "{err}");
}

/// A table whose name holds `/` is compared as any other, and its divergence names the
/// nested table.
#[test]
fn a_nested_table_divergence_names_the_nested_table() {
    let r = Repo::init();
    reference(&r, "ref");
    copy(&r, "ref", "aws");
    flip_byte(&r, "aws/tables/ns/events/data/runs/r1/n1/part-00000.parquet");
    let o = parity(&r, &["aws=aws"]);
    assert!(!o.status.success(), "a nested table's flipped byte diverges");
    let err = stderr(&o);
    assert!(err.contains("ParityDivergence") && err.contains("`ns/events`") && err.contains("`runs/r1/n1/part-00000.parquet`"), "{err}");

    r.write("nested/tables/_visibility/src/data/runs/r1/n1/part-00000.parquet", "PAR1 vis PAR1");
    let o = r.run_ci(&["deploy", "parity", "--reference", "nested", "--target", "self=nested"]);
    assert!(o.status.success(), "a reference holding only nested tables is compared: {}", stderr(&o));
}

/// A part the target lacks or adds diverges as a differing byte does, and a reference
/// holding no part is refused, so a passing comparison has compared at least one part.
#[test]
fn a_missing_or_extra_part_diverges_and_an_empty_reference_is_refused() {
    let r = Repo::init();
    reference(&r, "ref");
    copy(&r, "ref", "gone");
    std::fs::remove_file(r.root.join("gone/tables/events/data/runs/r2/n1/part-00000.parquet")).unwrap();
    let o = parity(&r, &["gone=gone"]);
    assert!(!o.status.success());
    let err = stderr(&o);
    assert!(err.contains("ParityDivergence") && err.contains("`events`") && err.contains("`runs/r2/n1/part-00000.parquet`") && err.contains("absent"), "{err}");

    copy(&r, "ref", "extra");
    r.write("extra/tables/users/data/runs/r3/n1/part-00000.parquet", "PAR1 stray PAR1");
    let err = stderr(&parity(&r, &["extra=extra"]));
    assert!(err.contains("ParityDivergence") && err.contains("`users`") && err.contains("`runs/r3/n1/part-00000.parquet`"), "{err}");

    r.write("empty/README", "no parts\n");
    let o = r.run_ci(&["deploy", "parity", "--reference", "empty", "--target", "local=ref"]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("no table part"), "{}", stderr(&o));
    let o = r.run_ci(&["deploy", "parity", "--reference", "ref"]);
    assert!(!o.status.success(), "a comparison with no target is refused");
}
