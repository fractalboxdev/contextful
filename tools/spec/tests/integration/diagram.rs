//! `corpus.diagram`: the shape rules every flowchart, sequence and state diagram obeys.
//! Each test appends one diagram to a guide and reads the findings it adds.

use crate::Scratch;

const GUIDE: &str = "spec/guide/store.md";

/// The `code` findings a fenced block adds to the live corpus's `diagram` check.
fn added(block: &str, code: &str) -> Vec<String> {
    let s = Scratch::copy();
    let before = messages(&s, code);
    let text = s.read(GUIDE);
    s.write(GUIDE, &format!("{text}\n```mermaid\n{block}```\n"));
    let mut after = messages(&s, code);
    for m in before {
        if let Some(i) = after.iter().position(|a| *a == m) {
            after.remove(i);
        }
    }
    after
}

fn messages(s: &Scratch, code: &str) -> Vec<String> {
    s.lint("diagram").into_iter().filter(|(c, _)| c == code).map(|(_, m)| m).collect()
}

fn names(found: &[String], label: &str) -> bool {
    found.iter().any(|m| m.contains(&format!("`{label}`")))
}

// spec: corpus.diagram.boundary@e86f00e0
#[test]
fn a_flowchart_node_standing_for_a_contract_or_boundary_is_a_diagram_finding() {
    let container = "flowchart LR\n  subgraph READ[\"read contract\"]\n    Q[\"query face\"]\n  end\n  F([\"caller\"]) -->|\"query\"| Q\n";
    assert!(added(container, "SpecDiagramBoundary").is_empty());
    let squeezed = "flowchart LR\n  F([\"caller\"]) -- \"read contract\" --> Q[\"read contract\"]\n  Q -->|\"crosses\"| T[trust boundary]\n";
    assert_eq!(added(squeezed, "SpecDiagramBoundary").len(), 2);
}

// spec: corpus.diagram.node@b0a97cc8
#[test]
fn a_node_bundling_attributes_naming_an_error_or_an_operation_is_a_diagram_finding() {
    let clean = "flowchart LR\n  subgraph ENGINE[\"engine · store\"]\n    W[\"writer\"] -->|\"StorePartialSnapshot · 409\"| R([\"operator\"])\n    W -- \"8 hops, 10000 nodes\" --> C[(catalog)]\n  end\n  C -->|\"listing\"| B[\"bucket &lt;prefix&gt;\"]\n";
    assert!(added(clean, "SpecDiagramNode").is_empty(), "{:?}", added(clean, "SpecDiagramNode"));
    let bundled = "flowchart LR\n  D[\"engine · scheduler\"] -->|\"a\"| B[\"writer<br/>reader\"]\n  B -->|\"b\"| W[the snapshot and every declared sidecar]\n  W -->|\"c\"| E([StorePartialSnapshot])\n  E -->|\"d\"| L[\"lease: holder, epoch\"]\n  L -->|\"e\"| F[fold]\n  F -->|\"f\"| G[\"write the manifest\"]\n";
    let found = added(bundled, "SpecDiagramNode");
    for label in ["engine · scheduler", "writer<br/>reader", "the snapshot and every declared sidecar", "StorePartialSnapshot", "lease: holder, epoch", "fold", "write the manifest"] {
        assert!(names(&found, label), "no finding for `{label}`: {found:?}");
    }
}

// spec: corpus.diagram.shape@9c4aeb73
#[test]
fn a_table_drawn_as_a_box_or_a_question_drawn_as_a_box_is_a_shape_finding() {
    let clean = "flowchart LR\n  A([\"caller\"]) -->|\"rows\"| T[(memory_facts)]\n  T -->|\"entries\"| L[(\"audit log\")]\n  L -->|\"checked\"| Q{\"chain intact?\"}\n  Q -->|\"yes\"| K[(\"object store\")]\n  Q -->|\"no\"| A\n";
    assert!(added(clean, "SpecDiagramShape").is_empty(), "{:?}", added(clean, "SpecDiagramShape"));
    let wrong = "flowchart LR\n  A([\"caller\"]) -->|\"rows\"| T[memory_facts]\n  T -->|\"rows\"| U[\"dead-letter table\"]\n  U -->|\"checked\"| Q[\"chain intact?\"]\n";
    let found = added(wrong, "SpecDiagramShape");
    for label in ["memory_facts", "dead-letter table", "chain intact?"] {
        assert!(names(&found, label), "no finding for `{label}`: {found:?}");
    }
}

