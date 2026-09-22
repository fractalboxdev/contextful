# Specification readability

How specifications of large engineering systems stay readable to a human while every
normative sentence keeps an address, a test link and a checker behind it — and what that
practice implies for a corpus of 1,391 clause rows across 19 contract files.

## Where the corpus loses a human reader

| Symptom | Measure | Consequence |
| --- | --- | --- |
| No lede per operation | Every `## <operation>` section in `10-store.md` and `30-run.md` opens directly on the clause table; the one-sentence gloss lives in `spec/terms/<contract>.toml` | A reader meets `probe` or `fold` as a bare verb and reconstructs its purpose from the rows |
| No guide layer | Each contract file carries one intro paragraph and one diagram, then tables to the end | The file is pure reference; nothing teaches the model the rules assume |
| Repeated id prefix | `store.lay-out.` appears 137 times in `10-store.md` | The widest column restates the heading above it |
| Rationale in a cell | 14 `because` cells in `10-store.md`, each up to 30 words, in the third column | Reasons render as a cramped column the eye skips |
| Raw pointers | 71 `{{id}}` references render as literal ids | The reader leaves the sentence to learn what it says |
| Isolated rows | One row states one obligation, with no shared subject or sequence | Sentences cannot say "if that fails"; order and causality are implicit |

The grammar serves the checker, `slice` and agents well. The loss is in presentation and
in a missing explanatory layer, not in the clause model.

## Practices

| Practice | Source | Informs | What it offers the corpus |
| --- | --- | --- | --- |
| Rust Reference rule names: dotted id from general to specific, rendered as a small margin link on an ordinary prose paragraph, with a Tests link beneath | <https://doc.rust-lang.org/stable/reference/introduction.html> | `corpus.address`, `corpus.render` | The same `contract.operation.subject` addressing, displayed as margin anchors on sentences instead of a table column |
| Ferrocene Language Specification: every sentence a paragraph with a stable id (`fls_xsdmun5uqy4c`), fixed subchapter parts (Syntax, Legality Rules, Dynamic Semantics, Examples), a generated traceability matrix | <https://rust-lang.github.io/fls/general.html> · <https://github.com/rust-lang/fls/blob/main/src/lexical-elements.rst> · <https://public-docs.ferrocene.dev/main/qualification/traceability-matrix.html> | `corpus.anatomy`, `corpus.state` | Certification-grade traceability with prose as the source; the matrix is a build product nobody reads start to finish |
| SQLite requirements: conversational English with no "shall", requirement spans marked inline and stripped on render, hash-derived `R-` ids, `EVIDENCE-OF:` comments in code and tests | <https://www.sqlite.org/requirements.html> · <https://www.sqlite.org/lang_select.html> · <https://www.sqlite.org/testing.html> | `corpus.render`, `corpus.state` | The readable page is the specification and the requirement index is generated from it — the inverse of rendering the index as the page |
| WHATWG living standards: "This section is non-normative", per-API developer summary boxes, `dfn` autolinks, `<wpt>` test annotations that shade tested sections | <https://html.spec.whatwg.org/multipage/introduction.html> · <https://dom.spec.whatwg.org/#interface-node> · <https://speced.github.io/bikeshed/> | `corpus.anatomy`, `corpus.reference` | Explanation sits beside rules when its status is marked by structure, and the tool refuses definitions inside it |
| RFC 8446 layout: Overview with message flows and no requirements, normative body, compliance list and data structures gathered in appendices | <https://www.rfc-editor.org/rfc/rfc8446.html> · <https://www.rfc-editor.org/rfc/rfc7322.html> | `corpus.anatomy` | One protocol shown three ways — narrative, rules, condensed index — each pointing at the others |
| ecmarkup and Test262: `emu-note` for non-normative text, autolinked abstract operations, a linter over algorithm steps, tests carrying `esid:` pointing at spec anchors | <https://tc39.es/ecmarkup/> · <https://github.com/tc39/test262/blob/main/CONTRIBUTING.md> | `corpus.reference`, `corpus.state` | Tests point at the spec, the spec lists no tests, and cross-references resolve to rendered links |
| WebAssembly and SpecTec: every rule in prose and formal notation side by side, both generated from one DSL | <https://webassembly.github.io/spec/core/valid/conventions.html> · <https://webassembly.org/news/2025-03-27-spectec/> | `corpus.render`, `assurance.prove` | Two views cannot drift when one source generates both |
| Raft Figure 2: the whole algorithm on one page, captioned with section numbers, backed by a TLA+ model; understandability measured in a user study | <https://raft.github.io/raft.pdf> | `corpus.render` | A one-page card per contract: every rule, no reasons, pointers into the narrative |
| TLA+ at AWS: prose design first, refined into an executable model for the parts that need precision | <https://lamport.azurewebsites.net/tla/formal-methods-amazon.pdf> | `assurance.prove` | Prose stays the entry point; precision lives in the model beside it |
| Rust RFC guide-level vs reference-level explanation; PEP "How to Teach This" | <https://raw.githubusercontent.com/rust-lang/rfcs/master/0000-template.md> · <https://peps.python.org/pep-0012/> | `corpus.anatomy` | Teach the model as if it exists, then state the corner cases — two sections, two audiences |
| Diátaxis: tutorial, how-to, reference, explanation; "one hardly reads reference material; one consults it" | <https://diataxis.fr/reference/> | `corpus.render` | The contract files are correct reference; the missing piece is explanation, kept on its own pages rather than mixed into clauses |
| Google AIPs and api-linter: two-page rule documents, lowercase bold keywords, a Rationale section that may not carry guidance, lint rule ids keyed to the AIP number | <https://google.aip.dev/8> · <https://linter.aip.dev/131/request-name-required> | `corpus.rationale` | A document can read as prose and still have a linter keyed to its every rule |
| EARS sentence templates (ubiquitous, state, event, optional, unwanted) | <https://alistairmavin.com/ears/> | `corpus.anatomy` | Constrains the sentence, not the layout — a shape the checker can hold paragraphs to |

