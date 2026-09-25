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
  - diagram
  - targets
  - guide
---

# Corpus law

This corpus is the specification of **Contextful**, written before the code and built
against. This file is the grammar every other file obeys, and it obeys that grammar
itself. `contextful-spec lint` implements every clause below; a local run and the gate
invoke the identical command, and a rule the checker cannot enforce is not a rule.

```
spec/
  00-corpus.md          this file
  01-topology.md        the system, its contracts, its build profiles
  NN-*.md               one or more files per contract, listed in terms/contract.toml
  terms/                contract.toml, unit.toml, refused-names.toml,
                        and one <contract>.toml fragment per contract
  adr/                  P<n>-<slug>.md principles, A-<contract>.md decisions
  guide/                <contract>.md, one teaching page per contract
  cards/                <contract>.md, generated
  pins.toml             clause id -> demonstrating artifact, plus the coverage floor
  roadmap.md            milestone -> operation set
  status.md             generated
  spec.lock.json        generated
references/             literature and practice; points into spec/, never the reverse
tools/spec/             the checker
formal/                 Lean 4 models and theorems, pinned like tests
```

## address

The three segments of a clause id and what makes each legal.

- `clause-id` — A clause id is three dot-separated segments — contract, operation, subject — each matching `[a-z0-9-]+`. A clause item writes the subject alone; its file and section supply the rest.
  *A-corpus*
- `contract-segment` — The first segment is the file's front-matter `contract`, which names an entry of `spec/terms/contract.toml` listing that file.
  *P8*
- `operation-segment` — The second segment is the `## ` section holding the item: an operation registered in the contract's fragment and listed in the front-matter `owns` of exactly one of the contract's files.
  *P8*
- `subject-segment` — The third segment is a slug of at most 40 chars, unique within its operation. It is a name, not a registry entry.
  *P8*
- `ids-are-stable` — An id changes when a fact changes obligor. Moving an operation between two files of one contract rewrites no id.
  *P8*
- `malformed-id` — A clause id violating the shape, or a clause item under a section its file does not own, raises `SpecMalformedId`.
  *P8*
- `duplicate-id` — A clause id appearing in two items raises `SpecDuplicateId`, naming both locations.
  *P8*

## anatomy

The section order of a contract file, the lede and clause list of each operation, and the computed kind of a clause.

- `file-headings` — A contract file carries front matter with `contract` and `owns`, one `# ` title matching its contract entry, one `## <operation>` section per owned operation in `owns` order, then an optional `## Shapes`.
  *P8*
- `lede` — An operation section opens with a lede: one paragraph stating what the operation covers. The lede is the operation's only description; its fragment entry carries no gloss.
  *A-corpus*
- `clause-list` — After the lede, one contiguous list holds the section's clauses, each item `` - `<subject>` — <statement> `` on one line, optionally followed by its Why on a two-space-indented `*…*` line.
  *A-corpus*
- `after-list` — Prose, diagrams, tables, scenarios and unsettled lines follow the clause list and state no obligation.
  *A-corpus*
- `scenario` — A `#### Scenarios` list under an operation holds items `` - `<clause id>`: WHEN <trigger>, THEN <outcome> `` or a backticked `tests/fixtures/` path; the lock file attaches each to its clause.
  *because an example is the test an implementer writes first, and a rule without one leaves the agent to invent it*
- `bad-scenario` — A scenario naming a clause outside its operation, or holding neither a WHEN/THEN sentence nor a fixture path, raises `SpecScenario`.
  *P8*
- `kind-is-computed` — A clause is a refusal when its statement names an error its fragment assigns to it, a limit when its fragment assigns it a named bound, and a behavior otherwise. The checker writes the kind into `spec/spec.lock.json`.
  *P8*
- `statement-words` — A clause statement holds at most 40 words, a `{{id}}` reference counting as one.
  *because one item states one obligation; a longer item is two clauses or carries rationale*
- `file-length` — A contract file holds at most 900 lines.
  *because a file past that splits by adding a path to its contract entry, which renames nothing*
- `bad-anatomy` — A missing, extra or out-of-order `##` section, a missing lede, a broken clause list, a malformed item, a title differing from the registry, a file over its length or an overlong statement raises `SpecAnatomy`.
  *P8*

An operation section, with one clause carrying its Why, and an unsettled line:

```markdown
## fold

Compaction: pass order, triggers, retention, and the pointer commit that makes a snapshot readable.

- `partial-snapshot` — A reader observes a snapshot and every declared sidecar together or neither; a commit exposing one without the other raises `StorePartialSnapshot`.
  *P4*

unsettled: Does a replica that has never pulled report an empty store or refuse the read? owner: read-path affects: read.serve
```

## registry

Where a spelling is registered, the single owner of an error or a bound, and the collision test.