// spec: corpus.diagram.edge@2fb7f6fc
#[test]
fn an_unlabelled_two_way_or_bundled_edge_is_an_edge_finding() {
    let clean = "flowchart LR\n  A([\"caller\"]) -->|\"query\"| S[\"server\"]\n  S -- \"snapshot set\" --> Q{\"fresh?\"}\n  Q -->|\"yes\"| T[(catalog)]\n  Q -->|\"no\"| A\n";
    assert!(added(clean, "SpecDiagramEdge").is_empty(), "{:?}", added(clean, "SpecDiagramEdge"));
    let wrong = "flowchart LR\n  A([\"caller\"]) --> S[\"server\"]\n  S <-->|\"sync\"| T[(catalog)]\n  T -->|\"parts · manifest\"| O[(\"object store\")]\n  O -- \"one very long label about everything\" --> A\n";
    let found = added(wrong, "SpecDiagramEdge");
    assert_eq!(found.len(), 4, "{found:?}");
}

// spec: corpus.diagram.decision@45b0c316
#[test]
fn a_decision_without_labelled_exits_or_with_a_long_label_is_a_decision_finding() {
    let clean = "flowchart LR\n  A([\"caller\"]) -->|\"request\"| Q{\"output valid?\"}\n  Q -->|\"yes\"| T[(memory_facts)]\n  Q -->|\"no, attempts left\"| A\n";
    assert!(added(clean, "SpecDiagramDecision").is_empty(), "{:?}", added(clean, "SpecDiagramDecision"));
    let wrong = "flowchart LR\n  A([\"caller\"]) -->|\"request\"| Q{\"is the response valid against the declared output schema?\"}\n  Q -->|\"yes\"| T[(memory_facts)]\n  Q --> A\n  R{\"fresh?\"} -->|\"yes\"| T\n  A -->|\"asks\"| S{\"granted?\"}\n  S -->|\"yes\"| T\n  S -->|\"yes\"| A\n";
    let found = added(wrong, "SpecDiagramDecision");
    for label in ["is the response valid against the declared output schema?", "fresh?", "granted?"] {
        assert!(names(&found, label), "no finding for `{label}`: {found:?}");
    }
}

// spec: corpus.diagram.branch@26a72ee9
#[test]
fn a_box_branching_on_a_condition_is_a_branch_finding() {
    let clean = "flowchart LR\n  A([\"caller\"]) -->|\"request\"| S[\"server\"]\n  S -->|\"rows\"| T[(catalog)]\n  S -->|\"answer\"| A\n";
    assert!(added(clean, "SpecDiagramBranch").is_empty());
    let wrong = "flowchart LR\n  A([\"caller\"]) -->|\"yes\"| S[\"server\"]\n  S -->|\"if granted\"| T[(catalog)]\n  S -->|\"when refused\"| A\n  W[\"writer\"] -->|\"rows\"| T\n  W -->|\"StorePartialSnapshot\"| A\n";
    let found = added(wrong, "SpecDiagramBranch");
    assert!(names(&found, "caller") && names(&found, "server") && names(&found, "writer"), "{found:?}");
}

// spec: corpus.diagram.connected@4a61a54c
#[test]
fn a_node_no_edge_reaches_is_an_orphan() {
    let clean = "flowchart LR\n  A([\"caller\"]) -->|\"request\"| SRV\n  subgraph SRV[\"server\"]\n    Q[\"query face\"]\n  end\n";
    assert!(added(clean, "SpecDiagramOrphan").is_empty());
    let wrong = "flowchart LR\n  A([\"caller\"]) -->|\"request\"| S[\"server\"]\n  L[\"lonely cache\"]\n";
    let found = added(wrong, "SpecDiagramOrphan");
    assert!(names(&found, "lonely cache") && found.len() == 1, "{found:?}");
}

