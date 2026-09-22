# Specification readability

How specifications of large engineering systems stay readable to a human while every
normative sentence keeps an address, a test link and a checker behind it. The corpus's
answer — subject-anchored clause items under a lede, a guide and a generated card per
contract, and a site rendering all three — is recorded in `A-corpus`.

## Practices

| Practice | Source | Informs | What the corpus takes from it |
| --- | --- | --- | --- |
| Rust Reference rule names: dotted id from general to specific, rendered as a small margin link on an ordinary prose paragraph, with a Tests link beneath | <https://doc.rust-lang.org/stable/reference/introduction.html> | `corpus.address`, `corpus.anatomy` | The `contract.operation.subject` address sits on the sentence as its opening anchor, and the site renders it in the margin with the clause's pins beside it. |
| Ferrocene Language Specification: every sentence a paragraph with a stable id (`fls_xsdmun5uqy4c`), fixed subchapter parts, a generated traceability matrix | <https://rust-lang.github.io/fls/general.html> · <https://github.com/rust-lang/fls/blob/main/src/lexical-elements.rst> · <https://public-docs.ferrocene.dev/main/qualification/traceability-matrix.html> | `corpus.anatomy`, `corpus.state` | Traceability survives prose as the source; `status.md` and the lock file are the matrix, generated and never read start to finish. |
| SQLite requirements: conversational English with no "shall", requirement spans marked inline and stripped on render, hash-derived `R-` ids, `EVIDENCE-OF:` comments in code and tests | <https://www.sqlite.org/requirements.html> · <https://www.sqlite.org/lang_select.html> · <https://www.sqlite.org/testing.html> | `corpus.render`, `corpus.state` | The readable page is the specification and every index is generated from it; a tag's statement digest plays the role of the hash id without renaming the clause. |
| WHATWG living standards: "This section is non-normative", per-API developer summary boxes, `dfn` autolinks, `<wpt>` test annotations | <https://html.spec.whatwg.org/multipage/introduction.html> · <https://dom.spec.whatwg.org/#interface-node> · <https://speced.github.io/bikeshed/> | `corpus.anatomy`, `corpus.reference` | Non-normative text is licensed by structure — prose after the clause list, the guide — and every `{{id}}` renders as a link. |
| RFC 8446 layout: Overview with message flows and no requirements, normative body, compliance list and data structures gathered in appendices | <https://www.rfc-editor.org/rfc/rfc8446.html> · <https://www.rfc-editor.org/rfc/rfc7322.html> | `corpus.guide`, `corpus.render` | One contract shown three ways: guide, contract file, card, each pointing at the others. |
| ecmarkup and Test262: `emu-note` for non-normative text, autolinked abstract operations, tests carrying `esid:` pointing at spec anchors | <https://tc39.es/ecmarkup/> · <https://github.com/tc39/test262/blob/main/CONTRIBUTING.md> | `corpus.reference`, `corpus.state` | Tests point at the clause through `// spec:` tags; the spec lists no tests. |
| WebAssembly and SpecTec: every rule in prose and formal notation side by side, both generated from one DSL | <https://webassembly.github.io/spec/core/valid/conventions.html> · <https://webassembly.org/news/2025-03-27-spectec/> | `corpus.render` | Two views drift unless one is generated, so the card is rendered from the registry rather than written. |
| Raft Figure 2: the whole algorithm on one page, captioned with section numbers, backed by a TLA+ model | <https://raft.github.io/raft.pdf> | `corpus.render` | The per-contract card: operations, refusals and bounds on one page, each naming its clause. |
| TLA+ at AWS: prose design first, refined into an executable model for the parts that need precision | <https://lamport.azurewebsites.net/tla/formal-methods-amazon.pdf> | `assurance.prove` | Prose stays the entry point; precision lives in the Lean model pinned beside the clause. |
| Rust RFC guide-level vs reference-level explanation; PEP "How to Teach This" | <https://raw.githubusercontent.com/rust-lang/rfcs/master/0000-template.md> · <https://peps.python.org/pep-0012/> | `corpus.guide` | The guide teaches the model as if it exists, then hands off to the contract file for the corner cases. |
| Diátaxis: tutorial, how-to, reference, explanation; "one hardly reads reference material; one consults it" | <https://diataxis.fr/reference/> | `corpus.guide`, `corpus.anatomy` | Contract files stay pure reference, and explanation lives on its own page instead of inside clauses. |
| Google AIPs and api-linter: two-page rule documents, a Rationale section that may not carry guidance, lint rule ids keyed to the AIP number | <https://google.aip.dev/8> · <https://linter.aip.dev/131/request-name-required> | `corpus.rationale` | A document reads as prose and still has a linter keyed to its every rule; rationale sits under the rule, never inside it. |
| EARS sentence templates (ubiquitous, state, event, optional, unwanted) | <https://alistairmavin.com/ears/> | `corpus.anatomy` | A constraint on the sentence rather than the layout, which the list item keeps checkable. |

## Patterns

**Overview before rules.** RFC 8446, DOM and Rust RFCs all give the reader the model
before the obligations. The corpus puts a lede on every operation and a guide on every
contract.

**Ids on sentences, not rows.** The Rust Reference, FLS, SQLite and Test262 each give
every normative sentence an address without table layout. Sentences under one heading
share a subject and a sequence, and each still carries its anchor.

**The rendered view is not the source.** SQLite strips markers, Bikeshed generates
indexes and coverage shading, SpecTec generates prose, Ferrocene generates the matrix.
`contextful-spec` generates the lock file, status and cards, and the site renders clause
items with margin anchors and resolved pointers.

**Non-normative text licensed by structure.** WHATWG sections, `emu-note`, FLS
Examples and AIP Rationale each mark explanation so the checker skips it and the reader
sees it as explanation.

## Trade-offs

- **Prose invites ambiguity.** The AWS paper's critique of prose design documents is the
  counterweight; the one-obligation item stays the unit the checker measures.
- **Two views drift unless one is generated.** WebAssembly avoided drift only through
  SpecTec. A guide that restates a clause is a second home; the checker refuses error
  names and clause items in guides, and a restated number stays a review concern.
- **Semantic ids break on reorganization.** The Rust Reference states its rule names are
  unstable between releases; FLS and SQLite pay with meaningless ids instead. The corpus
  keeps semantic ids and bounds the cost with `corpus.address.ids-are-stable`.
- **Complete traceability does not read.** DO-178C and Ferrocene matrices are complete
  and unreadable by design; the readable layer is always a separate artifact.