- `fragment` — Each contract owns `spec/terms/<contract>.toml`, holding its operations, and its errors and named bounds each with a gloss.
  *P8*
- `one-error-one-clause` — An error entry names the one clause raising it. That clause's statement names the error, and no other clause statement does; a second site reaches the condition by `{{id}}`.
  *P2*
- `bound-entry` — A named-bound entry carries its owning clause, a value, a unit from `spec/terms/unit.toml` and a basis: `chosen`, `measured:<benchmark>` or `standard:<name>`. The owning statement carries the value followed by the unit.
  *because an unmeasured number presented as a measurement is a hypothesis stated as fact*
- `unregistered` — An unregistered error after `raises`, an entry naming a missing clause, an error absent from its clause or present in another, and a malformed bound raise `SpecRegistry`.
  *P8*

## reference

The one legal way a statement reaches a fact another clause owns, and the ban on restatement and literature.

- `pointer` — `{{<clause id>}}` is the one way a statement reaches a fact another clause owns. The lock file records every pointer as an edge.
  *P8*
- `dangling` — A `{{id}}` naming no clause raises `SpecDanglingReference`.
  *P8*
- `no-literature` — A spec file carries no hyperlink to an external document and no path under `references/`; either raises `SpecExternalLink`. Literature lives in `references/`, which cites clause ids.
  *because the contract is judged by behavior, and a citation invites arguing from authority*

## rationale

Where an argument lives: the Why line, the record and its anatomy, and the words a contract file cannot carry.

- `why-cell` — A clause's Why is absent, one or more record ids such as `P3` or `A-store`, or `because` followed by at most 30 words stating the deciding criterion. A refusal carries a Why.
  *P8*
- `record-threshold` — A decision earns a section in its contract's ADR when two or more clauses depend on it, it spans contracts, or it names a revisit trigger; otherwise it lives in a `because` cell.
  *P8*
- `record-anatomy` — A principle `P<n>-<slug>.md` carries `# <id> — <title>`, a `**Status:**` line, then Context (optional), Decision, Options, Consequences, Revisit (optional), in at most 400 words.
  *because a principle argues one rule every contract obeys, and the length of the argument is part of its quality*
- `contract-adr` — A contract's decisions live in one `A-<contract>.md`: `# A-<contract> — <title>`, a `**Status:**` line, then one `## <decision>` section per decision, each holding its options table in at most 250 words.
  *because one file per contract puts every decision beside the clauses it governs, and a fresh corpus has no history to keep apart*
- `options-table` — A principle's Options and each ADR section hold one table headed `Option`, `Lost on`, `Cost` with two to five rows. One row is marked *(chosen)* with `—` as Lost on; every other row names the criterion it lost on.
  *P8*
- `unsettled-line` — An unknown is one line where it applies: `unsettled:`, a question ending `?`, `owner:` and a handle, `affects:` and a `<contract>.<operation>`. Another form, or a heading `Open questions`, `Out of scope` or `See also`, raises `SpecUnsettled`.
  *P8*
- `orphan-record` — A record no Why cites, a Why naming no record, and a record breaking its anatomy raise `SpecRecord`.
  *P8*

## state

Pins, verdicts, the coverage floor, the roadmap's operation claims and each milestone's acceptance test.

- `status-is-computed` — `spec/status.md` is the sole statement of which clauses the tree demonstrates. No authored file says whether something is built.
  *P8*
- `pin` — `spec/pins.toml` maps a clause id to one artifact: a `test` function path, a `theorem` constant under `formal/` or an `item` path. A refusal or a limit takes a test or a theorem.
  *P8*
- `verdict` — An unpinned clause computes `committed`; a pinned clause computes `performed` when its artifact's final path segment is defined under `crates/`, `tools/` or `formal/`, and `broken` otherwise. A test carrying an `#[ignore]` attribute computes `broken`.
  *P8*
- `tag-pin` — A line `// spec: <id>@<rev>` among the comments and attributes above a test function under `crates/` or `tools/` pins clause `<id>` to that test; `<rev>` is the first 8 hex digits of the statement's SHA-256.
  *because a pin written beside its test travels with every move and rename, and the digest records which wording the test demonstrates*
- `lean-tag` — A line `-- spec: <id>@<rev>` above a Lean `theorem` or `lemma` under `formal/`, past comments, docstrings and attributes, pins clause `<id>` to that theorem, with `<rev>` as in {{corpus.state.tag-pin}}.
  *because a theorem's pin then moves with the proof, and a reworded clause marks the proof stale*
- `theorem-beside-test` — A clause carries at most one theorem pin and one test pin, and computes `performed` when both perform: the theorem proves the model, the test ties the model to the code.
  *A-assurance*
