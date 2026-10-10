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

/// The `| <heading> ` row of the `## Milestones` table in `spec/status.md`.
fn milestone_row(status: &str, heading: &str) -> String {
    status.lines().find(|l| l.starts_with(&format!("| {heading} "))).unwrap_or_default().to_string()
}

#[test]
fn status_reports_each_milestone_acceptance_verdict() {
    let s = Scratch::copy();
    s.write("crates/acceptance/tests/integration/m02.rs", "#[test]\n#[ignore]\nfn m02_store() {}\n");
    s.write("crates/acceptance/tests/integration/m03.rs", "#[test]\nfn m03_run_path() {}\n");
    s.write("crates/acceptance/tests/integration/m04.rs", "#[test]\nfn m04_ingest() {\n    todo!(\"ingest\")\n}\n");
    assert!(s.cmd(&["state"]).status.success());
    let status = s.read("spec/status.md");
    assert!(status.contains("| Performed | Acceptance | Closed |"), "{status}");
    assert!(milestone_row(&status, "2 — The store").ends_with("| open | open |"), "{status}");
    assert!(milestone_row(&status, "3 — The run path").ends_with("| passing | open |"), "{status}");
    assert!(milestone_row(&status, "4 — Ingest").ends_with("| open | open |"), "{status}");
    assert!(milestone_row(&status, "6 — Sync and replicas").ends_with("| absent | open |"), "{status}");
}

#[test]
fn a_milestone_closes_only_when_every_operation_it_names_holds_a_performed_clause() {
    let s = Scratch::copy();
    s.write("crates/acceptance/tests/integration/m07.rs", "#[test]\nfn m07_memory() {}\n");
    let ops = ["read.declare", "read.synthesize", "read.revise", "read.recall", "read.resolve-entity", "read.settle"];
    let mut lib = String::new();
    for (i, op) in ops.iter().enumerate() {
        lib.push_str(&format!("#[test]\nfn covers_{i}() {{}}\n"));
        if i + 1 < ops.len() {
            s.pin(&s.clause_of(op), "test", &format!("x::covers_{i}"));
        }
    }
    s.write("crates/x/src/lib.rs", &lib);
    assert!(s.cmd(&["state"]).status.success());
    let row = milestone_row(&s.read("spec/status.md"), "7 — Memory");
    assert!(row.ends_with("| passing | open |"), "`read.settle` holds no performed clause: {row}");

    s.pin(&s.clause_of("read.settle"), "test", "x::covers_5");
    assert!(s.cmd(&["state"]).status.success());
    let row = milestone_row(&s.read("spec/status.md"), "7 — Memory");
    assert!(row.ends_with("| passing | closed |"), "{row}");

    s.write("crates/acceptance/tests/integration/m07.rs", "#[test]\n#[ignore]\nfn m07_memory() {}\n");
    assert!(s.cmd(&["state"]).status.success());
    let row = milestone_row(&s.read("spec/status.md"), "7 — Memory");
    assert!(row.ends_with("| open | open |"), "{row}");
}

/// Insert `Depth: operation` after the `Reach:` line of the milestone headed `heading`.
fn mark_operation_depth(s: &Scratch, heading: &str) {
    let roadmap = s.read("spec/roadmap.md");
    let at = roadmap.find(&format!("## {heading}\n")).expect("the milestone heading");
    let reach = at + roadmap[at..].find("\nReach: ").expect("a reach line") + 1;
    let end = reach + roadmap[reach..].find('\n').unwrap() + 1;
    s.write("spec/roadmap.md", &format!("{}\nDepth: operation\n{}", &roadmap[..end], &roadmap[end..]));
}

