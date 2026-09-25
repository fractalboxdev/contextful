//! The readable form: subject-anchored clause items under a lede, one guide per
//! contract, and a generated card per contract.

use crate::{codes, Scratch};

const STORE: &str = "spec/10-store.md";
const GUIDE: &str = "spec/guide/store.md";

fn lock(s: &Scratch) -> serde_json::Value {
    assert!(s.cmd(&["extract"]).status.success());
    serde_json::from_str(&s.read("spec/spec.lock.json")).unwrap()
}

// spec: corpus.address.clause-id@8719f092
#[test]
fn a_clause_item_takes_its_contract_and_operation_from_its_file_and_section() {
    let s = Scratch::copy();
    let lock = lock(&s);
    let clause = lock["clauses"].as_array().unwrap().iter().find(|c| c["id"] == "store.lay-out.components").expect("the clause");
    assert_eq!(clause["subject"], "components");
    assert!(clause["statement"].as_str().unwrap().starts_with("A store holds Parquet table data"));
    assert!(clause["why"].as_str().unwrap().starts_with("because a catalog"));
}

// spec: corpus.anatomy.lede@81cd69c5
#[test]
fn the_lede_lands_in_the_lock_and_its_absence_is_an_anatomy_finding() {
    let s = Scratch::copy();
    let lede = lock(&s)["ledes"]["store.fold"].as_str().expect("a fold lede").to_string();
    assert!(lede.starts_with("Compaction"), "{lede}");
    let body = s.read(STORE);
    s.write(STORE, &body.replacen(&format!("## fold\n\n{lede}\n"), "## fold\n", 1));
    let found = codes(&s.lint("anatomy"), "SpecAnatomy");
    assert!(found.iter().any(|m| m.contains("`fold`") && m.contains("lede")), "{found:?}");
}

// spec: corpus.anatomy.clause-list@8574bc7c
#[test]
fn a_clause_list_split_by_prose_is_an_anatomy_finding() {
    let s = Scratch::copy();
    let body = s.read(STORE);
    let item = body.lines().find(|l| l.starts_with("- `includes-runs` — ")).unwrap().to_string();
    s.write(STORE, &body.replacen(&item, &format!("{item}\n\nA sentence in the middle.\n"), 1));
    let found = codes(&s.lint("anatomy"), "SpecAnatomy");
    assert!(found.iter().any(|m| m.contains("`fold`") && m.contains("broken")), "{found:?}");
}

// spec: corpus.anatomy.bad-anatomy@443124ed
#[test]
fn an_item_outside_the_clause_shape_is_an_anatomy_finding() {
    let s = Scratch::copy();
    let body = s.read(STORE);
    let item = body.lines().find(|l| l.starts_with("- `includes-runs` — ")).unwrap().to_string();
    s.write(STORE, &body.replacen(&item, &item.replacen(" — ", ": ", 1), 1));
    let found = codes(&s.lint("anatomy"), "SpecAnatomy");
    assert!(found.iter().any(|m| m.contains("clause item")), "{found:?}");
}

#[test]
fn an_operation_gloss_in_the_fragment_is_a_registry_finding() {
    let s = Scratch::copy();
    let frag = s.read("spec/terms/store.toml");
    s.write("spec/terms/store.toml", &frag.replacen("fold       = {}", "fold       = { gloss = \"Compaction.\" }", 1));
    let found = codes(&s.lint("registry"), "SpecRegistry");
    assert!(found.iter().any(|m| m.contains("`fold`") && m.contains("gloss")), "{found:?}");
}

// spec: corpus.guide.bad-guide@d6d374ef
#[test]
fn a_contract_without_a_guide_is_a_guide_finding() {
    let s = Scratch::copy();
    std::fs::remove_file(s.root.join(GUIDE)).unwrap();
    let found = codes(&s.lint("guide"), "SpecGuide");
    assert!(found.iter().any(|m| m.contains("`store` has no guide")), "{found:?}");
}

// spec: corpus.guide.non-normative@3597e673
#[test]
fn a_guide_naming_an_error_or_holding_a_clause_item_is_a_guide_finding() {
    let s = Scratch::copy();
    let text = s.read(GUIDE);
    s.write(GUIDE, &format!("{text}\nA torn commit raises StorePartialSnapshot.\n\n- `extra` — A guide states a rule.\n"));
    let found = codes(&s.lint("guide"), "SpecGuide");
    assert!(found.iter().any(|m| m.contains("StorePartialSnapshot")), "{found:?}");
    assert!(found.iter().any(|m| m.contains("clause item")), "{found:?}");
}

// spec: corpus.guide.file@e5fe69c4
#[test]
fn a_guide_over_its_length_or_off_its_title_is_a_guide_finding() {
    let s = Scratch::copy();
    let text = s.read(GUIDE).replacen("# The store and its sync", "# The store", 1);
    s.write(GUIDE, &format!("{text}\n{}\n", "word ".repeat(700)));
    let found = codes(&s.lint("guide"), "SpecGuide");
    assert!(found.iter().any(|m| m.contains("exceed 700")), "{found:?}");
    assert!(found.iter().any(|m| m.contains("differs from registry")), "{found:?}");
}

