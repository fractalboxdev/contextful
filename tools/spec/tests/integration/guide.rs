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
