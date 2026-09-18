---
contract: corpus
owns:
  - address
  - anatomy
  - registry
  - reference
  - rationale
  - state
  - render
---

# Corpus law

This corpus is the specification of contextful. It is written before the code and the
code is built against it. This file states the grammar every other file obeys, and
obeys that grammar itself.

## Parties

| Party | Obligation |
| --- | --- |
| **The author** | States a fact once, in the file whose contract owns it, as one clause row carrying a four-coordinate address. Cites a decision record for every refusal whose direction is a choice. |
| **The registry** | Holds the controlled vocabulary. A contract, an operation, a subject, a unit, an error identifier and a named bound exist because `spec/terms/` says so, and nowhere else. |
| **The checker** | `contextful spec` is the one implementation of every rule below. It reads authored text as `spec/terms/scope.toml` defines it, and the gate and a local run invoke the identical command. |
| **The reader** | Reads a contract file for behavior, `spec/decisions/` for why, `spec/status.md` for what the tree demonstrates, and `spec/roadmap.md` for order. |

## Operations

| Operation | What it governs |
| --- | --- |
| `address` | The four coordinates of a clause id and what makes each one legal. |
| `anatomy` | The heading order every contract file carries, and the binding between an operation and the clauses that constrain it. |
| `registry` | Registration of a spelling, and the collision test that refuses a near-synonym. |
| `reference` | The one legal way a file reaches a fact another file owns. |
| `rationale` | Where an argument lives, and what a specification sentence is forbidden to contain. |
| `state` | How performed, committed and broken are computed rather than written. |
| `render` | The generated tree, and the equality that keeps it honest. |

## Clauses — address

| Clause | Statement | decided-by |
| --- | --- | --- |
| `corpus.address.shape.clause-id` | A clause id is four dot-separated segments of `[a-z0-9-]+`, written in the row's first cell: the contract, the operation, the kind, the subject. | `0001` |
| `corpus.address.invariant.contract-segment` | The first segment equals the file's front-matter `contract`, one of the twenty entries in `spec/terms/contract.toml`. A contract entry carries an ordered file list, so one contract spans several files. | `0001` |
| `corpus.address.invariant.operation-segment` | The second segment is an operation registered in `spec/terms/operation.toml` under the key `<contract>.<operation>` and listed in exactly one front-matter `owns` list among its contract's files. The first two segments together decide which file carries the clause, and two contracts needing one verb each register it under their own key. | `0001` |
| `corpus.address.shape.kind-segment` | The third segment is one of `invariant`, `refusal`, `limit`, `shape`, `interface`, `workflow`, read off the sentence's own form. | `0001` |
| `corpus.address.invariant.subject-segment` | The fourth segment is a canonical subject in `spec/terms/term.toml` whose owning contract is this clause's contract. | `0001` |
| `corpus.address.invariant.ids-are-stable` | An id changes when a fact changes obligor and at no other time. Moving an operation between two files of one contract rewrites no id, no reference, no pin, no roadmap row and no record citation. | `0001` |
| `corpus.address.refusal.duplicate-id` | Extraction refuses a repeated clause id, naming both files and both line numbers, and raises `SpecDuplicateId`. | `0001` |
| `corpus.address.refusal.retired-id` | An id listed under `[retired]` in the registry is refused on reuse, raising `SpecRetiredId`. | `0001` |

## Clauses — anatomy

| Clause | Statement | decided-by |
| --- | --- | --- |
| `corpus.anatomy.shape.file-headings` | A contract file carries front matter with `contract` and `owns`, one `# ` title, and top-level headings Parties, Operations, Clauses, Shapes, Unsettled in that order. Clause tables carry the heading `## Clauses — <operation>`. | `0001` |
| `corpus.anatomy.invariant.operation-is-bound` | Every operation a file lists under Operations is addressed by at least one clause, and every obligation a Parties row states is addressed by an invariant or a refusal. | `0001` |
| `corpus.anatomy.refusal.unowned-operation` | An operation claimed by two files of one contract, claimed by none, or addressed by a clause whose file does not own it raises `SpecOwnsConflict`. | `0001` |
| `corpus.anatomy.limit.file-length` | A file whose registry role is `contract` holds at most 900 lines; the checker names the operations whose clauses relieve it. A file whose role is `registry` or `generated` carries no length bound. | `0001` |
| `corpus.anatomy.invariant.title-matches-registry` | A file's single `# ` heading equals the title its contract entry declares for that path. | `0001` |

## Clauses — registry