- `stale-pin` — A tag whose `<rev>` differs from the digest of its clause's current statement raises `SpecStalePin`, and its clause computes `broken`.
  *because a reworded clause no longer says what its test was written to demonstrate*
- `unfinished-test` — A pinned test holding a body line that opens with `todo!`, and a pinned Lean declaration holding `sorry` or `admit`, compute `broken`.
  *because a placeholder demonstrates nothing, whether it panics at run time or proves by assumption*
- `scaffold` — `contextful-spec scaffold <contract>.<operation> --package <path>` writes one tagged test per refusal and limit clause of the operation into `<path>/tests/integration/`, its body a `todo!` naming the error or bound, and rewrites no existing function.
- `scaffold-lean` — `contextful-spec scaffold <contract>.<operation> --lean <file>` appends one tagged theorem per clause of the operation, its statement the docstring and its proposition and proof `sorry`, and rewrites no existing theorem.
- `coverage-floor` — `spec/pins.toml` carries a per-contract floor of pinned clauses. A live count below its floor raises `SpecCoverageRegression`.
  *because deleting a failing pin must not read as progress*
- `bad-pin` — A pin naming no clause, a tag above no declaration, an item pin on a refusal or limit, a broken pin, and one clause pinned to two tests or two theorems raise `SpecBrokenPin`.
  *P8*
- `roadmap` — `spec/roadmap.md` names operations as `<contract>.<operation>` or `<contract>.*`. A name resolving to no operation, an operation claimed by two milestones, or a milestone lacking its `Reach:` or `Acceptance:` line raises `SpecRoadmap`.
  *P8*
- `acceptance` — A milestone's `Acceptance:` line names one test under `crates/acceptance/`; that test computes `absent` when undefined, `open` when ignored, and `passing` otherwise.
  *A-assurance*
- `acceptance-first` — A milestone holding a pinned clause while its acceptance test computes `absent` raises `SpecAcceptanceMissing`.
  *A-assurance*
- `deferred-depth` — A milestone carrying a `Depth: operation` line admits only refusal and limit clauses; a behavior clause of an operation it schedules raises `SpecDeferredBehavior`.
  *because an unopened milestone fixes what it refuses and bounds, and its behavior is written against the code that opens it*

## render

The generated files, and the vocabulary no authored file carries.

- `generated` — `spec/status.md`, `spec/targets.md`, `spec/cards/` and `spec/spec.lock.json` are regenerated by `contextful-spec state` and `contextful-spec extract`. A committed copy differing from regeneration raises `SpecStaleRender`.
  *P8*
- `lock-file` — The lock file carries every clause with its segments, computed kind, statement, Why and location, each operation's lede, the per-file `owns` map, the registry and the pointer graph.
  *P8*
- `card` — `contextful-spec state` renders `spec/cards/<contract>.md` per contract: each operation with its lede and clause count, each refusal as error and gloss, each bound as value, unit and basis, all naming their clause.
  *A-corpus*
- `dated-prose` — The words `planned`, `not yet`, `currently`, `today`, `shipped`, `implemented`, `previously`, `used to`, `legacy`, `TODO`, `FIXME` and `WIP`, and an ISO date, raise `SpecDatedProse` in any authored file.
  *because build state is computed, and a dated sentence is stale the day it lands*
- `counterfactual` — The words `will`, `would` and `shall` raise `SpecCounterfactual` in a contract file or a guide.
  *because a contract states present behavior, and a record's Options is where a path not taken is described*
- `banned-vocabulary` — The nouns `seam`, `load-bearing`, `wedge`, `rung`, `land-grab` and an unbackticked `axiom`, a word whose digest `spec/terms/refused-names.toml` lists, a bare `#<digits>` and a pull-request link raise `SpecBannedWord`.
  *P8*
- `local-path` — A path beginning `/Users/`, `/home/`, `$HOME/` or `~/` raises `SpecLocalPath`.
  *P8*
## diagram

The fenced diagram, and the shape every flowchart, sequence and state diagram keeps so a reader sees the parts and what flows between them.

- `fence` — A diagram is a fenced `mermaid` block; box-drawing characters outside a fence raise `SpecAsciiDiagram`.
  *P8*
- `boundary` — A diagram draws a contract, party, process or trust zone as a container — a flowchart `subgraph` or a sequence `box` — holding its components. A flowchart node naming a contract or a boundary raises `SpecDiagramBoundary`.
  *P8*
- `node` — A flowchart node names one thing in at most 5 words: its label holds no `·`, `<br/>`, `,`, `;`, `: ` or error identifier, is no operation name, and sets no article second. A breach raises `SpecDiagramNode`.
  *because a box of bundled attributes or a verb hides the things and what flows between them; operations, limits and errors ride the edges*