#[test]
fn a_guide_pointer_naming_no_clause_dangles() {
    let s = Scratch::copy();
    let text = s.read(GUIDE);
    s.write(GUIDE, &format!("{text}\nSee {{{{store.fold.no-such-clause}}}}.\n"));
    assert_eq!(codes(&s.lint("reference"), "SpecDanglingReference").len(), 1);
}

#[test]
fn a_guide_in_the_future_tense_is_counterfactual() {
    let s = Scratch::copy();
    let text = s.read(GUIDE);
    s.write(GUIDE, &format!("{text}\nA fold will publish a snapshot.\n"));
    assert_eq!(codes(&s.lint("render"), "SpecCounterfactual").len(), 1);
}

// spec: corpus.render.card@8fc19676
#[test]
fn state_writes_a_card_per_contract_and_a_stale_card_is_a_render_finding() {
    let s = Scratch::copy();
    assert!(s.cmd(&["state"]).status.success());
    assert!(s.cmd(&["extract"]).status.success());
    let card = s.read("spec/cards/store.md");
    assert!(card.contains("| `fold` | Compaction"), "{card}");
    assert!(card.contains("| `StorePartialSnapshot` |") && card.contains("`store.fold.partial-snapshot`"), "{card}");
    assert!(codes(&s.lint("render"), "SpecStaleRender").is_empty());
    s.write("spec/cards/store.md", &format!("{card}\nedited\n"));
    let found = codes(&s.lint("render"), "SpecStaleRender");
    assert!(found.iter().any(|m| m.contains("spec/cards/store.md")), "{found:?}");
}

#[test]
fn an_unbackticked_axiom_is_a_banned_word_and_a_lean_identifier_is_not() {
    let s = Scratch::copy();
    let text = s.read(GUIDE);
    s.write(GUIDE, &format!("{text}\nThe audit reads `#print axioms` for each constant.\n"));
    assert!(codes(&s.lint("render"), "SpecBannedWord").is_empty());
    s.write(GUIDE, &format!("{text}\nThe audit lists every axiom a constant reaches.\n"));
    assert_eq!(codes(&s.lint("render"), "SpecBannedWord").len(), 1);
}

// spec: corpus.render.diagram-boundary@e86f00e0
#[test]
fn a_flowchart_node_standing_for_a_contract_or_boundary_is_a_diagram_finding() {
    let s = Scratch::copy();
    let text = s.read(GUIDE);
    let container = "```mermaid\nflowchart LR\n  subgraph READ[\"read contract\"]\n    Q[\"query face\"]\n  end\n  F[\"fold\"] --> Q\n```\n";
    s.write(GUIDE, &format!("{text}\n{container}"));
    assert!(codes(&s.lint("render"), "SpecDiagramBoundary").is_empty());
    let squeezed = "```mermaid\nflowchart LR\n  F[\"fold\"] -- \"read contract\" --> Q[\"read contract:<br/>query face\"]\n  Q --> T{\"trust boundary\"}\n```\n";
    s.write(GUIDE, &format!("{text}\n{squeezed}"));
    assert_eq!(codes(&s.lint("render"), "SpecDiagramBoundary").len(), 2);
}

// spec: corpus.render.diagram-node@b820d37c
#[test]
fn a_flowchart_node_bundling_attributes_or_naming_an_error_is_a_diagram_finding() {
    let s = Scratch::copy();
    let text = s.read(GUIDE);
    let clean = "```mermaid\nflowchart LR\n  subgraph ENGINE[\"engine · store\"]\n    F[\"fold the table\"] -->|\"StoreStaleFence · 409\"| R([refused])\n    F -- \"8 hops, 10000 nodes\" --> Q{clears the floor?}\n  end\n  Q --> C[(catalog)]\n```\n";
    s.write(GUIDE, &format!("{text}\n{clean}"));
    let found = codes(&s.lint("render"), "SpecDiagramNode");
    for label in ["fold the table", "refused", "clears the floor?", "catalog", "engine · store", "8 hops", "409"] {
        assert!(!found.iter().any(|m| m.contains(&format!("`{label}`"))), "`{label}` is no bundled node: {found:?}");
    }
    let bundled = "```mermaid\nflowchart LR\n  D[\"engine · scheduler\"] --> B[\"fold<br/>compact\"]\n  B --> W[write the snapshot and every declared sidecar]\n  W --> E([StorePartialSnapshot])\n  E --> OK((done))\n```\n";
    s.write(GUIDE, &format!("{text}\n{bundled}"));
    let found = codes(&s.lint("render"), "SpecDiagramNode");
    for label in ["engine · scheduler", "fold<br/>compact", "write the snapshot and every declared sidecar", "StorePartialSnapshot"] {
        assert!(found.iter().any(|m| m.contains(label)), "no finding for `{label}`: {found:?}");
    }
    assert!(!found.iter().any(|m| m.contains("`done`")), "{found:?}");
}