| Clause | Statement | decided-by |
| --- | --- | --- |
| `corpus.registry.invariant.vocabulary-is-registered` | A backticked token appearing in two or more files carries a `[term]` entry naming its gloss, its owner and whether it resolves in code or names a concept. | `0001` |
| `corpus.registry.refusal.colliding-spelling` | Registration folds case, strips separators, strips a plural, strips a leading `max-`, `min-` or `the-`, and expands SI prefixes and byte multiples, then refuses a proposed contract, operation, term, unit or error spelling that normalizes onto an existing canonical or alias, naming the canonical it collided with, and raises `SpecSpellingCollision`. | `0001` |
| `corpus.registry.refusal.alias-in-a-statement` | A clause statement written in a refused alias spelling raises `SpecAliasUsed`, naming the canonical spelling, the file and the line. | `0001` |
| `corpus.registry.invariant.minting-is-recorded` | A commit adding an operation entry also adds a decision record citing it, and `spec/status.md` prints the operation count per contract. | `0001` |
| `corpus.registry.invariant.one-named-bound-one-owner` | Every numeral-and-unit pair in a limit statement resolves to one `limit-id` in `spec/terms/limit.toml`, and each `limit-id` is asserted by one clause. Two independent bounds carrying the same number are two `limit-id` entries; one shared ceiling is one entry reached by reference. | `0001` |
| `corpus.registry.refusal.limit-without-a-number` | A `limit` row carrying no numeral, a unit absent from `spec/terms/unit.toml`, or a numeral spelled as a word raises `SpecUnmeasuredLimit`. | `0001` |
| `corpus.registry.refusal.refusal-without-an-error` | A `refusal` row naming no identifier present in `spec/terms/error.toml` raises `SpecUnnamedRefusal`. | `0001` |
| `corpus.registry.invariant.scan-boundary-is-data` | Every check reads its globs and its exemptions from `spec/terms/scope.toml`. Authored text is `spec/**/*.md` less the generated tree, the registries, `spec/status.md` and `spec/roadmap.md`, less front matter, less every span a reference produced. | `0001` |

## Clauses — reference

| Clause | Statement | decided-by |
| --- | --- | --- |
| `corpus.reference.interface.transclusion` | `{{<clause id>}}` is the one way a file states a fact another file owns. The renderer inlines the owner's single statement into `spec/build/`, so the second appearance is generated. | `0001` |
| `corpus.reference.interface.term-link` | `[[<term>]]` links a registered term to its gloss and its owning clause. | `0001` |
| `corpus.reference.shape.pointer-sentence` | A pointer sentence names an owning section and the one consequence a local reader needs. It carries no numeral, no error identifier and no normative modal. | `0001` |
| `corpus.reference.refusal.assertion-about-a-foreign-term` | A clause naming a term another contract owns raises `SpecForeignAssertion` on three arms: the subject segment naming a foreign term; the registered term nearest before the statement's modal being foreign; or a foreign term sharing the statement with a numeral-and-unit pair, a registered error identifier, a wire code or a status code this clause does not own. Referenced spans are excluded from all three. | `0001` |
| `corpus.reference.refusal.restated-sentence` | Two authored statements sharing an 8-gram, after lowercasing, punctuation stripping and whitespace collapse, raise `SpecRestatement`, naming both ids. | `0001` |
| `corpus.reference.refusal.dangling-reference` | A `{{id}}` naming no clause and a `[[term]]` naming no registry entry raise `SpecDanglingReference`. | `0001` |

## Clauses — rationale

| Clause | Statement | decided-by |
| --- | --- | --- |
| `corpus.rationale.invariant.argument-lives-in-a-record` | An argument lives under `spec/decisions/`, one record per decision, named `NNNN-<slug>.md`. A contract file states behavior and names no alternative. | `0002` |
| `corpus.rationale.refusal.argument-in-a-contract-file` | The tokens `because`, `so that`, `in order to`, `the reason`, `which is why`, `judged on`, `at the cost of` and `trade-off`, and any term in the criteria vocabulary, raise `SpecRationaleLeak` outside `spec/decisions/`. `rather than` and `instead of` stay legal, so a statement names what a behavior is not. | `0002` |
| `corpus.rationale.refusal.modal-outside-a-clause` | `must`, `never`, `refuses`, `is refused`, `at most`, `at least`, `exactly` and `always` raise `SpecStrayModal` outside a clause cell, an unsettled line and `spec/decisions/`. `only` raises it in a sentence that also carries a registered term. | `0001` |
| `corpus.rationale.invariant.refusal-cites-a-record` | A `refusal` row carries a `decided-by` cell naming a record under `spec/decisions/`. A `limit` row inherits the citation of the clause owning its `limit-id`. | `0002` |
| `corpus.rationale.shape.record-anatomy` | A record carries a dated Status line, Context, Decision, Options considered, Criteria, Consequences. Every rejected option names the criterion it lost on, and the Decision states the outcome as a standing fact. | `0002` |
| `corpus.rationale.refusal.orphan-record` | A record no clause cites and no citing record supersedes raises `SpecOrphanRecord`, as does a `decides` entry naming an id that does not extract, and a record whose `decides` list spans more than two contracts. | `0002` |
| `corpus.rationale.shape.unsettled-line` | An unknown is one inline line in the section it affects: an `unsettled:` marker, a question ending in a question mark, an `owner:` handle, and an `affects:` operation. It carries no modal and no numeral, and `spec/status.md` counts it per file. | `0002` |
| `corpus.rationale.refusal.appendix-heading` | The headings `Open questions`, `Out of scope` and `See also` raise `SpecAppendixHeading`. An unknown lives where it applies. | `0002` |