- `shape` — A node labelled with a snake_case identifier or a phrase ending `table`, `store`, `log` or `queue` is a cylinder `[( )]`, and a label ending `?` is a decision `{ }`. A breach raises `SpecDiagramShape`.
  *because one shape meaning one kind lets a reader tell data from the parts acting on it at a glance*
- `edge` — A flowchart edge points one way; its label holds at most 5 words and no `·`, and an edge touching no decision node carries one. A breach raises `SpecDiagramEdge`.
  *because an unlabelled line says two things relate without saying how, and a two-way line hides which side acts*
- `decision` — A decision node asks a question or names a condition in at most 8 words and no error identifier, has an incoming edge, and has two or more outgoing edges each labelled with a distinct outcome. A breach raises `SpecDiagramDecision`.
  *because a branch without a guard on each exit leaves the reader to guess which path a case takes*
- `branch` — A non-decision node with an outgoing edge labelled `yes` or `no`, an outgoing error identifier beside another exit, or two exits opening `if`, `when`, `over`, `under` or `else` raises `SpecDiagramBranch`; a branch is a decision node.
  *because a condition hidden in edge labels off a box reads as parallel flow*
- `connected` — Every flowchart node, or a subgraph enclosing it, is an edge endpoint. A breach raises `SpecDiagramOrphan`.
  *because an unconnected element asserts nothing about the system*
- `unique-label` — No two nodes of one flowchart carry the same label; a second mention reuses the node id. A breach raises `SpecDiagramDuplicate`.
  *because one label naming two nodes leaves the reader to decide whether they are one thing*
- `layout` — A flowchart header declares `LR`, `TB` or `TD`, no subgraph declares `direction`, subgraphs nest at most 2 levels, and the chart holds at most 20 nodes and 25 edges. A breach raises `SpecDiagramLayout`.
  *because one reading direction keeps the flow followable, and a chart past that size or depth is two diagrams*
- `sequence` — A sequence diagram declares each participant it messages and messages each it declares, and holds at most 6 participants, 15 messages, 2 messages from a participant to itself and fragments nested 2 levels. A breach raises `SpecDiagramSequence`.
  *because a sequence diagram shows one scenario's interactions, and branching logic past that belongs in a flowchart*
- `message` — A sequence message label holds at most 8 words and a note at most 12 words. A breach raises `SpecDiagramMessage`.
  *because a message names one call; its parameters and bounds belong in clauses*
- `state` — A state diagram has one top-level `[*]` start reaching every state, every state has an outgoing transition, `[*]` counting as a target, and a transition label holds at most 6 words. A breach raises `SpecDiagramState`.
  *because an unreachable or dead-end state is a lifecycle the system cannot run*

## targets

Per-provider deployment target files, the roles each shape fills, and the page rendered from them.

- `file` — Each deployment provider's target profile is `spec/targets/<provider>.toml`: named shapes, exactly one marked default, each with a kind, the profiles it hosts, an optional per-invocation wall-clock cap, its exclusions and the primitive filling each role.
  *A-topology*
- `roles` — A shape fills eight roles — cron tick, reconciler, durable orchestrator, single-writer catalog, object store, hot-path query face, heavy compute and secrets — and may fill a realtime run projection.
- `incomplete` — A shape leaving a role unfilled, or a target file marking other than one default shape, raises `SpecTargetIncomplete`, naming the provider, the shape and the role.
  *because an unfilled role surfaces only when a deploy reaches it, on someone else's account*
- `cap-unrecorded` — A shape whose cap sits at or below the {{topology.deploy.wall-clock-cap}} bound and whose exclusions omit `first-time-backfill` or `component-connector` raises `SpecTargetCapUnrecorded`.
  *because the exclusion is the difference between a refused dispatch and a job killed mid-run*
- `function-profile` — A function-class shape listing a profile other than `edge` raises `SpecTargetFunctionProfile`, naming the provider and the shape.
  *because {{topology.package.edge-eligibility}} admits the edge profile alone on a function-class target*
- `page` — `contextful-spec state` renders `spec/targets.md` from the target files: per provider, a role table across its shapes and a diagram of its default shape.
  *because a diagram drawn by hand beside the data it shows drifts from it*

## guide

A non-normative teaching page per contract, and what keeps it from becoming a second home for a rule.

- `file` — Each contract has one guide, `spec/guide/<contract>.md`: front matter `contract`, one `# ` title equal to the contract's registry title, then prose of at most 700 words outside diagrams.
  *A-corpus*
- `non-normative` — A guide holds no clause item and names no registered error; it reaches a rule by `{{id}}`. No contract file links a guide.
  *because a rule restated in teaching prose is a second home that drifts from the first*
- `bad-guide` — A contract without a guide, a guide breaking its shape or length, a guide holding a clause item or an error name, and a contract file linking a guide raise `SpecGuide`.
  *P8*
