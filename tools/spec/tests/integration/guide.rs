//! The guide rules: operation pointers, the per-operation length cap, operation reach,
//! pointer density and restated bounds.

use crate::{codes, Scratch};

const TOPOLOGY: &str = "spec/guide/topology.md";

// spec: corpus.guide.non-normative@ff2e9531
#[test]
fn a_guide_operation_pointer_resolves_against_the_fragment_and_a_contract_file_one_dangles() {
    let s = Scratch::copy();
    let text = s.read(TOPOLOGY);
    s.write(TOPOLOGY, &format!("{text}\nA deployment starts from {{{{topology.package}}}} and names no {{{{topology.no-such-operation}}}}.\n"));
    let found = codes(&s.lint("reference"), "SpecDanglingReference");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("topology.no-such-operation"), "{found:?}");
    let contract = s.read("spec/01-topology.md");
    s.write("spec/01-topology.md", &format!("{contract}\nThe profile follows {{{{topology.package}}}}.\n"));
    let found = codes(&s.lint("reference"), "SpecDanglingReference");
    assert!(found.iter().any(|m| m.contains("{{topology.package}}")), "{found:?}");
}

// spec: corpus.guide.file@52864563
#[test]
fn a_guide_cap_grows_by_twenty_words_per_registered_operation() {
    let s = Scratch::copy();
    let text = s.read(TOPOLOGY);
    s.write(TOPOLOGY, &format!("{text}\n{}\n", "word ".repeat(560)));
    let found = codes(&s.lint("guide"), "SpecGuide");
    assert!(found.iter().any(|m| m.contains("exceed 520")), "{found:?}");
}

// spec: corpus.guide.reach@dfdcbfbe
#[test]
fn an_operation_the_guide_never_points_at_is_a_reach_finding() {
    let s = Scratch::copy();
    s.write(TOPOLOGY, "---\ncontract: topology\n---\n\n# System topology\n\nAn operator picks a build profile for each process ({{topology.package}}).\n");
    let found: Vec<String> = codes(&s.lint("guide"), "SpecGuideReach").into_iter().filter(|m| m.contains("`topology.")).collect();
    assert_eq!(found.len(), 5, "{found:?}");
    assert!(found.iter().any(|m| m.contains("`topology.compose`")), "{found:?}");
    assert!(!found.iter().any(|m| m.contains("`topology.package`")), "{found:?}");
}

// spec: corpus.guide.pointer-density@a2b31a2f
#[test]
fn a_paragraph_of_pointers_is_a_density_finding() {
    let s = Scratch::copy();
    let text = s.read(TOPOLOGY);
    let crowded = "One workspace builds every component, and the components share three contracts, each versioned and tested for conformance before a release reaches an operator";
    s.write(
        TOPOLOGY,
        &format!(
            "{text}\n{crowded} ({{{{topology.compose.workspace}}}}, {{{{topology.compose.run-path}}}}, {{{{topology.compose.read-path}}}}, {{{{topology.compose.three-crossings}}}}).\n\nSee {{{{topology.compose.workspace}}}} and {{{{topology.compose.run-path}}}} for both paths.\n"
        ),
    );
    let found: Vec<String> = codes(&s.lint("guide"), "SpecGuidePointerDensity").into_iter().filter(|m| m.contains("`topology`")).collect();
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found.iter().any(|m| m.contains("4 clause pointers")), "{found:?}");
    assert!(found.iter().any(|m| m.contains("words of prose for 2 clause pointers")), "{found:?}");
}

// spec: corpus.guide.bound@a5a8779f
#[test]
fn a_guide_number_equal_to_a_bound_carries_its_pointer() {
    let s = Scratch::copy();
    let text = s.read(TOPOLOGY);
    s.write(TOPOLOGY, &format!("{text}\nA cadence lease lives 90 s before another reconciler takes the cadence over.\n"));
    let found = codes(&s.lint("guide"), "SpecGuideBound");
    assert!(found.iter().any(|m| m.contains("`90`") && m.contains("topology.coordinate.cadence-lease-ttl")), "{found:?}");
    s.write(
        TOPOLOGY,
        &format!("{text}\nA cadence lease lives 90 s ({{{{topology.coordinate.cadence-lease-ttl}}}}) before another reconciler takes the cadence over.\n"),
    );
    let found = codes(&s.lint("guide"), "SpecGuideBound");
    assert!(!found.iter().any(|m| m.contains("`90`")), "{found:?}");
}