## Clauses — state

| Clause | Statement | decided-by |
| --- | --- | --- |
| `corpus.state.invariant.status-is-computed` | `spec/status.md` is generated. It is the sole statement of performed, committed and broken per clause, with the pin that produced each verdict, the pinned fraction and its floor per contract and per file, and the operation count per contract. | `0002` |
| `corpus.state.shape.pin` | `spec/pins.toml` maps a clause id to the one artifact demonstrating it: a test function path, a theorem constant, or a type path. | `0002` |
| `corpus.state.invariant.unpinned-is-committed` | A clause with no pin computes `committed`. An empty tree therefore computes a clean corpus with no authorial action. | `0002` |
| `corpus.state.invariant.performed-needs-resolution` | A clause computes `performed` when its pin resolves against the source tree and every backticked identifier it names that is registered as resolving in code is defined in a definition position — an item, a field name, an error variant, a command verb, a schema key, an environment read — never a comment, a fixture or a string literal. A term registered as naming a concept is excluded from that test. | `0002` |
| `corpus.state.refusal.broken-pin` | A pin that does not resolve, and a resolving pin whose clause names a code term the tree no longer defines, compute `broken` and raise `SpecBrokenPin`. A rename is what reds the gate. | `0002` |
| `corpus.state.refusal.presence-pin-on-a-bound` | A `refusal` or `limit` clause accepts a test pin or a theorem pin. A type-path pin on either raises `SpecPresencePinOnBound`. | `0002` |
| `corpus.state.limit.coverage-floor` | `spec/pins.toml` carries a machine-maintained floor of the pinned clause count per contract. A live count below its floor raises `SpecCoverageRegression`, so deleting a pin clears no verdict. The floor rises by an explicit command after a clean run. | `0002` |
| `corpus.state.refusal.status-word-in-a-contract-file` | A build-state word, a date, or a milestone token in authored text raises `SpecDatedProse`. Build state is reached by the roadmap and the generated status file, named by path. | `0002` |

## Clauses — render

| Clause | Statement | decided-by |
| --- | --- | --- |
| `corpus.render.invariant.generated-tree-is-derived` | `spec/build/`, `spec/status.md` and `spec/spec.lock.json` are generated from authored text and the registries. Nothing under them is an input to a duplication check. | `0001` |
| `corpus.render.shape.lock-file` | `spec/spec.lock.json` carries the corpus as data: every clause with its four coordinates, its kind, its authored statement, its citation and its source location; the per-file `owns` map; the flattened registry; the named-bound index; and the pointer graph. | `0001` |
| `corpus.render.refusal.stale-generated-tree` | Regeneration into a temporary directory that differs from the committed tree by one byte raises `SpecStaleRender`. | `0001` |
| `corpus.render.refusal.banned-vocabulary` | The nouns `seam`, `load-bearing`, `wedge`, `rung` and `land-grab`, and `axis` outside a chart caption, raise `SpecBannedNoun`. A bare `#<digits>` reference and a pull-request link raise `SpecProvenanceLeak`. | `0001` |
| `corpus.render.refusal.absolute-local-path` | A path beginning `/Users/`, `/home/`, `$HOME/` or `~/` raises `SpecLocalPath`. | `0001` |
| `corpus.render.refusal.operator-table` | A table whose header carries a sign-off or operations tag marker raises `SpecOperatorTable`. The corpus addresses a reader, not an operator in a session. | `0002` |
| `corpus.render.shape.diagram` | A diagram is a fenced `mermaid` block. Box-drawing characters and pipe-and-dash art outside such a fence raise `SpecAsciiDiagram`. | `0001` |

## Shapes

```
spec/
  00-corpus.md              this file — the grammar
  01-topology.md            the system and its contracts
  1x-*.md                   the store and its wire format
  2x-*.md                   the read face
  3x-*.md                   the run path
  4x-*.md                   authority and its enforcement
  5x-*.md                   operator and visitor surfaces
  6x-*.md                   assurance and engineering
  terms/                    the controlled vocabulary, one file per table
  decisions/                one record per decision
  pins.toml                 clause id -> demonstrating artifact, plus the coverage floor
  roadmap.md                milestone -> clause-id set
  status.md                 generated
  spec.lock.json            generated
  build/                    generated
tools/spec/                 the one implementation of every rule above
```

A clause row reads:

```markdown
| `store.commit.refusal.partial-snapshot` | A reader observes a snapshot and every declared sidecar together or observes neither, and a commit that would expose one without the other raises `StorePartialSnapshot`. | `0012` |
```

An unsettled line reads:

```markdown
unsettled: Does a replica that has never pulled report an empty store or refuse the read? owner: read-path affects: read.serve
```

## Unsettled

unsettled: Does a clause whose pin is a theorem accept a second pin naming the differential test that ties the theorem to the code? owner: corpus affects: corpus.state