#[test]
fn a_behavior_clause_in_an_operation_depth_milestone_is_refused() {
    let s = Scratch::copy();
    let lock: serde_json::Value = serde_json::from_str(&s.read("spec/spec.lock.json")).unwrap();
    let kind_of = |id: &str| -> String {
        lock["clauses"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .map(|c| c["kind"].as_str().unwrap().to_string())
            .unwrap_or_default()
    };
    let before = s.read("spec/roadmap.md");
    mark_operation_depth(&s, "2 — The store");
    let found = codes(&s.lint("state"), "SpecDeferredBehavior");
    assert!(!found.is_empty(), "a behavior clause of the store raises nothing");
    for m in &found {
        let id = m.split('`').nth(1).expect("a clause id in backticks");
        assert!(id.starts_with("store."), "{m}");
        assert_eq!(kind_of(id), "behavior", "{m}");
    }

    s.write("spec/roadmap.md", &before);
    assert!(codes(&s.lint("state"), "SpecDeferredBehavior").is_empty());
}

#[test]
fn the_live_roadmap_holds_no_deferred_behavior() {
    let s = Scratch::copy();
    assert!(codes(&s.lint("state"), "SpecDeferredBehavior").is_empty());
}

/// The verdict column of `clause`'s row in `spec/status.md`, if the row exists.
fn status_row(s: &Scratch, clause: &str) -> Option<String> {
    assert!(s.cmd(&["state"]).status.success());
    let status = s.read("spec/status.md");
    status.lines().find(|l| l.starts_with(&format!("| `{clause}` |"))).map(|row| row.trim_end_matches(" |").rsplit("| ").next().unwrap().to_string())
}

// spec: corpus.state.status-is-computed@093655f6
#[test]
fn status_is_regenerated_from_pins_alone() {
    let s = Scratch::copy();
    assert_eq!(status_row(&s, LIMIT_CLAUSE), None);
    s.write("crates/x/src/lib.rs", "#[test]\nfn words_over_forty_refused() {}\n");
    s.pin(LIMIT_CLAUSE, "test", "x::words_over_forty_refused");
    assert_eq!(status_row(&s, LIMIT_CLAUSE).as_deref(), Some("performed"));
    assert!(s.read("spec/status.md").starts_with("# Status\n\nGenerated by `contextful-spec state`"));
    s.write("crates/x/src/lib.rs", "#[test]\n#[ignore]\nfn words_over_forty_refused() {}\n");
    assert_eq!(status_row(&s, LIMIT_CLAUSE).as_deref(), Some("broken"));
}

// spec: corpus.state.pin@c5f33380
#[test]
fn a_pin_maps_a_clause_to_a_test_or_a_behavior_to_an_item() {
    let s = Scratch::copy();
    s.write("crates/x/src/lib.rs", "pub struct Lede;\n\n#[test]\nfn words_over_forty_refused() {}\n");
    s.pin(LIMIT_CLAUSE, "test", "x::words_over_forty_refused");
    s.pin("corpus.anatomy.lede", "item", "x::Lede");
    assert!(codes(&s.lint("state"), "SpecBrokenPin").is_empty(), "{:?}", s.lint("state"));
    assert_eq!(status_row(&s, LIMIT_CLAUSE).as_deref(), Some("performed"));
    assert_eq!(status_row(&s, "corpus.anatomy.lede").as_deref(), Some("performed"));
}

// spec: corpus.state.coverage-floor@15b71f16
#[test]
fn a_live_count_below_its_floor_is_a_coverage_regression() {
    let s = Scratch::copy();
    s.write("crates/x/src/lib.rs", "#[test]\nfn words_over_forty_refused() {}\n");
    s.pin(LIMIT_CLAUSE, "test", "x::words_over_forty_refused");
    let pins = s.read("spec/pins.toml");
    s.write("spec/pins.toml", &format!("{pins}corpus = 1\n"));
    assert!(codes(&s.lint("state"), "SpecCoverageRegression").is_empty());
    s.write("spec/pins.toml", &format!("{pins}corpus = 2\n"));
    let found = codes(&s.lint("state"), "SpecCoverageRegression");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("`corpus` holds 1 performed pins under floor 2"), "{found:?}");
}

// spec: corpus.state.bad-pin@18147168
#[test]
fn a_pin_naming_no_clause_an_item_on_a_limit_or_an_unresolved_test_is_a_broken_pin() {
    let s = Scratch::copy();
    assert!(codes(&s.lint("state"), "SpecBrokenPin").is_empty());
    s.write("crates/x/src/lib.rs", "pub struct Words;\n");
    s.pin("corpus.anatomy.no-such-clause", "test", "x::nothing");
    s.pin(LIMIT_CLAUSE, "item", "x::Words");
    s.pin("corpus.anatomy.file-length", "test", "x::absent_test");
    let found = codes(&s.lint("state"), "SpecBrokenPin");
    assert!(found.iter().any(|m| m.contains("pin `corpus.anatomy.no-such-clause` names no clause")), "{found:?}");
    assert!(found.iter().any(|m| m.contains("limit `corpus.anatomy.statement-words` takes a test or theorem, not an item")), "{found:?}");
    assert!(found.iter().any(|m| m.contains("pin for `corpus.anatomy.file-length` does not resolve")), "{found:?}");
    s.write("crates/x/tests/integration/stray.rs", "// spec: corpus.anatomy.lede@00000000\nconst STRAY: u8 = 0;\n");
    let found = codes(&s.lint("state"), "SpecBrokenPin");
    assert!(found.iter().any(|m| m.contains("tag for `corpus.anatomy.lede` sits above no function")), "{found:?}");
}
