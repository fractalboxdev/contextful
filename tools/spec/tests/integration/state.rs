//! `corpus.state`: pin verdicts, the acceptance line, and acceptance before progress.

use crate::{codes, Scratch};

const LIMIT_CLAUSE: &str = "corpus.anatomy.statement-words";

#[test]
fn ignored_test_pin_is_broken() {
    let s = Scratch::copy();
    s.write("crates/x/src/lib.rs", "#[test]\n#[ignore = \"red\"]\nfn words_over_forty_refused() {}\n");
    s.pin(LIMIT_CLAUSE, "test", "x::words_over_forty_refused");
    let broken = codes(&s.lint("state"), "SpecBrokenPin");
    assert_eq!(broken.len(), 1, "{broken:?}");
    assert!(broken[0].contains("ignore"), "{broken:?}");
}

#[test]
fn running_test_pin_performs() {
    let s = Scratch::copy();
    s.write("crates/x/src/lib.rs", "#[test]\nfn words_over_forty_refused() {}\n");
    s.pin(LIMIT_CLAUSE, "test", "x::words_over_forty_refused");
    assert!(codes(&s.lint("state"), "SpecBrokenPin").is_empty());
}

#[test]
fn a_pin_resolves_under_tools() {
    let s = Scratch::copy();
    s.write("tools/y/tests/integration/words.rs", "#[test]\nfn words_over_forty_refused() {}\n");
    s.pin(LIMIT_CLAUSE, "test", "y::words::words_over_forty_refused");
    assert!(codes(&s.lint("state"), "SpecBrokenPin").is_empty());
}

#[test]
fn a_milestone_without_an_acceptance_line_is_a_roadmap_finding() {
    let s = Scratch::copy();
    let roadmap = s.read("spec/roadmap.md");
    let line = roadmap.lines().find(|l| l.starts_with("Acceptance: ")).expect("an acceptance line").to_string();
    s.write("spec/roadmap.md", &roadmap.replacen(&format!("{line}\n"), "", 1));
    let found = codes(&s.lint("state"), "SpecRoadmap");
    assert!(found.iter().any(|m| m.contains("Acceptance")), "{found:?}");
}

#[test]
fn the_live_roadmap_names_an_acceptance_test_per_milestone() {
    let s = Scratch::copy();
    assert!(codes(&s.lint("state"), "SpecRoadmap").is_empty());
}

#[test]
fn a_pin_in_a_milestone_with_no_acceptance_test_is_refused_until_one_exists() {
    let s = Scratch::copy();
    let clause = s.clause_of("store.lay-out");
    s.write("crates/store/src/lib.rs", "#[test]\nfn lays_out_a_table() {}\n");
    s.pin(&clause, "test", "store::lays_out_a_table");
    let missing = codes(&s.lint("state"), "SpecAcceptanceMissing");
    assert_eq!(missing.len(), 1, "{missing:?}");

    s.write(
        "crates/acceptance/tests/integration/m02.rs",
        "#[test]\n#[ignore = \"milestone open\"]\nfn m02_store() {}\n",
    );
    assert!(codes(&s.lint("state"), "SpecAcceptanceMissing").is_empty());
}

#[test]
fn status_reports_each_milestone_acceptance_verdict() {
    let s = Scratch::copy();
    s.write("crates/acceptance/tests/integration/m02.rs", "#[test]\n#[ignore]\nfn m02_store() {}\n");
    s.write("crates/acceptance/tests/integration/m03.rs", "#[test]\nfn m03_run_path() {}\n");
    assert!(s.cmd(&["state"]).status.success());
    let status = s.read("spec/status.md");
    let row = |m: &str| status.lines().find(|l| l.starts_with(&format!("| {m} "))).unwrap_or_default().to_string();
    assert!(row("2 — The store").ends_with("| open |"), "{status}");
    assert!(row("3 — The run path").ends_with("| passing |"), "{status}");
    assert!(row("4 — Ingest").ends_with("| absent |"), "{status}");
}