// spec: corpus.diagram.unique-label@ccb3f03f
#[test]
fn two_nodes_sharing_a_label_are_a_duplicate() {
    let wrong = "flowchart LR\n  A([\"caller\"]) -->|\"request\"| S[\"server\"]\n  S -->|\"answer\"| B([\"caller\"])\n";
    let found = added(wrong, "SpecDiagramDuplicate");
    assert!(names(&found, "caller") && found.len() == 1, "{found:?}");
}

// spec: corpus.diagram.layout@7b4e5995
#[test]
fn a_chart_without_direction_too_deep_or_too_large_is_a_layout_finding() {
    let clean = "flowchart LR\n  subgraph X[\"outer\"]\n    subgraph Y[\"inner\"]\n      A[\"writer\"]\n    end\n  end\n  A -->|\"rows\"| T[(catalog)]\n";
    assert!(added(clean, "SpecDiagramLayout").is_empty());
    let deep = "flowchart\n  subgraph X[\"outer\"]\n    direction TB\n    subgraph Y[\"inner\"]\n      subgraph Z[\"core\"]\n        A[\"writer\"]\n      end\n    end\n  end\n  A -->|\"rows\"| T[(catalog)]\n";
    assert_eq!(added(deep, "SpecDiagramLayout").len(), 3, "{:?}", added(deep, "SpecDiagramLayout"));
    let mut big = String::from("flowchart LR\n");
    for i in 0..21 {
        big.push_str(&format!("  N{i}[\"node {i}\"] -->|\"next\"| N{}[\"node {}\"]\n", i + 1, i + 1));
    }
    assert_eq!(added(&big, "SpecDiagramLayout").len(), 1, "{:?}", added(&big, "SpecDiagramLayout"));
}

// spec: corpus.diagram.sequence@4281c187
#[test]
fn a_sequence_with_undeclared_or_too_many_parts_is_a_sequence_finding() {
    let clean = "sequenceDiagram\n  participant A as caller\n  participant B as server\n  A->>B: query\n  alt granted\n    B-->>A: rows\n  end\n";
    assert!(added(clean, "SpecDiagramSequence").is_empty());
    let wrong = "sequenceDiagram\n  participant A as caller\n  participant C as idle\n  A->>B: query\n  A->>A: think\n  A->>A: think\n  A->>A: think\n  loop each\n    alt a\n      opt b\n        B-->>A: rows\n      end\n    end\n  end\n";
    let found = added(wrong, "SpecDiagramSequence");
    assert_eq!(found.len(), 4, "{found:?}");
}

// spec: corpus.diagram.message@8d390cf8
#[test]
fn a_long_message_or_note_is_a_message_finding() {
    let wrong = "sequenceDiagram\n  participant A as caller\n  participant B as server\n  A->>B: query the table and every sidecar it declares at once\n  Note over A,B: one note that runs on well past the twelve words it may hold here\n";
    assert_eq!(added(wrong, "SpecDiagramMessage").len(), 2);
}

// spec: corpus.diagram.state@f80e5688
#[test]
fn a_state_diagram_with_two_starts_a_dead_end_or_a_long_label_is_a_state_finding() {
    let clean = "stateDiagram-v2\n  [*] --> Free\n  Free --> Held : acquire\n  Held --> Free : release\n";
    assert!(added(clean, "SpecDiagramState").is_empty());
    let wrong = "stateDiagram-v2\n  [*] --> Free\n  [*] --> Held\n  Free --> Held : acquire the lease with a fresh epoch now\n  Held --> Stuck : halt\n  Lost --> Free : recover\n";
    let found = added(wrong, "SpecDiagramState");
    assert_eq!(found.len(), 4, "{found:?}");
}