## Patterns

**Overview before rules.** RFC 8446, DOM and Rust RFCs all give the reader the model
before the obligations. The corpus's single intro paragraph per file is the smallest
version of this; an operation has none.

**Ids on sentences, not rows.** The Rust Reference, FLS, SQLite and Test262 each give
every normative sentence an address without table layout. Sentences under one heading
share a subject and a sequence, and each still carries its anchor.

**The rendered view is not the source.** SQLite strips markers, Bikeshed generates
indexes and coverage shading, SpecTec generates prose, Ferrocene generates the matrix.
`contextful-spec extract` already produces the full clause store in `spec.lock.json`;
the human view can be rendered from it rather than printed from the Markdown tables.

**Non-normative text licensed by structure.** WHATWG sections, `emu-note`, FLS
Examples and AIP Rationale each mark explanation so the checker skips it and the reader
sees it as explanation. The corpus has this for `#### Scenarios` and `unsettled:` lines,
not for a lede or a guide.

**One-page cards.** Raft Figure 2 and the RFC 8446 appendices condense every rule into a
reference sheet. Per contract, the fragment's errors and bounds already form that sheet.

## Direction for the corpus

Ordered by readability gained per change to corpus law.

1. **Render, don't reformat.** The documentation site renders each operation from
   `spec.lock.json`: the gloss as its lede, each clause as a sentence with its subject
   slug as a margin anchor (full id on hover and copy), the Why as a footnote or
   disclosure, and every `{{id}}` resolved to a link titled by the target's subject.
   Source, checker and `slice` are unchanged.
2. **Operation lede in source.** The gloss moves from the fragment into a one-sentence
   paragraph under each `## <operation>` heading, and the fragment keeps the error and
   bound entries. One home, visible where the rows are.
3. **A guide per contract.** A non-normative page per contract teaches the main flow
   with one worked example and links clause ids instead of restating them, so it
   carries no fact the checker needs to own. `references/` or the site holds it; no
   contract file cites it.
4. **A generated card per contract.** `contextful-spec state` renders refusals as
   trigger → error and bounds as value, unit and basis — the Raft Figure 2 of each
   contract.
5. **Paragraph clauses in source.** Rust Reference style, `[subject]` anchors on prose
   sentences under the operation heading, replacing the table. This changes
   `corpus.anatomy` and the checker's parser, and earns its cost only if steps 1–4
   leave the source itself unreadable to the people who edit it.

## Trade-offs

- **Prose invites ambiguity.** The AWS paper's critique of prose design documents is the
  counterweight; steps 1–4 keep the one-obligation row as the checked unit, and step 5
  holds only if each anchored sentence remains the unit the linter measures.
- **Two views drift unless one is generated.** WebAssembly avoided drift only through
  SpecTec. A hand-written guide that restates clauses breaks one home per fact; it links
  or it does not ship.
- **Semantic ids break on reorganization.** The Rust Reference states its rule names are
  unstable between releases; FLS and SQLite pay with meaningless ids instead. The corpus
  keeps semantic ids and bounds the cost with `corpus.address.ids-are-stable`.
- **Complete traceability does not read.** DO-178C and Ferrocene matrices are complete
  and unreadable by design; the readable layer is always a separate artifact.
